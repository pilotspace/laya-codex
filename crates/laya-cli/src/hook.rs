//! Claude Code hook handlers. Every handler is fail-open: any error or timeout yields no output.
//!
//! Events handled (schemas verified empirically, see docs/build-context.md):
//! - `UserPromptSubmit`: inject the ranked spans as `additionalContext`.
//! - `PreToolUse` `Read`: the first whole-file Read of a large indexed file shows the daemon's
//!   planned region plus a file outline; a second whole-file Read passes through (escape hatch).
//! - `PreToolUse` `Agent|Task`: hand the parent's working set to the subagent prompt.
//! - `PostToolUse` edits: re-index the edited file.
//! - `SessionStart`: `compact` → re-inject the working set; `startup`/`resume` → background re-index.

use std::path::{Path, PathBuf};

use laya_core::{QueryResult, RankMode};
use serde_json::{Value, json};

use crate::config::rel_path;
use crate::protocol::{RenderReq, Request, Response, SessionView};

/// Transport to the daemon (a unix-socket client in production, a fake in tests).
pub trait DaemonApi {
    fn call(&self, req: Request) -> anyhow::Result<Response>;
}

pub struct HookCtx<'a> {
    pub api: &'a dyn DaemonApi,
    pub root: PathBuf,
    pub budget_ms: u64,
    pub inject_tokens: usize,
    /// Formerly the minimum Laya P for a ranked span to justify narrowing a Read. Unused since
    /// the daemon plans Reads (`Request::ReadPlan`: any ranked span, else task terms, always with
    /// an outline and a full-read escape hatch); kept so `LAYA_READ_P` configs still parse.
    #[allow(dead_code)]
    pub read_p: f32,
    /// Inject the compact format (ranked map + top spans) instead of every span's full code.
    pub compact: bool,
    /// Append the "Related by references" section (one-hop callers/callees of the top spans).
    pub related: bool,
    /// Let the daemon size the injection (scope + calibrated P) and skip spans already sent.
    pub adaptive: bool,
}

/// What a handler did, for the optional JSONL hook log.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    pub output: Option<Value>,
    pub action: &'static str,
    pub injected_chars: usize,
}

impl Outcome {
    fn skip(action: &'static str) -> Self {
        Outcome {
            output: None,
            action,
            injected_chars: 0,
        }
    }
}

pub fn handle(input: &Value, ctx: &HookCtx) -> Outcome {
    let event = input["hook_event_name"].as_str().unwrap_or_default();
    let session = input["session_id"].as_str().unwrap_or_default();
    match event {
        "UserPromptSubmit" => {
            user_prompt(input["prompt"].as_str().unwrap_or_default(), session, ctx)
        }
        "PreToolUse" => match input["tool_name"].as_str().unwrap_or_default() {
            "Read" => pre_read(&input["tool_input"], session, ctx),
            "Agent" | "Task" => pre_agent(&input["tool_input"], session, ctx),
            _ => Outcome::skip("ignored_tool"),
        },
        "PostToolUse" => post_edit(&input["tool_input"], ctx),
        "SessionStart" => session_start(input["source"].as_str().unwrap_or_default(), session, ctx),
        _ => Outcome::skip("ignored_event"),
    }
}

/// Prompts that never need code retrieval: slash commands, `#` memory lines, trivially short text.
pub fn skip_prompt(prompt: &str) -> bool {
    let p = prompt.trim();
    p.len() < 3 || p.starts_with('/') || p.starts_with('#') || laya_core::ident::terms(p).len() < 2
}

fn user_prompt(prompt: &str, session: &str, ctx: &HookCtx) -> Outcome {
    if skip_prompt(prompt) {
        return Outcome::skip("skip_prompt");
    }
    let req = Request::Query {
        repo: ctx.root.to_string_lossy().into_owned(),
        session: Some(session.to_string()),
        prompt: prompt.to_string(),
        budget_ms: Some(ctx.budget_ms),
        top_n: None,
        render: ctx.compact.then_some(RenderReq {
            budget_tokens: ctx.inject_tokens,
            related: ctx.related,
            adaptive: ctx.adaptive,
        }),
    };
    let (result, rendered) = match ctx.api.call(req) {
        Ok(Response::Query {
            result, rendered, ..
        }) => (result, rendered),
        _ => return Outcome::skip("query_failed"),
    };
    if result.spans.is_empty() {
        return Outcome::skip("no_spans");
    }
    let text = if let Some(text) = rendered {
        if text.is_empty() {
            return Outcome::skip("already_in_context");
        }
        text
    } else if ctx.compact {
        laya_rank::render_compact_opts(&result, 3, ctx.inject_tokens, ctx.related)
    } else {
        laya_rank::render_context_opts(&result, ctx.inject_tokens, ctx.related)
    };
    Outcome {
        injected_chars: text.len(),
        output: Some(
            json!({"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": text}}),
        ),
        action: "inject",
    }
}

fn session_view(session: &str, reset: bool, ctx: &HookCtx) -> Option<SessionView> {
    match ctx.api.call(Request::Session {
        session: session.to_string(),
        reset,
    }) {
        Ok(Response::Session { view }) => Some(view),
        _ => None,
    }
}

fn pre_read(tool_input: &Value, session: &str, ctx: &HookCtx) -> Outcome {
    let Some(file) = tool_input["file_path"].as_str() else {
        return Outcome::skip("no_path");
    };
    let Some(rel) = rel_path(&ctx.root, file) else {
        return Outcome::skip("outside_repo");
    };
    let ranged = !tool_input["offset"].is_null() || !tool_input["limit"].is_null();
    let count = match ctx.api.call(Request::NoteRead {
        session: session.to_string(),
        path: rel.clone(),
        full: !ranged,
    }) {
        Ok(Response::Count { count }) => count,
        _ => return Outcome::skip("daemon_unavailable"),
    };
    if ranged || count > 1 {
        return Outcome::skip(if ranged {
            "already_ranged"
        } else {
            "escape_hatch"
        });
    }
    let Some(total) = count_lines(&ctx.root.join(&rel)) else {
        return Outcome::skip("unreadable");
    };
    if total < laya_rank::ReadPolicy::default().min_file_lines {
        return Outcome::skip("small_file");
    }
    let plan = match ctx.api.call(Request::ReadPlan {
        repo: ctx.root.to_string_lossy().into_owned(),
        session: session.to_string(),
        path: rel.clone(),
    }) {
        Ok(Response::ReadPlan { plan: Some(p) }) => p,
        Ok(Response::ReadPlan { plan: None }) => return Outcome::skip("not_narrowed"),
        _ => return Outcome::skip("daemon_unavailable"),
    };
    // The daemon read the file too; if it saw another length or planned outside it, the file
    // changed in between (or the plan is bad): never hide lines on a plan for other bytes.
    let end = plan.offset.saturating_add(plan.limit).saturating_sub(1);
    if plan.total_lines != total || plan.offset == 0 || plan.limit == 0 || end > total {
        return Outcome::skip("stale_plan");
    }
    let mut updated = tool_input.clone();
    updated["offset"] = json!(plan.offset);
    updated["limit"] = json!(plan.limit);
    let note = format!(
        "[laya-codex] {rel} has {total} lines. Showing lines {}-{end} of {total} (best match for the task). \
         Read again with offset/limit for any other part, or Read the whole file again to get all of it.\n\
         Outline (start-end item; * = shown):\n{}",
        plan.offset, plan.outline
    );
    Outcome {
        injected_chars: note.len(),
        output: Some(
            json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow",
            "updatedInput": updated, "additionalContext": note}}),
        ),
        action: if plan.basis == "ranking" {
            "narrow_read"
        } else {
            "outline_read"
        },
    }
}

fn pre_agent(tool_input: &Value, session: &str, ctx: &HookCtx) -> Outcome {
    let Some(prompt) = tool_input["prompt"].as_str() else {
        return Outcome::skip("no_prompt");
    };
    let Some(view) = session_view(session, false, ctx) else {
        return Outcome::skip("daemon_unavailable");
    };
    if view.working_set.is_empty() {
        return Outcome::skip("empty_working_set");
    }
    let mut block = String::from(
        "\n\n[laya-codex] Code the parent agent already located (read these ranges first):\n",
    );
    for s in view.working_set.iter().take(5) {
        block.push_str(&format!(
            "- {}:{}-{} {}\n",
            s.path, s.start_line, s.end_line, s.symbol
        ));
    }
    let mut updated = tool_input.clone();
    updated["prompt"] = json!(format!("{prompt}{block}"));
    Outcome {
        injected_chars: block.len(),
        output: Some(
            json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow",
            "updatedInput": updated}}),
        ),
        action: "agent_handoff",
    }
}

fn post_edit(tool_input: &Value, ctx: &HookCtx) -> Outcome {
    let Some(file) = tool_input["file_path"]
        .as_str()
        .or(tool_input["notebook_path"].as_str())
    else {
        return Outcome::skip("no_path");
    };
    let req = Request::ReindexFile {
        repo: ctx.root.to_string_lossy().into_owned(),
        path: file.to_string(),
    };
    match ctx.api.call(req) {
        Ok(Response::Ok) => Outcome::skip("reindexed"),
        _ => Outcome::skip("reindex_failed"),
    }
}

fn session_start(source: &str, session: &str, ctx: &HookCtx) -> Outcome {
    if source == "clear" {
        let _ = session_view(session, true, ctx);
        return Outcome::skip("context_reset");
    }
    if source == "compact" {
        // Compaction dropped earlier injections and reads from the agent's context.
        let Some(view) = session_view(session, true, ctx) else {
            return Outcome::skip("daemon_unavailable");
        };
        if view.working_set.is_empty() {
            return Outcome::skip("empty_working_set");
        }
        let result = QueryResult {
            spans: view.working_set.into_iter().take(8).collect(),
            mode: RankMode::Laya,
            elapsed_ms: 0,
            candidates: 0,
            related: vec![],
        };
        let text = laya_rank::render_context(&result, ctx.inject_tokens * 2 / 3);
        return Outcome {
            injected_chars: text.len(),
            output: Some(
                json!({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": text}}),
            ),
            action: "reinject_after_compact",
        };
    }
    let _ = ctx.api.call(Request::IndexRepo {
        repo: ctx.root.to_string_lossy().into_owned(),
    });
    Outcome::skip("index_started")
}

fn count_lines(path: &Path) -> Option<u32> {
    Some(line_count(&std::fs::read(path).ok()?))
}

/// Lines as the Read tool numbers them (`\n`-terminated, so CRLF counts once; a final line
/// without a newline counts). Saturates at `u32::MAX`.
pub(crate) fn line_count(bytes: &[u8]) -> u32 {
    let n = bytes.iter().filter(|&&b| b == b'\n').count()
        + usize::from(!bytes.is_empty() && !bytes.ends_with(b"\n"));
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ReadPlan;
    use laya_core::RankedSpan;
    use std::cell::RefCell;

    struct Fake {
        calls: RefCell<Vec<Request>>,
        result: Option<QueryResult>,
        read_count: u32,
        rendered: Option<String>,
        plan: Option<ReadPlan>,
        /// `ReadPlan` fails (an older daemon answers `bad request`).
        plan_down: bool,
        /// Every call fails (no daemon).
        down: bool,
    }

    impl DaemonApi for Fake {
        fn call(&self, req: Request) -> anyhow::Result<Response> {
            self.calls.borrow_mut().push(req.clone());
            if self.down {
                anyhow::bail!("down");
            }
            Ok(match req {
                Request::ReadPlan { .. } if self.plan_down => Response::Error {
                    message: "bad request".into(),
                },
                Request::ReadPlan { .. } => Response::ReadPlan {
                    plan: self.plan.clone(),
                },
                Request::Query { render, .. } => match &self.result {
                    Some(r) => Response::Query {
                        result: r.clone(),
                        rendered: render.and_then(|_| self.rendered.clone()),
                        scope: Some("file".into()),
                    },
                    None => anyhow::bail!("down"),
                },
                Request::NoteRead { .. } => Response::Count {
                    count: self.read_count,
                },
                Request::Session { .. } => Response::Session {
                    view: SessionView {
                        last: self.result.clone(),
                        working_set: self.result.clone().map(|r| r.spans).unwrap_or_default(),
                    },
                },
                _ => Response::Ok,
            })
        }
    }

    fn root() -> PathBuf {
        crate::config::repo_root(Path::new(env!("CARGO_MANIFEST_DIR")))
    }

    fn span(path: &str, a: u32, b: u32, p: f32) -> RankedSpan {
        RankedSpan {
            path: path.into(),
            start_line: a,
            end_line: b,
            symbol: "fn x".into(),
            p_relevant: Some(p),
            score: p,
            text: "fn x() {}".into(),
        }
    }

    fn fake(result: Option<QueryResult>, read_count: u32) -> Fake {
        Fake {
            calls: RefCell::new(vec![]),
            result,
            read_count,
            rendered: None,
            plan: None,
            plan_down: false,
            down: false,
        }
    }

    fn ctx<'a>(api: &'a dyn DaemonApi) -> HookCtx<'a> {
        HookCtx {
            api,
            root: root(),
            budget_ms: 500,
            inject_tokens: 4000,
            read_p: 0.7,
            compact: false,
            related: true,
            adaptive: false,
        }
    }

    fn res(spans: Vec<RankedSpan>) -> QueryResult {
        QueryResult {
            spans,
            mode: RankMode::Laya,
            elapsed_ms: 5,
            candidates: 10,
            related: vec![],
        }
    }

    #[test]
    fn skip_rules() {
        assert!(skip_prompt("/clear"));
        assert!(skip_prompt("# remember this"));
        assert!(skip_prompt("ok"));
        assert!(skip_prompt("thanks!"));
        assert!(!skip_prompt("fix the WAL replay bug in recover_shard"));
    }

    #[test]
    fn prompt_injects_context_and_fails_open() {
        let f = fake(Some(res(vec![span("src/a.rs", 1, 20, 0.9)])), 1);
        let input = json!({"hook_event_name": "UserPromptSubmit", "session_id": "s", "prompt": "where is the WAL replay implemented"});
        let o = handle(&input, &ctx(&f));
        assert_eq!(o.action, "inject");
        let text = o.output.unwrap()["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(text.contains("src/a.rs"));
        let down = fake(None, 1);
        assert_eq!(handle(&input, &ctx(&down)).output, None);
    }

    fn read_input(path: &str) -> Value {
        json!({"hook_event_name": "PreToolUse", "session_id": "s", "tool_name": "Read",
            "tool_input": {"file_path": root().join(path).to_string_lossy()}})
    }

    fn plan_fake(read_count: u32) -> Fake {
        let mut f = fake(None, read_count);
        f.plan = Some(ReadPlan {
            offset: 100,
            limit: 60,
            total_lines: 400,
            basis: "lexical".into(),
            outline: "  1-99 fn head\n* 100-159 fn replay_wal\n  160-400 fn tail".into(),
        });
        f
    }

    fn plan_calls(f: &Fake) -> usize {
        f.calls
            .borrow()
            .iter()
            .filter(|r| matches!(r, Request::ReadPlan { .. }))
            .count()
    }

    #[test]
    fn first_full_read_of_a_big_file_shows_the_planned_region_and_the_outline() {
        let big = big_file();
        let f = plan_fake(1);
        let o = handle(&read_input(big), &ctx(&f));
        assert_eq!(o.action, "outline_read");
        let out = o.output.unwrap();
        let h = &out["hookSpecificOutput"];
        assert_eq!(h["permissionDecision"], "allow");
        let upd = &h["updatedInput"];
        assert!(upd["file_path"].as_str().unwrap().ends_with(big));
        assert_eq!(
            (upd["offset"].as_u64(), upd["limit"].as_u64()),
            (Some(100), Some(60))
        );
        let note = h["additionalContext"].as_str().unwrap();
        assert!(note.contains("Showing lines 100-159 of 400"), "{note}");
        assert!(note.contains("Read the whole file again"), "{note}");
        assert!(note.contains("* 100-159 fn replay_wal"), "{note}");
        assert_eq!(o.injected_chars, note.len());
        assert!(matches!(
            &f.calls.borrow()[1],
            Request::ReadPlan { path, session, .. } if path == big && session == "s"
        ));
        // A plan from the session's ranking is logged as before.
        let mut g = plan_fake(1);
        g.plan.as_mut().unwrap().basis = "ranking".into();
        assert_eq!(handle(&read_input(big), &ctx(&g)).action, "narrow_read");
    }

    #[test]
    fn second_full_read_is_the_escape_hatch() {
        let f = plan_fake(2);
        let o = handle(&read_input(big_file()), &ctx(&f));
        assert_eq!((o.action, o.output), ("escape_hatch", None));
        assert_eq!(plan_calls(&f), 0);
    }

    #[test]
    fn small_files_pass_through_without_asking_the_daemon() {
        let rel = "target/laya-hook-test-small.rs";
        std::fs::create_dir_all(root().join("target")).unwrap();
        std::fs::write(root().join(rel), "// small\n".repeat(249)).unwrap();
        let f = plan_fake(1);
        let o = handle(&read_input(rel), &ctx(&f));
        assert_eq!((o.action, o.output), ("small_file", None));
        assert_eq!(plan_calls(&f), 0);
    }

    #[test]
    fn reads_pass_through_when_nothing_is_planned_or_the_daemon_fails() {
        let big = big_file();
        let none = fake(None, 1);
        let o = handle(&read_input(big), &ctx(&none));
        assert_eq!((o.action, o.output), ("not_narrowed", None));
        let mut old = plan_fake(1);
        old.plan_down = true;
        let o = handle(&read_input(big), &ctx(&old));
        assert_eq!((o.action, o.output), ("daemon_unavailable", None));
        let mut down = plan_fake(1);
        down.down = true;
        let o = handle(&read_input(big), &ctx(&down));
        assert_eq!((o.action, o.output), ("daemon_unavailable", None));
    }

    #[test]
    fn plans_that_disagree_with_the_file_pass_through() {
        let big = big_file();
        let mut changed = plan_fake(1);
        changed.plan.as_mut().unwrap().total_lines = 401;
        let o = handle(&read_input(big), &ctx(&changed));
        assert_eq!((o.action, o.output), ("stale_plan", None));
        for (offset, limit) in [(0, 10), (10, 0), (390, 20)] {
            let mut bad = plan_fake(1);
            let p = bad.plan.as_mut().unwrap();
            (p.offset, p.limit) = (offset, limit);
            let o = handle(&read_input(big), &ctx(&bad));
            assert_eq!(
                (o.action, o.output),
                ("stale_plan", None),
                "{offset}+{limit}"
            );
        }
    }

    /// A 400-line file inside the repo (under target/, which is gitignored).
    fn big_file() -> &'static str {
        let rel = "target/laya-hook-test-big.rs";
        let body: String = (0..400).map(|i| format!("// line {i}\n")).collect();
        std::fs::create_dir_all(root().join("target")).unwrap();
        // Tests run in parallel: write aside and rename (atomic) so no reader sees a partial file.
        let tmp = root().join(format!("{rel}.{:?}", std::thread::current().id()));
        std::fs::write(&tmp, body).unwrap();
        std::fs::rename(&tmp, root().join(rel)).unwrap();
        rel
    }

    #[test]
    fn ranged_reads_pass_through() {
        let big = big_file();
        let r = res(vec![span(big, 100, 140, 0.95)]);
        let input = json!({"hook_event_name": "PreToolUse", "session_id": "s", "tool_name": "Read",
            "tool_input": {"file_path": root().join(big).to_string_lossy(), "offset": 1, "limit": 10}});
        assert_eq!(
            handle(&input, &ctx(&fake(Some(r), 1))).action,
            "already_ranged"
        );
    }

    #[test]
    fn agent_prompt_gets_working_set() {
        let f = fake(Some(res(vec![span("src/a.rs", 1, 20, 0.9)])), 1);
        let input = json!({"hook_event_name": "PreToolUse", "session_id": "s", "tool_name": "Agent",
            "tool_input": {"description": "d", "prompt": "investigate", "subagent_type": "Explore"}});
        let o = handle(&input, &ctx(&f));
        let upd = &o.output.unwrap()["hookSpecificOutput"]["updatedInput"];
        assert_eq!(upd["subagent_type"], "Explore");
        assert!(upd["prompt"].as_str().unwrap().contains("src/a.rs:1-20"));
    }

    #[test]
    fn adaptive_prompt_uses_daemon_render_and_skips_when_nothing_new() {
        let mut f = fake(Some(res(vec![span("src/a.rs", 1, 20, 0.8)])), 0);
        f.rendered = Some("SIZED CONTEXT".into());
        let c = HookCtx {
            compact: true,
            adaptive: true,
            ..ctx(&f)
        };
        let o = handle(
            &json!({"hook_event_name": "UserPromptSubmit", "session_id": "s", "prompt": "where is wal replay done"}),
            &c,
        );
        assert_eq!(o.action, "inject");
        assert_eq!(
            o.output.unwrap()["hookSpecificOutput"]["additionalContext"],
            "SIZED CONTEXT"
        );
        assert!(f.calls.borrow().iter().any(|r| matches!(
            r,
            Request::Query {
                render: Some(RenderReq { adaptive: true, .. }),
                ..
            }
        )));
        let mut g = fake(Some(res(vec![span("src/a.rs", 1, 20, 0.8)])), 0);
        g.rendered = Some(String::new());
        let c = HookCtx {
            compact: true,
            adaptive: true,
            ..ctx(&g)
        };
        let o = handle(
            &json!({"hook_event_name": "UserPromptSubmit", "session_id": "s", "prompt": "where is wal replay done"}),
            &c,
        );
        assert_eq!((o.action, o.output), ("already_in_context", None));
    }

    #[test]
    fn compact_or_clear_resets_the_sessions_context() {
        let f = fake(Some(res(vec![span("src/a.rs", 1, 20, 0.8)])), 0);
        for source in ["compact", "clear"] {
            handle(
                &json!({"hook_event_name": "SessionStart", "session_id": "s", "source": source}),
                &ctx(&f),
            );
            assert!(
                matches!(
                    f.calls.borrow().last(),
                    Some(Request::Session { reset: true, .. })
                ),
                "{source}"
            );
        }
    }

    #[test]
    fn compact_reinjects_and_startup_indexes() {
        let f = fake(Some(res(vec![span("src/a.rs", 1, 20, 0.9)])), 1);
        let c = handle(
            &json!({"hook_event_name": "SessionStart", "session_id": "s", "source": "compact"}),
            &ctx(&f),
        );
        assert_eq!(c.action, "reinject_after_compact");
        let s = handle(
            &json!({"hook_event_name": "SessionStart", "session_id": "s", "source": "startup"}),
            &ctx(&f),
        );
        assert_eq!(s.output, None);
        assert!(
            f.calls
                .borrow()
                .iter()
                .any(|r| matches!(r, Request::IndexRepo { .. }))
        );
    }

    #[test]
    fn post_edit_reindexes() {
        let f = fake(None, 1);
        let o = handle(
            &json!({"hook_event_name": "PostToolUse", "session_id": "s", "tool_name": "Edit",
            "tool_input": {"file_path": "/x/y.rs"}}),
            &ctx(&f),
        );
        assert_eq!(o.output, None);
        assert!(matches!(f.calls.borrow()[0], Request::ReindexFile { .. }));
    }
}
