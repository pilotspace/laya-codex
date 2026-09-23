//! Minimal MCP server over stdio (newline-delimited JSON-RPC 2.0) exposing `laya_search`.
//! Kept dependency-free: the protocol surface we need is initialize, tools/list, tools/call, ping.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::hook::DaemonApi;
use crate::protocol::{Request, Response};

const DEFAULT_PROTOCOL: &str = "2025-06-18";

pub fn tool_list() -> Value {
    json!({"tools": [{
        "name": "laya_search",
        "description": "Ranked code search over this repository (tree-sitter chunks + BM25 + the Laya relevance model). \
Returns the most relevant 10-50 line code spans with file paths and line ranges. Use it to locate where something \
is implemented before reading files; then Read only the returned ranges (offset/limit).",
        "inputSchema": {"type": "object", "properties": {
            "query": {"type": "string", "description": "What you are looking for, in natural language and/or identifiers"},
            "top_n": {"type": "integer", "description": "Number of spans (default 10, max 20)"}
        }, "required": ["query"]}
    }]})
}

pub fn handle_message(msg: &Value, api: &dyn DaemonApi, root: &std::path::Path, inject_tokens: usize, budget_ms: u64) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg["method"].as_str().unwrap_or_default();
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or(DEFAULT_PROTOCOL),
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "laya-codex", "version": env!("CARGO_PKG_VERSION")}
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tool_list()),
        "tools/call" => Ok(call_tool(&msg["params"], api, root, inject_tokens, budget_ms)),
        _ if id.is_none() => return None, // notifications (e.g. notifications/initialized)
        _ => Err(json!({"code": -32601, "message": format!("method not found: {method}")})),
    };
    let id = id?;
    Some(match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
    })
}

fn call_tool(params: &Value, api: &dyn DaemonApi, root: &std::path::Path, inject_tokens: usize, budget_ms: u64) -> Value {
    let text_result = |text: String, is_error: bool| json!({"content": [{"type": "text", "text": text}], "isError": is_error});
    if params["name"] != "laya_search" {
        return text_result(format!("unknown tool {}", params["name"]), true);
    }
    let query = params["arguments"]["query"].as_str().unwrap_or_default().to_string();
    if query.trim().is_empty() {
        return text_result("query must not be empty".into(), true);
    }
    let top_n = params["arguments"]["top_n"].as_u64().map(|n| n.clamp(1, 20) as usize);
    let req = Request::Query { repo: root.to_string_lossy().into_owned(), session: None, prompt: query, budget_ms: Some(budget_ms), top_n, render: None };
    match api.call(req) {
        Ok(Response::Query { result, .. }) if !result.spans.is_empty() => text_result(laya_rank::render_context(&result, inject_tokens), false),
        Ok(Response::Query { .. }) => text_result("no matching code found; fall back to Grep/Glob".into(), false),
        Ok(other) => text_result(format!("unexpected daemon response: {other:?}"), true),
        Err(e) => text_result(format!("laya daemon unavailable ({e}); fall back to Grep/Glob"), true),
    }
}

pub fn serve(api: &dyn DaemonApi, root: PathBuf, inject_tokens: usize, budget_ms: u64) -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle_message(&msg, api, &root, inject_tokens, budget_ms),
            Err(e) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}})),
        };
        if let Some(r) = reply {
            writeln!(out, "{r}")?;
            out.flush()?;
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
            Ok(Response::Query { result: QueryResult {
                spans: vec![RankedSpan { path: "src/a.rs".into(), start_line: 3, end_line: 9, symbol: "fn a".into(),
                    p_relevant: Some(0.8), score: 0.8, text: "fn a() {}".into() }],
                mode: RankMode::Laya, elapsed_ms: 3, candidates: 5, related: vec![] }, rendered: None, scope: None })
        }
    }

    fn call(msg: Value, up: bool) -> Option<Value> {
        handle_message(&msg, &Fake(up), &PathBuf::from("/r"), 3000, 500)
    }

    #[test]
    fn initialize_echoes_protocol_and_lists_tool() {
        let r = call(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}}), true).unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        let t = call(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}), true).unwrap();
        assert_eq!(t["result"]["tools"][0]["name"], "laya_search");
        assert_eq!(call(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}), true), None);
    }

    #[test]
    fn search_returns_spans_or_graceful_error() {
        let msg = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "laya_search", "arguments": {"query": "wal replay"}}});
        let ok = call(msg.clone(), true).unwrap();
        assert!(ok["result"]["content"][0]["text"].as_str().unwrap().contains("src/a.rs"));
        let down = call(msg, false).unwrap();
        assert_eq!(down["result"]["isError"], true);
    }

    #[test]
    fn unknown_method_is_jsonrpc_error() {
        let r = call(json!({"jsonrpc": "2.0", "id": 4, "method": "resources/list"}), true).unwrap();
        assert_eq!(r["error"]["code"], -32601);
    }
}
