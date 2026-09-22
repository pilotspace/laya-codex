//! Claude Code hook handlers. Every handler is fail-open: any error or timeout yields no output.
//!
//! Events handled (schemas verified empirically, see docs/build-context.md):
//! - `UserPromptSubmit`: inject the ranked spans as `additionalContext`.
//! - `PreToolUse` `Read`: guarded narrowing to the ranked range (first unranged Read of a large file).
//! - `PreToolUse` `Agent|Task`: hand the parent's working set to the subagent prompt.
//! - `PostToolUse` edits: re-index the edited file.
//! - `SessionStart`: `compact` → re-inject the working set; `startup`/`resume` → background re-index.

use std::path::{Path, PathBuf};

use laya_core::{QueryResult, RankMode};
use serde_json::{Value, json};

use crate::config::rel_path;
use crate::protocol::{Request, Response, SessionView};

/// Transport to the daemon (a unix-socket client in production, a fake in tests).
pub trait DaemonApi {
    fn call(&self, req: Request) -> anyhow::Result<Response>;
}

pub struct HookCtx<'a> {
    pub api: &'a dyn DaemonApi,
    pub root: PathBuf,
    pub repo: String,
    pub budget_ms: u64,
    pub inject_tokens: usize,
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
        Outcome { output: None, action, injected_chars: 0 }
    }
}

pub const HEADER_MARK: &str = "laya-codex: pre-ranked";

pub fn handle(input: &Value, ctx: &HookCtx) -> Outcome {
    let event = input["hook_event_name"].as_str().unwrap_or_default();
    let session = input["session_id"].as_str().unwrap_or_default();
    match event {
        "UserPromptSubmit" => user_prompt(input["prompt"].as_str().unwrap_or_default(), session, ctx),
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
    };
    let result = match ctx.api.call(req) {
        Ok(Response::Query { result }) => result,
        _ => return Outcome::skip("query_failed"),
    };
    if result.spans.is_empty() {
        return Outcome::skip("no_spans");
    }
    let text = laya_rank::render_context(&result, ctx.inject_tokens);
    Outcome {
        injected_chars: text.len(),
        output: Some(json!({"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": text}})),
        action: "inject",
    }
}

fn session_view(session: &str, ctx: &HookCtx) -> Option<SessionView> {
    match ctx.api.call(Request::Session { session: session.to_string() }) {
        Ok(Response::Session { view }) => Some(view),
        _ => None,
    }
}

fn pre_read(tool_input: &Value, session: &str, ctx: &HookCtx) -> Outcome {
    let Some(file) = tool_input["file_path"].as_str() else { return Outcome::skip("no_path") };
    let Some(rel) = rel_path(&ctx.root, file) else { return Outcome::skip("outside_repo") };
    let count = match ctx.api.call(Request::NoteRead { session: session.to_string(), path: rel.clone() }) {
        Ok(Response::Count { count }) => count,
        _ => return Outcome::skip("daemon_unavailable"),
    };
    let ranged = !tool_input["offset"].is_null() || !tool_input["limit"].is_null();
    if ranged || count > 1 {
        return Outcome::skip(if ranged { "already_ranged" } else { "escape_hatch" });
    }
    let Some(last) = session_view(session, ctx).and_then(|v| v.last) else { return Outcome::skip("no_ranking") };
    let Some(total) = count_lines(&ctx.root.join(&rel)) else { return Outcome::skip("unreadable") };
    let Some((offset, limit)) = laya_rank::read_narrowing(&last, &rel, total, &laya_rank::ReadPolicy::default()) else {
        return Outcome::skip("not_narrowed");
    };
    let mut updated = tool_input.clone();
    updated["offset"] = json!(offset);
    updated["limit"] = json!(limit);
    let note = format!(
        "laya-codex narrowed this Read of {rel} ({total} lines) to lines {offset}-{} — the ranked relevant region. \
         Read the same file again (no offset/limit) if you need all of it.",
        offset + limit - 1
    );
    Outcome {
        injected_chars: note.len(),
        output: Some(json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow",
            "updatedInput": updated, "additionalContext": note}})),
        action: "narrow_read",
    }
}

fn pre_agent(tool_input: &Value, session: &str, ctx: &HookCtx) -> Outcome {
    let Some(prompt) = tool_input["prompt"].as_str() else { return Outcome::skip("no_prompt") };
    let Some(view) = session_view(session, ctx) else { return Outcome::skip("daemon_unavailable") };
    if view.working_set.is_empty() {
        return Outcome::skip("empty_working_set");
    }
    let mut block = String::from("\n\n[laya-codex] Code the parent agent already located (read these ranges first):\n");
    for s in view.working_set.iter().take(5) {
        block.push_str(&format!("- {}:{}-{} {}\n", s.path, s.start_line, s.end_line, s.symbol));
    }
    let mut updated = tool_input.clone();
    updated["prompt"] = json!(format!("{prompt}{block}"));
    Outcome {
        injected_chars: block.len(),
        output: Some(json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow",
            "updatedInput": updated}})),
        action: "agent_handoff",
    }
}

fn post_edit(tool_input: &Value, ctx: &HookCtx) -> Outcome {
    let Some(file) = tool_input["file_path"].as_str().or(tool_input["notebook_path"].as_str()) else {
        return Outcome::skip("no_path");
    };
    let req = Request::ReindexFile { repo: ctx.root.to_string_lossy().into_owned(), path: file.to_string() };
    match ctx.api.call(req) {
        Ok(Response::Ok) => Outcome::skip("reindexed"),
        _ => Outcome::skip("reindex_failed"),
    }
}

fn session_start(source: &str, session: &str, ctx: &HookCtx) -> Outcome {
    if source == "compact" {
        let Some(view) = session_view(session, ctx) else { return Outcome::skip("daemon_unavailable") };
        if view.working_set.is_empty() {
            return Outcome::skip("empty_working_set");
        }
        let result = QueryResult { spans: view.working_set.into_iter().take(8).collect(), mode: RankMode::Laya, elapsed_ms: 0, candidates: 0 };
        let text = laya_rank::render_context(&result, ctx.inject_tokens * 2 / 3);
        return Outcome {
            injected_chars: text.len(),
            output: Some(json!({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": text}})),
            action: "reinject_after_compact",
        };
    }
    let _ = ctx.api.call(Request::IndexRepo { repo: ctx.root.to_string_lossy().into_owned() });
    Outcome::skip("index_started")
}

fn count_lines(path: &Path) -> Option<u32> {
    let bytes = std::fs::read(path).ok()?;
    let n = bytes.iter().filter(|&&b| b == b'\n').count() + usize::from(!bytes.is_empty() && !bytes.ends_with(b"\n"));
    u32::try_from(n).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use laya_core::RankedSpan;
    use std::cell::RefCell;

    struct Fake {
        calls: RefCell<Vec<Request>>,
        result: Option<QueryResult>,
        read_count: u32,
    }

    impl DaemonApi for Fake {
        fn call(&self, req: Request) -> anyhow::Result<Response> {
            self.calls.borrow_mut().push(req.clone());
            Ok(match req {
                Request::Query { .. } => match &self.result {
                    Some(r) => Response::Query { result: r.clone() },
                    None => anyhow::bail!("down"),
                },
                Request::NoteRead { .. } => Response::Count { count: self.read_count },
                Request::Session { .. } => Response::Session {
                    view: SessionView { last: self.result.clone(), working_set: self.result.clone().map(|r| r.spans).unwrap_or_default() },
                },
                _ => Response::Ok,
            })
        }
    }

    fn root() -> PathBuf {
        crate::config::repo_root(Path::new(env!("CARGO_MANIFEST_DIR")))
    }

    fn span(path: &str, a: u32, b: u32, p: f32) -> RankedSpan {
        RankedSpan { path: path.into(), start_line: a, end_line: b, symbol: "fn x".into(), p_relevant: Some(p), score: p, text: "fn x() {}".into() }
    }

    fn fake(result: Option<QueryResult>, read_count: u32) -> Fake {
        Fake { calls: RefCell::new(vec![]), result, read_count }
    }

    fn ctx<'a>(api: &'a dyn DaemonApi) -> HookCtx<'a> {
        HookCtx { api, root: root(), repo: "r".into(), budget_ms: 500, inject_tokens: 4000 }
    }

    fn res(spans: Vec<RankedSpan>) -> QueryResult {
        QueryResult { spans, mode: RankMode::Laya, elapsed_ms: 5, candidates: 10 }
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
        let text = o.output.unwrap()["hookSpecificOutput"]["additionalContext"].as_str().unwrap().to_string();
        assert!(text.contains("src/a.rs"));
        let down = fake(None, 1);
        assert_eq!(handle(&input, &ctx(&down)).output, None);
    }

    #[test]
    fn read_is_narrowed_once_then_escape_hatch() {
        let big = big_file();
        let r = res(vec![span(big, 100, 140, 0.95)]);
        let input = json!({"hook_event_name": "PreToolUse", "session_id": "s", "tool_name": "Read",
            "tool_input": {"file_path": root().join(big).to_string_lossy()}});
        let first = handle(&input, &ctx(&fake(Some(r.clone()), 1)));
        assert_eq!(first.action, "narrow_read");
        let out = first.output.unwrap();
        let upd = &out["hookSpecificOutput"]["updatedInput"];
        assert!(upd["file_path"].as_str().unwrap().ends_with(big));
        assert!(upd["offset"].as_u64().unwrap() <= 100 && upd["limit"].as_u64().unwrap() >= 41);
        let second = handle(&input, &ctx(&fake(Some(r), 2)));
        assert_eq!(second.action, "escape_hatch");
        assert_eq!(second.output, None);
    }

    /// A 400-line file inside the repo (under target/, which is gitignored).
    fn big_file() -> &'static str {
        let rel = "target/laya-hook-test-big.rs";
        let body: String = (0..400).map(|i| format!("// line {i}\n")).collect();
        std::fs::create_dir_all(root().join("target")).unwrap();
        std::fs::write(root().join(rel), body).unwrap();
        rel
    }

    #[test]
    fn ranged_reads_pass_through() {
        let big = big_file();
        let r = res(vec![span(big, 100, 140, 0.95)]);
        let input = json!({"hook_event_name": "PreToolUse", "session_id": "s", "tool_name": "Read",
            "tool_input": {"file_path": root().join(big).to_string_lossy(), "offset": 1, "limit": 10}});
        assert_eq!(handle(&input, &ctx(&fake(Some(r), 1))).action, "already_ranged");
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
    fn compact_reinjects_and_startup_indexes() {
        let f = fake(Some(res(vec![span("src/a.rs", 1, 20, 0.9)])), 1);
        let c = handle(&json!({"hook_event_name": "SessionStart", "session_id": "s", "source": "compact"}), &ctx(&f));
        assert_eq!(c.action, "reinject_after_compact");
        let s = handle(&json!({"hook_event_name": "SessionStart", "session_id": "s", "source": "startup"}), &ctx(&f));
        assert_eq!(s.output, None);
        assert!(f.calls.borrow().iter().any(|r| matches!(r, Request::IndexRepo { .. })));
    }

    #[test]
    fn post_edit_reindexes() {
        let f = fake(None, 1);
        let o = handle(&json!({"hook_event_name": "PostToolUse", "session_id": "s", "tool_name": "Edit",
            "tool_input": {"file_path": "/x/y.rs"}}), &ctx(&f));
        assert_eq!(o.output, None);
        assert!(matches!(f.calls.borrow()[0], Request::ReindexFile { .. }));
    }
}
