//! Minimal MCP server over stdio (newline-delimited JSON-RPC 2.0) exposing `search`.
//! Kept dependency-free: the protocol surface we need is initialize, tools/list, tools/call, ping.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::hook::DaemonApi;
use crate::protocol::{Request, Response};
use crate::trace::{self, Recording, Tracer};

const DEFAULT_PROTOCOL: &str = "2025-06-18";

/// Server instructions, which Claude Code adds to the system prompt. Benchmark v3 (v9): with code
/// already injected, agents still grepped ~2.9 times a session, mostly for callers and tests of
/// that code, and called this server's search 0.02 times.
const INSTRUCTIONS: &str = "laya-codex has indexed this repository. To find where something is \
implemented, what calls or uses it, or which tests cover it, call the laya-codex `search` tool \
before Grep or Glob: one call returns the relevant code with file paths and line numbers. Keep \
Grep for exhaustive exact-text matches.";

pub fn tool_list() -> Value {
    json!({"tools": [{
        "name": "search",
        "description": "Find code in this repository by meaning or by name: where something is implemented, \
    its callers and uses, and the tests that cover it. Prefer this over Grep and Glob for those questions: it \
    returns the code itself (ranked 10-50 line spans with file paths and line numbers) plus definition and use \
    lines, so you usually need no Read or further search afterwards. Use Grep only for exact text that must be \
    matched everywhere (every occurrence of a string) or for non-code files.",
        "inputSchema": {"type": "object", "properties": {
            "query": {"type": "string", "description": "What you are looking for, in natural language and/or identifiers"},
            "top_n": {"type": "integer", "description": "Number of spans (default 10, max 20)"}
        }, "required": ["query"]}
    }]})
}

pub fn handle_message(
    msg: &Value,
    api: &dyn DaemonApi,
    root: &std::path::Path,
    inject_tokens: usize,
    budget_ms: u64,
) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg["method"].as_str().unwrap_or_default();
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or(DEFAULT_PROTOCOL),
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "laya-codex", "version": env!("CARGO_PKG_VERSION")},
            "instructions": INSTRUCTIONS
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tool_list()),
        "tools/call" => Ok(call_tool(
            &msg["params"],
            api,
            root,
            inject_tokens,
            budget_ms,
        )),
        _ if id.is_none() => return None, // notifications (e.g. notifications/initialized)
        _ => Err(json!({"code": -32601, "message": format!("method not found: {method}")})),
    };
    let id = id?;
    Some(match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
    })
}

fn call_tool(
    params: &Value,
    api: &dyn DaemonApi,
    root: &std::path::Path,
    inject_tokens: usize,
    budget_ms: u64,
) -> Value {
    let text_result = |text: String, is_error: bool| json!({"content": [{"type": "text", "text": text}], "isError": is_error});
    if params["name"] != "search" {
        return text_result(format!("unknown tool {}", params["name"]), true);
    }
    let query = params["arguments"]["query"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    if query.trim().is_empty() {
        return text_result("query must not be empty".into(), true);
    }
    let top_n = params["arguments"]["top_n"]
        .as_u64()
        .map(|n| n.clamp(1, crate::protocol::MAX_TOP_N as u64) as usize);
    let req = Request::Query {
        repo: root.to_string_lossy().into_owned(),
        session: None,
        prompt: query,
        budget_ms: Some(budget_ms),
        top_n,
        render: None,
    };
    match api.call(req) {
        Ok(Response::Query { result, .. }) if !result.spans.is_empty() => {
            text_result(laya_rank::render_context(&result, inject_tokens), false)
        }
        Ok(Response::Query { .. }) => text_result(
            "no matching code found; fall back to Grep/Glob".into(),
            false,
        ),
        Ok(other) => text_result(format!("unexpected daemon response: {other:?}"), true),
        Err(e) => text_result(
            format!("laya-codex daemon unavailable ({e}); fall back to Grep/Glob"),
            true,
        ),
    }
}

pub fn serve(
    api: &dyn DaemonApi,
    root: PathBuf,
    inject_tokens: usize,
    budget_ms: u64,
    tracer: Option<&Tracer>,
) -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    let recording = Recording::new(api);
    let api: &dyn DaemonApi = if tracer.is_some() { &recording } else { api };
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let t0 = std::time::Instant::now();
        let msg = serde_json::from_str::<Value>(&line);
        let reply = match &msg {
            Ok(msg) => handle_message(msg, api, &root, inject_tokens, budget_ms),
            Err(e) => Some(
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}}),
            ),
        };
        if let Some(t) = tracer {
            let request = msg.unwrap_or_else(|_| json!({"unparsed": line}));
            t.record(&trace::mcp_entry(
                &request,
                reply.as_ref(),
                t0.elapsed().as_millis() as u64,
                recording.take(),
            ));
        }
        if let Some(r) = reply {
            writeln!(out, "{r}").map_err(crate::sys::stdout_err)?;
            out.flush().map_err(crate::sys::stdout_err)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use laya_core::{QueryResult, RankMode, RankedSpan};

    struct Fake(bool);
    impl DaemonApi for Fake {
        fn call(&self, _req: Request) -> anyhow::Result<Response> {
            if !self.0 {
                anyhow::bail!("down")
            }
            Ok(Response::Query {
                result: QueryResult {
                    spans: vec![RankedSpan {
                        path: "src/a.rs".into(),
                        start_line: 3,
                        end_line: 9,
                        symbol: "fn a".into(),
                        p_relevant: Some(0.8),
                        score: 0.8,
                        text: "fn a() {}".into(),
                    }],
                    mode: RankMode::Laya,
                    elapsed_ms: 3,
                    candidates: 5,
                    scored: 0,
                    offered: 0,
                    related: vec![],
                },
                rendered: None,
                scope: None,
            })
        }
    }

    fn call(msg: Value, up: bool) -> Option<Value> {
        handle_message(&msg, &Fake(up), &PathBuf::from("/r"), 3000, 500)
    }

    #[test]
    fn initialize_echoes_protocol_and_lists_tool() {
        let r = call(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}}), true).unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        let t = call(
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            true,
        )
        .unwrap();
        assert_eq!(t["result"]["tools"][0]["name"], "search");
        assert_eq!(
            call(
                json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                true
            ),
            None
        );
    }

    #[test]
    fn search_returns_spans_or_graceful_error() {
        let msg = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "search", "arguments": {"query": "wal replay"}}});
        let ok = call(msg.clone(), true).unwrap();
        assert!(
            ok["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("src/a.rs")
        );
        let down = call(msg, false).unwrap();
        assert_eq!(down["result"]["isError"], true);
    }

    #[test]
    fn search_description_steers_code_lookups_away_from_grep() {
        let t = call(
            json!({"jsonrpc": "2.0", "id": 5, "method": "tools/list"}),
            true,
        )
        .unwrap();
        let d = t["result"]["tools"][0]["description"].as_str().unwrap();
        // v9: agents still made ~2.9 Grep calls a session and 0.02 laya-codex searches, mostly
        // to find callers and tests of code they had been given.
        for needle in [
            "Prefer this over Grep",
            "callers",
            "tests",
            "returns the code",
        ] {
            assert!(d.contains(needle), "description lacks {needle:?}: {d}");
        }
        assert!(d.contains("exact"), "Grep keeps exact-text matches: {d}");
    }

    #[test]
    fn initialize_tells_claude_to_search_before_grep() {
        let r = call(
            json!({"jsonrpc": "2.0", "id": 6, "method": "initialize", "params": {}}),
            true,
        )
        .unwrap();
        let i = r["result"]["instructions"].as_str().unwrap_or_default();
        assert!(i.contains("before Grep"), "instructions: {i:?}");
        assert!(i.contains("`search`"), "instructions name the tool: {i:?}");
    }

    #[test]
    fn unknown_method_is_jsonrpc_error() {
        let r = call(
            json!({"jsonrpc": "2.0", "id": 4, "method": "resources/list"}),
            true,
        )
        .unwrap();
        assert_eq!(r["error"]["code"], -32601);
    }
}
