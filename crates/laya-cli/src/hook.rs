//! Claude Code hook handlers. Every handler is fail-open: any error or timeout yields no output.
//!
//! Events handled (schemas verified empirically, see docs/build-context.md):
//! - `UserPromptSubmit`: inject the ranked spans as `additionalContext`.
//! - `PreToolUse` `Read`: record the Read and output nothing, so later prompts do not inject code
//!   from a file Claude already read whole.
//! - `PreToolUse` `Agent|Task`: hand the parent's working set to the subagent prompt.
//! - `PostToolUse` edits: re-index the edited file.
//! - `SessionStart`: `compact` → re-inject the working set; `startup`/`resume` → background re-index.

use std::path::PathBuf;

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
    /// Inject the compact format (ranked map + top spans) instead of every span's full code.
    pub compact: bool,
    /// Append the "Related by references" section (one-hop callers/callees of the top spans).
    pub related: bool,
    /// Let the daemon size the injection and skip spans already sent.
    pub adaptive: bool,
}

/// What a handler did, for the optional JSONL hook log.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    pub output: Option<Value>,
    pub action: &'static str,
    pub injected_chars: usize,
    /// How the prompt's query was ranked, when one ran.
    pub rank: Option<Rank>,
}

/// How a query was ranked, so benchmarks can attribute effects to the model: `laya` (every
/// candidate the model was given was scored), `laya-partial` (the budget stopped it early) or
/// `lexical` (keyword order only; `offered > 0` means the model timed out rather than being off).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Rank {
    pub mode: &'static str,
    pub scored: usize,
    pub offered: usize,
    pub candidates: usize,
}

impl Rank {
    pub fn of(result: &QueryResult) -> Self {
        let mode = match result.mode {
            RankMode::Lexical => "lexical",
            RankMode::Laya if result.scored < result.offered => "laya-partial",
            RankMode::Laya => "laya",
        };
        Rank {
            mode,
            scored: result.scored,
            offered: result.offered,
            candidates: result.candidates,
        }
    }
}

impl Outcome {
    fn skip(action: &'static str) -> Self {
        Outcome {
            action,
            ..Outcome::default()
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
    let rank = Some(Rank::of(&result));
    if result.spans.is_empty() {
        return Outcome {
            rank,
            ..Outcome::skip("no_spans")
        };
    }
    let text = if let Some(text) = rendered {
        if text.is_empty() {
            return Outcome {
                rank,
                ..Outcome::skip("already_in_context")
            };
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
        rank,
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

/// Record the Read and output nothing: the Read runs exactly as Claude asked. A whole-file Read
/// (no offset/limit) puts the file in Claude's context, so the daemon leaves it out of later
/// injections.
fn pre_read(tool_input: &Value, session: &str, ctx: &HookCtx) -> Outcome {
    let Some(file) = tool_input["file_path"].as_str() else {
        return Outcome::skip("no_path");
    };
    let Some(rel) = rel_path(&ctx.root, file) else {
        return Outcome::skip("outside_repo");
    };
    let ranged = !tool_input["offset"].is_null() || !tool_input["limit"].is_null();
    match ctx.api.call(Request::NoteRead {
        session: session.to_string(),
        path: rel,
        full: !ranged,
    }) {
        Ok(Response::Count { .. }) if ranged => Outcome::skip("note_ranged_read"),
        Ok(Response::Count { .. }) => Outcome::skip("note_read"),
        _ => Outcome::skip("daemon_unavailable"),
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
        rank: None,
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
            scored: 0,
            offered: 0,
            related: vec![],
        };
        let text = laya_rank::render_context(&result, ctx.inject_tokens * 2 / 3);
        return Outcome {
            injected_chars: text.len(),
            output: Some(
                json!({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": text}}),
            ),
            action: "reinject_after_compact",
            rank: None,
        };
    }
    // Only git repositories are indexed automatically: with laya-codex enabled everywhere (a user-scope
    // plugin), Claude Code opened in a home directory or /tmp must not index it wholesale.
    // `laya-codex index <dir>` still indexes any directory on request.
    if !ctx.root.join(".git").exists() {
        return Outcome::skip("not_a_repository");
    }
    let _ = ctx.api.call(Request::IndexRepo {
        repo: ctx.root.to_string_lossy().into_owned(),
    });
    Outcome::skip("index_started")
}

#[cfg(test)]
mod tests {
    use super::*;
    use laya_core::RankedSpan;
    use std::cell::RefCell;
    use std::path::Path;

    struct Fake {
        calls: RefCell<Vec<Request>>,
        result: Option<QueryResult>,
        read_count: u32,
        rendered: Option<String>,
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
            down: false,
        }
    }

    fn ctx<'a>(api: &'a dyn DaemonApi) -> HookCtx<'a> {
        HookCtx {
            api,
            root: root(),
            budget_ms: 500,
            inject_tokens: 4000,
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
            scored: 10,
            offered: 10,
            related: vec![],
        }
    }

    #[test]
    fn prompt_outcome_records_how_the_query_was_ranked() {
        let prompt = json!({"hook_event_name": "UserPromptSubmit", "session_id": "s", "prompt": "where is wal replay done"});
        let cases = [
            (RankMode::Laya, 10, "laya"),
            (RankMode::Laya, 4, "laya-partial"),
            (RankMode::Lexical, 0, "lexical"),
        ];
        for (mode, scored, label) in cases {
            let f = fake(
                Some(QueryResult {
                    mode,
                    scored,
                    offered: 10,
                    ..res(vec![span("src/a.rs", 1, 20, 0.8)])
                }),
                0,
            );
            let o = handle(&prompt, &ctx(&f));
            assert_eq!(o.action, "inject");
            assert_eq!(
                o.rank,
                Some(Rank {
                    mode: label,
                    scored,
                    offered: 10,
                    candidates: 10
                })
            );
        }
        // Nothing new to send still ran a ranked query: the log keeps its mode.
        let mut g = fake(Some(res(vec![span("src/a.rs", 1, 20, 0.8)])), 0);
        g.rendered = Some(String::new());
        let c = HookCtx {
            compact: true,
            adaptive: true,
            ..ctx(&g)
        };
        let o = handle(&prompt, &c);
        assert_eq!(o.action, "already_in_context");
        assert_eq!(o.rank.map(|r| r.mode), Some("laya"));
        // No daemon, no rank.
        let mut d = fake(None, 0);
        d.down = true;
        assert_eq!(handle(&prompt, &ctx(&d)).rank, None);
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
    fn a_read_is_recorded_and_passes_through_untouched() {
        // A 400-line indexed-size file: the kind of Read the hook once narrowed.
        let big = big_file();
        let path = root().join(big).to_string_lossy().into_owned();
        let ranged = json!({"hook_event_name": "PreToolUse", "session_id": "s", "tool_name": "Read",
            "tool_input": {"file_path": path, "offset": 1, "limit": 10}});
        for (input, whole, action) in [
            (read_input(big), true, "note_read"),
            (ranged, false, "note_ranged_read"),
        ] {
            for count in [1, 2] {
                let f = fake(None, count);
                let o = handle(&input, &ctx(&f));
                assert_eq!((o.action, o.output, o.injected_chars), (action, None, 0));
                let calls = f.calls.borrow();
                assert_eq!(calls.len(), 1, "only the NoteRead: {calls:?}");
                assert!(
                    matches!(&calls[0], Request::NoteRead { session, path, full }
                        if session == "s" && path == big && *full == whole),
                    "{calls:?}"
                );
            }
        }
    }

    #[test]
    fn a_read_outside_the_repo_or_without_a_daemon_outputs_nothing() {
        let outside = json!({"hook_event_name": "PreToolUse", "session_id": "s", "tool_name": "Read",
            "tool_input": {"file_path": "/etc/hosts"}});
        let f = fake(None, 1);
        assert_eq!(
            (handle(&outside, &ctx(&f)).action, f.calls.borrow().len()),
            ("outside_repo", 0)
        );
        let mut down = fake(None, 1);
        down.down = true;
        let o = handle(&read_input(big_file()), &ctx(&down));
        assert_eq!((o.action, o.output), ("daemon_unavailable", None));
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
    fn startup_only_auto_indexes_git_repositories() {
        // A plugin enables laya-codex in every folder Claude Code opens; a folder that is not a git
        // repository (a home directory, Downloads, /tmp) must never be indexed wholesale.
        let plain = std::env::temp_dir().join(format!("laya-hook-nogit-{}", std::process::id()));
        std::fs::create_dir_all(&plain).unwrap();
        let f = fake(None, 0);
        let c = HookCtx {
            root: plain.clone(),
            ..ctx(&f)
        };
        let s = handle(
            &json!({"hook_event_name": "SessionStart", "session_id": "s", "source": "startup"}),
            &c,
        );
        assert_eq!((s.action, s.output), ("not_a_repository", None));
        assert!(f.calls.borrow().is_empty(), "nothing may reach the daemon");
        let _ = std::fs::remove_dir_all(&plain);
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
