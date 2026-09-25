//! Minimal MCP server over stdio (newline-delimited JSON-RPC 2.0) exposing `search`.
//! Kept dependency-free: the protocol surface we need is initialize, tools/list, tools/call, ping.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use laya_core::RankedSpan;
use laya_parse::ChunkConfig;
use laya_rank::{FileMatches, IdentMatches, MatchGroup, MatchLine, render_matches};
use serde_json::{Value, json};

use crate::hook::DaemonApi;
use crate::protocol::{Request, Response};
use crate::trace::{self, Recording, Tracer};

const DEFAULT_PROTOCOL: &str = "2025-06-18";

/// Server instructions, which Claude Code adds to the system prompt. Benchmark v3 (v9): 72% of the
/// Greps agents still made were for identifiers (definition, callers, uses, tests), mostly names
/// they already knew; a pilot that steered them here by description alone got 0 calls.
const INSTRUCTIONS: &str = "laya-codex has indexed this repository. For the definition, callers, \
uses or tests of an identifier, call the laya-codex `search` tool with the name (or several, \
`foo|Bar`, optionally with a `path`) instead of Grep: it returns every line naming it (also inside \
longer names such as `test_foo`), grouped by enclosing function or test, and says whether the list \
is complete. Keep Grep for regular expressions and string literals.";

pub fn tool_list() -> Value {
    json!({"tools": [{
        "name": "search",
        "description": "Look up identifiers in this repository, or find code by description. Give one or \
    more identifiers (`generateDigest`, or `iter_text|aiter_text|TextChunker`) and optionally a `path` (a file \
    or directory, as for Grep): you get every line that contains them as a name or as part of a longer name \
    (`raise_for_status` also finds `test_raise_for_status`, `Transport` finds `HTTPTransport`, `quote` does not \
    find `unquote`), in code, docs and config, as `line: text` under each file, grouped by enclosing function, \
    class or test, with the definition marked, test files flagged and the match count, saying when the list is \
    complete. One call answers where a name is defined, its callers and uses and which tests cover it, without \
    the Read that usually follows a Grep. A description in words instead returns ranked code spans (10-50 lines \
    with paths and line numbers). Use Grep for regular expressions, string literals and text that is not an \
    identifier.",
        "inputSchema": {"type": "object", "properties": {
            "query": {"type": "string", "description": "One or more identifiers (`name`, `a|b|c`) for an exact lookup, or what you are looking for in words"},
            "path": {"type": "string", "description": "File or directory to search in (repo-relative or absolute); default the whole repository"},
            "top_n": {"type": "integer", "description": "Number of spans for a description in words (default 10, max 20)"}
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
    let scope = match resolve_scope(root, params["arguments"]["path"].as_str().unwrap_or("")) {
        Ok(s) => s,
        Err(e) => return text_result(e, true),
    };
    if let Some(idents) = identifier_query(&query) {
        return text_result(
            lookup(api, root, &idents, scope.as_deref(), budget_ms),
            false,
        );
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
        Ok(Response::Query { mut result, .. }) => {
            if let Some(s) = &scope {
                result.spans.retain(|x| in_scope(&x.path, s));
                result.related.retain(|x| in_scope(&x.path, s));
            }
            if result.spans.is_empty() {
                text_result(
                    "no matching code found; fall back to Grep/Glob".into(),
                    false,
                )
            } else {
                text_result(laya_rank::render_context(&result, inject_tokens), false)
            }
        }
        Ok(other) => text_result(format!("unexpected daemon response: {other:?}"), true),
        Err(e) => text_result(
            format!("laya-codex daemon unavailable ({e}); fall back to Grep/Glob"),
            true,
        ),
    }
}

/// At most this many names per lookup (agents' Grep alternations carry 1-5).
const MAX_IDENTS: usize = 8;

/// Words that frame an identifier lookup ("callers of x", "def x") rather than name code.
#[rustfmt::skip]
const FRAMING_WORDS: &[&str] = &[
    "a", "an", "the", "of", "for", "to", "in", "on", "at", "by", "with", "from", "and", "or", "is",
    "are", "where", "what", "which", "who", "how", "does", "do", "find", "show", "list", "all",
    "every", "its", "use", "uses", "usage", "usages", "used", "caller", "callers", "call", "calls",
    "called", "reference", "references", "refs", "test", "tests", "covering", "cover", "covers",
    "definition", "definitions", "defined", "define", "defines", "declaration", "implementation",
    "implemented", "def", "fn", "class", "function", "struct", "enum", "trait", "impl",
    "interface", "type", "const", "let", "var", "pub", "async", "export", "static", "mod",
];

/// Names an exact lookup searches for, or `None` when `query` describes code in words. A single
/// name, names separated by `|` or `,`, or several code-shaped names (`snake_case`, `CamelCase`,
/// digits, `a-b`, `a/b`) are a lookup; plain words ("wal replay") are a description. Framing words
/// ("callers of x", "def x") are skipped unless the names are listed with `|` or `,`.
fn identifier_query(query: &str) -> Option<Vec<String>> {
    let listed = query.contains('|') || query.contains(',');
    let mut names: Vec<String> = Vec::new();
    for raw in query.split(|c: char| c.is_whitespace() || c == '|' || c == ',') {
        let Some(token) = clean_token(raw) else {
            continue;
        };
        if !listed && FRAMING_WORDS.contains(&token.to_ascii_lowercase().as_str()) {
            continue;
        }
        if !is_identifier(&token) && !is_literal(&token) {
            return None;
        }
        if !names.contains(&token) {
            names.push(token);
        }
    }
    if names.is_empty() || names.len() > MAX_IDENTS {
        return None;
    }
    (names.len() == 1 || listed || names.iter().all(|i| code_shaped(i))).then_some(names)
}

/// A query token without regex escapes, quotes, call parentheses, anchors and trailing
/// punctuation; a qualified name (`Response.json`, `a::b`) becomes its last segment.
fn clean_token(raw: &str) -> Option<String> {
    let t = raw.replace("\\b", "").replace('\\', "");
    let mut t = t
        .trim_matches(|c: char| {
            matches!(
                c,
                '`' | '"' | '\'' | '^' | '(' | ')' | '?' | '!' | ';' | ':' | '.' | '*' | '='
            )
        })
        .to_string();
    if t.len() > 1 && t.ends_with('$') {
        t.pop();
    }
    if is_literal(&t) {
        return Some(t);
    }
    let last = t.rsplit(['.', ':']).next().unwrap_or_default().to_string();
    (!last.is_empty()).then_some(last)
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(is_ident_char)
}

/// Text that names code without being an identifier: a header or flag (`Permissions-Policy`,
/// `wal-max-lag`) or a module path (`middleware/etag`). Matched as plain text.
fn is_literal(s: &str) -> bool {
    s.len() >= 3
        && (s.contains('-') || s.contains('/'))
        && s.chars().any(|c| c.is_ascii_alphabetic())
        && s.chars()
            .all(|c| is_ident_char(c) || matches!(c, '-' | '/' | '.' | '@'))
}

fn code_shaped(s: &str) -> bool {
    s.chars()
        .any(|c| matches!(c, '_' | '$' | '-' | '/') || c.is_ascii_digit() || c.is_ascii_uppercase())
}

/// `name` occurs in `line` as a name or as a part of a longer one: at a word, `_` or camelCase
/// boundary on both sides. So `raise_for_status` matches `test_raise_for_status` and
/// `HTTPTransport` matches `AsyncHTTPTransport` (the test names and variants agents grep for),
/// while `quote` does not match `unquote`, `it` not `with`, `cookie` not `cookies`. Literals
/// (`a-b`, `a/b`) match as plain text.
fn has_name(line: &str, name: &str) -> bool {
    if !is_identifier(name) {
        return line.contains(name);
    }
    let (Some(first), Some(last)) = (name.chars().next(), name.chars().next_back()) else {
        return false;
    };
    line.match_indices(name).any(|(at, _)| {
        let before = line[..at].chars().next_back();
        let after = line[at + name.len()..].chars().next();
        starts_a_part(before, first) && ends_a_part(last, after)
    })
}

fn starts_a_part(before: Option<char>, first: char) -> bool {
    match before {
        None => true,
        Some(b) if !is_ident_char(b) => true,
        Some(b) => {
            b == '_' || first == '_' || (first.is_ascii_uppercase() && (b.is_ascii_alphanumeric()))
        }
    }
}

fn ends_a_part(last: char, after: Option<char>) -> bool {
    match after {
        None => true,
        Some(a) if !is_ident_char(a) => true,
        Some(a) => {
            a == '_' || last == '_' || (a.is_ascii_uppercase() && !last.is_ascii_uppercase())
        }
    }
}

fn in_scope(path: &str, scope: &str) -> bool {
    path == scope
        || path
            .strip_prefix(scope)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The `path` argument as a repo-relative scope (`None` = whole repository). Absolute paths
/// (what agents pass to Grep) are accepted inside the repository; anything outside it, or
/// missing, is an error.
fn resolve_scope(root: &Path, path: &str) -> Result<Option<String>, String> {
    let p = path.trim().trim_end_matches('/');
    if p.is_empty() || p == "." {
        return Ok(None);
    }
    let candidate = if Path::new(p).is_absolute() {
        PathBuf::from(p)
    } else {
        root.join(p)
    };
    let (Ok(full), Ok(base)) = (candidate.canonicalize(), root.canonicalize()) else {
        return Err(format!("path does not exist: {p}"));
    };
    let rel = full
        .strip_prefix(&base)
        .map_err(|_| format!("path is outside this repository: {p}"))?;
    let rel = rel.to_string_lossy().replace('\\', "/");
    Ok((!rel.is_empty()).then_some(rel))
}

/// A lookup reads files until this deadline; past it the answer says it may be incomplete.
const SCAN_DEADLINE: Duration = Duration::from_secs(3);
/// A file named as the `path` is read only up to this size (the walk skips files over 1 MiB).
const MAX_SCOPED_FILE_BYTES: u64 = 16 << 20;
/// Matching lines are cut to this many characters.
const MATCH_LINE_CHARS: usize = 160;
/// A definition's code is shown only for chunks up to this many lines.
const DEFINITION_MAX_LINES: u32 = 40;

/// One file with at least one matching line.
struct Hit {
    rel: String,
    source: String,
    /// 1-based numbers of the matching lines.
    lines: Vec<u32>,
}

struct Scan {
    files_searched: usize,
    complete: bool,
    hits: Vec<Hit>,
}

/// Read every indexable file under `scope` (the files laya-codex indexes: code, docs, config;
/// `.gitignore` respected) and keep the lines containing a name (see [`has_name`]).
fn scan(root: &Path, scope: Option<&str>, idents: &[String], deadline: Duration) -> Scan {
    let started = Instant::now();
    let base = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let files: Vec<PathBuf> = match scope {
        Some(s) if base.join(s).is_file() => std::fs::metadata(base.join(s))
            .is_ok_and(|m| m.len() <= MAX_SCOPED_FILE_BYTES)
            .then(|| base.join(s))
            .into_iter()
            .collect(),
        Some(s) => laya_parse::walk_repo(&base.join(s)),
        None => laya_parse::walk_repo(&base),
    };
    let mut out = Scan {
        files_searched: 0,
        complete: true,
        hits: Vec::new(),
    };
    for path in files {
        if started.elapsed() > deadline {
            out.complete = false;
            break;
        }
        out.files_searched += 1;
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        if !idents.iter().any(|i| source.contains(i.as_str())) {
            continue;
        }
        let lines: Vec<u32> = source
            .lines()
            .enumerate()
            .filter(|(_, l)| idents.iter().any(|i| has_name(l, i)))
            .map(|(n, _)| n as u32 + 1)
            .collect();
        if !lines.is_empty() {
            let rel = path.strip_prefix(&base).unwrap_or(&path);
            out.hits.push(Hit {
                rel: rel.to_string_lossy().replace('\\', "/"),
                source,
                lines,
            });
        }
    }
    out
}

/// Order `hits` by the daemon's ranking for the names (lexical candidates + Laya rerank), then
/// the unranked files by match count. The ranking is optional: without the daemon the list is
/// still complete, only its order is plainer.
fn rank_hits(
    api: &dyn DaemonApi,
    root: &Path,
    idents: &[String],
    hits: &mut [Hit],
    budget_ms: u64,
) {
    if hits.len() < 2 {
        return;
    }
    let req = Request::Query {
        repo: root.to_string_lossy().into_owned(),
        session: None,
        prompt: idents.join(" "),
        budget_ms: Some(budget_ms),
        top_n: Some(crate::protocol::MAX_TOP_N),
        render: None,
    };
    let ranked: Vec<String> = match api.call(req) {
        Ok(Response::Query { result, .. }) => result.spans.into_iter().map(|s| s.path).collect(),
        _ => Vec::new(),
    };
    let pos = |rel: &str| ranked.iter().position(|p| p == rel).unwrap_or(usize::MAX);
    hits.sort_by(|a, b| {
        pos(&a.rel)
            .cmp(&pos(&b.rel))
            .then(b.lines.len().cmp(&a.lines.len()))
            .then(a.rel.cmp(&b.rel))
    });
}

/// One chunk per line: each chunk's symbol is then the deepest definition containing that line
/// (e.g. `class Client > def stream`) and its `defines` the names declared on it.
const LINE_CHUNKS: ChunkConfig = ChunkConfig {
    min_lines: 1,
    max_lines: 1,
    text_window_lines: 1,
};

/// Group a file's matching lines by their innermost enclosing definition (tree-sitter, as
/// indexed; JS/TS test blocks `describe('x') > it('y')` by name); a line is a definition when it
/// declares the name. With `with_code`, also returns the indexed chunk holding the first
/// definition, when it is short enough to show.
fn file_matches(
    hit: &Hit,
    idents: &[String],
    with_code: bool,
) -> (FileMatches, Option<RankedSpan>) {
    let chunks = laya_parse::chunk_source_with(&LINE_CHUNKS, &hit.rel, &hit.source);
    let text: Vec<&str> = hit.source.lines().collect();
    let js = is_js(&hit.rel);
    let mut groups: Vec<MatchGroup> = Vec::new();
    let mut first_def = None;
    for &n in &hit.lines {
        let at = chunks.partition_point(|c| c.end_line < n);
        let chunk = chunks.get(at).filter(|c| c.start_line <= n);
        let definition = chunk.is_some_and(|c| idents.iter().any(|i| c.defines.contains(i)));
        if definition && first_def.is_none() {
            first_def = Some(n);
        }
        let mut symbol = chunk.map(|c| c.symbol.clone()).unwrap_or_default();
        if let Some(blocks) = js.then(|| js_test_blocks(&text, n)).flatten() {
            symbol = if symbol.is_empty() {
                blocks
            } else {
                format!("{symbol} > {blocks}")
            };
        }
        let line = MatchLine {
            line: n,
            text: text
                .get(n as usize - 1)
                .map(|l| l.trim().chars().take(MATCH_LINE_CHARS).collect())
                .unwrap_or_default(),
            definition,
        };
        match groups.last_mut() {
            Some(g) if g.symbol == symbol => g.lines.push(line),
            _ => groups.push(MatchGroup {
                symbol,
                lines: vec![line],
            }),
        }
    }
    let code = first_def.filter(|_| with_code).and_then(|n| {
        laya_parse::chunk_source(&hit.rel, &hit.source)
            .into_iter()
            .find(|c| c.start_line <= n && n <= c.end_line)
            .filter(|c| c.end_line - c.start_line < DEFINITION_MAX_LINES)
            .map(|c| RankedSpan {
                path: hit.rel.clone(),
                start_line: c.start_line,
                end_line: c.end_line,
                symbol: c.symbol,
                p_relevant: None,
                score: 0.0,
                text: c.text,
            })
    });
    (
        FileMatches {
            path: hit.rel.clone(),
            groups,
        },
        code,
    )
}

fn is_js(path: &str) -> bool {
    matches!(
        path.rsplit('.').next(),
        Some("ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts")
    )
}

/// How far up a JS/TS test file a line's `describe`/`it` openers are looked for.
const TEST_BLOCK_LOOKBACK: usize = 400;

/// The `describe('…') > it('…')` blocks around line `n` (1-based) of a JS/TS file, found by
/// walking up to lines of smaller indentation. Tree-sitter sees these callbacks as anonymous
/// functions, so the chunk symbol alone cannot say which test a line is in.
fn js_test_blocks(lines: &[&str], n: u32) -> Option<String> {
    let start = (n as usize).checked_sub(1).filter(|&s| s < lines.len())?;
    let mut chain: Vec<String> = Vec::new();
    let mut limit = usize::MAX;
    for k in (start.saturating_sub(TEST_BLOCK_LOOKBACK)..=start).rev() {
        let line = lines[k];
        let body = line.trim_start();
        let indent = line.len() - body.len();
        if body.is_empty() || indent >= limit {
            continue;
        }
        if let Some(label) = test_opener(body) {
            chain.push(label);
        }
        limit = indent;
        if indent == 0 {
            break;
        }
    }
    chain.reverse();
    (!chain.is_empty()).then(|| chain.join(" > "))
}

/// `describe('name'`, `it("name"`, `test.only(`name`` ... as `describe('name')`.
fn test_opener(body: &str) -> Option<String> {
    let (kw, rest) = ["describe", "it", "test", "suite", "bench"]
        .iter()
        .find_map(|kw| body.strip_prefix(kw).map(|r| (*kw, r)))?;
    let rest = rest
        .strip_prefix(".only")
        .or_else(|| rest.strip_prefix(".skip"))
        .or_else(|| rest.strip_prefix(".concurrent"))
        .unwrap_or(rest);
    let rest = rest.strip_prefix('(')?.trim_start();
    let quote = rest
        .chars()
        .next()
        .filter(|c| matches!(c, '\'' | '"' | '`'))?;
    let name: String = rest[1..].split(quote).next()?.chars().take(80).collect();
    Some(format!("{kw}('{name}')"))
}

/// A file past the renderer's cap: its lines are counted, never shown, so it is not parsed.
fn counted_only(hit: &Hit) -> FileMatches {
    FileMatches {
        path: hit.rel.clone(),
        groups: vec![MatchGroup {
            symbol: String::new(),
            lines: hit
                .lines
                .iter()
                .map(|&n| MatchLine {
                    line: n,
                    text: String::new(),
                    definition: false,
                })
                .collect(),
        }],
    }
}

/// Exact lookup: every line naming one of `idents` in the indexed files under `scope`, ranked
/// and rendered (see [`laya_rank::render_matches`]). Never fails: the daemon only orders files.
fn lookup(
    api: &dyn DaemonApi,
    root: &Path,
    idents: &[String],
    scope: Option<&str>,
    budget_ms: u64,
) -> String {
    let mut s = scan(root, scope, idents, SCAN_DEADLINE);
    rank_hits(api, root, idents, &mut s.hits, budget_ms);
    // Enclosing symbols need a parse; only files that can still be shown get one. Every line
    // renders to at least its text plus a few chars, so once the lines parsed so far exceed the
    // cap, later files can only be counted.
    let mut room = laya_rank::MAX_INJECT_CHARS;
    let mut files = Vec::with_capacity(s.hits.len());
    let mut definition = None;
    for hit in &s.hits {
        if room == 0 {
            files.push(counted_only(hit));
            continue;
        }
        // The code of the definition only for a lookup of one name: alternations (`a|b|c`) hunt
        // callers and tests, and would pay for code they do not read.
        let (f, def) = file_matches(hit, idents, idents.len() == 1 && definition.is_none());
        let shown: usize = f
            .groups
            .iter()
            .flat_map(|g| g.lines.iter())
            .map(|l| l.text.len() + 4)
            .sum();
        room = room.saturating_sub(shown + f.path.len());
        definition = definition.or(def);
        files.push(f);
    }
    render_matches(&IdentMatches {
        idents: idents.to_vec(),
        scope: scope.map(str::to_string),
        files_searched: s.files_searched,
        scan_complete: s.complete,
        files,
        definition,
    })
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
    fn initialize_tells_claude_to_look_up_identifiers_with_search() {
        let r = call(
            json!({"jsonrpc": "2.0", "id": 6, "method": "initialize", "params": {}}),
            true,
        )
        .unwrap();
        let i = r["result"]["instructions"].as_str().unwrap_or_default();
        assert!(i.contains("instead of Grep"), "instructions: {i:?}");
        assert!(i.contains("`search`"), "instructions name the tool: {i:?}");
        assert!(i.contains("identifier"), "instructions say for what: {i:?}");
    }

    /// A daemon whose ranking puts `paths` first, in that order.
    struct Ranked(Vec<&'static str>);
    impl DaemonApi for Ranked {
        fn call(&self, _req: Request) -> anyhow::Result<Response> {
            Ok(Response::Query {
                result: QueryResult {
                    spans: self
                        .0
                        .iter()
                        .map(|p| RankedSpan {
                            path: (*p).into(),
                            start_line: 1,
                            end_line: 2,
                            symbol: String::new(),
                            p_relevant: None,
                            score: 1.0,
                            text: String::new(),
                        })
                        .collect(),
                    mode: RankMode::Laya,
                    elapsed_ms: 1,
                    candidates: 1,
                    scored: 0,
                    offered: 0,
                    related: vec![],
                },
                rendered: None,
                scope: None,
            })
        }
    }

    /// A small repository: a definition, a caller, a test, a doc and a near-miss name.
    fn repo(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("laya-mcp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let files = [
            (
                "pkg/digest.py",
                "def generate_digest(stream):\n    return stream\n\n\ndef generate_digests(stream):\n    return stream\n",
            ),
            (
                "pkg/etag.py",
                "from pkg.digest import generate_digest\n\n\ndef etag(body):\n    digest = generate_digest(body)\n    return digest\n",
            ),
            (
                "tests/test_digest.py",
                "from pkg.digest import generate_digest\n\n\ndef test_hashes():\n    assert generate_digest(\"a\") == \"a\"\n",
            ),
            (
                "docs/notes.md",
                "# Notes\n\ngenerate_digest is documented here.\n",
            ),
            (
                "web/etag.test.ts",
                "import { generate_digest } from '../pkg'\n\ndescribe('etag', () => {\n  it('hashes a body', async () => {\n    const h = generate_digest('a')\n    expect(h).toBe('a')\n  })\n})\n",
            ),
        ];
        for (rel, text) in files {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        root
    }

    fn search_text(api: &dyn DaemonApi, root: &Path, args: Value) -> (String, bool) {
        let msg = json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call",
            "params": {"name": "search", "arguments": args}});
        let r = handle_message(&msg, api, root, 3000, 500).unwrap();
        (
            r["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            r["result"]["isError"] == true,
        )
    }

    #[test]
    fn identifier_queries_are_told_apart_from_descriptions() {
        let ids = |q: &str| identifier_query(q);
        let v = |xs: &[&str]| Some(xs.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(ids("generateDigest"), v(&["generateDigest"]));
        assert_eq!(
            ids("iter_text|aiter_text|TextChunker"),
            v(&["iter_text", "aiter_text", "TextChunker"])
        );
        assert_eq!(
            ids("iter_text, TextChunker"),
            v(&["iter_text", "TextChunker"])
        );
        assert_eq!(
            ids("Response.json()"),
            v(&["json"]),
            "a member is looked up by its name"
        );
        assert_eq!(
            ids("\\.json\\("),
            v(&["json"]),
            "a Grep-style pattern still works"
        );
        assert_eq!(ids("def json"), v(&["json"]));
        assert_eq!(
            ids("callers and tests of generate_digest"),
            v(&["generate_digest"])
        );
        assert_eq!(ids("$url"), v(&["$url"]), "JS names may start with $");
        assert_eq!(
            ids("recover_shard_v3 RecoveryTarget"),
            v(&["recover_shard_v3", "RecoveryTarget"])
        );
        assert_eq!(
            ids("Promise|async|describe"),
            v(&["Promise", "async", "describe"]),
            "a listed name is never a framing word"
        );
        assert_eq!(
            ids("Permissions-Policy|middleware/etag"),
            v(&["Permissions-Policy", "middleware/etag"]),
            "header names and module paths are looked up as text"
        );
        assert_eq!(ids("wal replay"), None, "plain words are a description");
        assert_eq!(ids("how does the wal replay work"), None);
        assert_eq!(ids("raise_for_status returns self?"), None);
        assert_eq!(ids("   "), None);
    }

    #[test]
    fn a_name_matches_whole_or_as_a_part_of_a_longer_name() {
        // v9 replay: whole-word matching missed the test names (`test_raise_for_status`) and
        // variants (`AsyncHTTPTransport`) that agents' Greps found and used.
        for (name, line) in [
            ("raise_for_status", "def test_raise_for_status():"),
            ("HTTPTransport", "transport = AsyncHTTPTransport()"),
            (
                "StreamingApi",
                "class SSEStreamingApi extends StreamingApi {",
            ),
            ("toString", "child.toStringToBuffer(buffer)"),
            ("_encoding", "self.default_encoding = x"),
            ("recover_shard", "recover_shard_v3(&dir)"),
            ("json", "r.json()"),
            ("Permissions-Policy", "'Permissions-Policy': value"),
            (
                "middleware/etag",
                "import { etag } from 'hono/middleware/etag'",
            ),
        ] {
            assert!(has_name(line, name), "{name:?} in {line:?}");
        }
        for (name, line) in [
            ("iter_text", "async def aiter_text(self):"),
            ("quote", "return unquote(s)"),
            ("it", "with open(p) as f:"),
            ("cookie", "cookies = {}"),
            ("lsn", "let lsns = vec![];"),
            ("url", "$url()"),
        ] {
            assert!(!has_name(line, name), "{name:?} must not match {line:?}");
        }
    }

    #[test]
    fn scope_is_repo_relative_and_stays_inside_the_repository() {
        let root = repo("scope");
        assert_eq!(resolve_scope(&root, ""), Ok(None));
        assert_eq!(resolve_scope(&root, "."), Ok(None));
        assert_eq!(resolve_scope(&root, "pkg/"), Ok(Some("pkg".into())));
        let abs = root.join("pkg/etag.py");
        assert_eq!(
            resolve_scope(&root, &abs.to_string_lossy()),
            Ok(Some("pkg/etag.py".into()))
        );
        assert!(resolve_scope(&root, "../outside").is_err());
        assert!(resolve_scope(&root, "/etc").is_err());
        assert!(resolve_scope(&root, "pkg/missing.py").is_err());
    }

    #[test]
    fn an_identifier_search_lists_every_matching_line_by_enclosing_function() {
        let root = repo("lookup");
        let (out, err) = search_text(&Fake(true), &root, json!({"query": "generate_digest"}));
        assert!(!err, "{out}");
        assert!(out.contains("8 matching lines in 5 files"), "{out}");
        assert!(out.contains("Complete"), "{out}");
        assert!(
            out.contains(
                "in def generate_digest:\n    1: def generate_digest(stream):  [definition]"
            ),
            "the innermost definition encloses each line: {out}"
        );
        assert!(
            out.contains("\npkg/etag.py\n  top level:\n    1: from pkg.digest import generate_digest\n  in def etag:\n    5: digest = generate_digest(body)\n"),
            "{out}"
        );
        assert!(
            out.contains(
                "in describe('etag') > it('hashes a body'):\n    5: const h = generate_digest('a')"
            ),
            "JS/TS test blocks are named: {out}"
        );
        assert!(out.contains("tests/test_digest.py (test file)"), "{out}");
        assert!(out.contains("in def test_hashes:"), "{out}");
        assert!(
            out.contains("docs/notes.md"),
            "docs are searched too: {out}"
        );
        let list = out.split("Definition:").next().unwrap();
        assert!(
            !list.contains("generate_digests"),
            "whole names or name parts only: {out}"
        );
        assert!(
            out.contains("### pkg/digest.py:1-6"),
            "the definition's code follows: {out}"
        );
    }

    #[test]
    fn only_a_single_name_lookup_shows_the_definitions_code() {
        // v9: 78% of Grep patterns were alternations (`a|b|c`), hunting callers and tests; the
        // definition's code is what a lookup of one name is for.
        let root = repo("defcode");
        let (one, _) = search_text(&Fake(false), &root, json!({"query": "generate_digest"}));
        assert!(one.contains("Definition:"), "{one}");
        let (two, _) = search_text(
            &Fake(false),
            &root,
            json!({"query": "generate_digest|etag"}),
        );
        assert!(!two.contains("Definition:"), "{two}");
        assert!(
            two.contains("[definition]"),
            "the line is still marked: {two}"
        );
    }

    #[test]
    fn an_identifier_search_answers_even_when_the_daemon_is_down() {
        let root = repo("down");
        let (out, err) = search_text(&Fake(false), &root, json!({"query": "generate_digest"}));
        assert!(!err, "fail open: {out}");
        assert!(out.contains("pkg/etag.py"), "{out}");
    }

    #[test]
    fn files_follow_the_ranking_then_the_rest() {
        let root = repo("rank");
        let api = Ranked(vec!["tests/test_digest.py", "pkg/etag.py"]);
        let (out, _) = search_text(&api, &root, json!({"query": "generate_digest"}));
        let at = |p: &str| out.find(&format!("\n{p}")).unwrap_or(usize::MAX);
        assert!(at("tests/test_digest.py") < at("pkg/etag.py"), "{out}");
        assert!(at("pkg/etag.py") < at("pkg/digest.py"), "{out}");
        assert!(at("pkg/digest.py") < usize::MAX, "{out}");
    }

    #[test]
    fn a_path_restricts_the_lookup_like_greps_path() {
        let root = repo("path");
        let (out, _) = search_text(
            &Fake(true),
            &root,
            json!({"query": "generate_digest", "path": "pkg"}),
        );
        assert!(out.contains("under pkg"), "{out}");
        assert!(
            out.contains("pkg/etag.py") && !out.contains("tests/test_digest.py"),
            "{out}"
        );
        let (none, err) = search_text(
            &Fake(true),
            &root,
            json!({"query": "generate_digest", "path": "docs"}),
        );
        assert!(!err && none.contains("docs/notes.md"), "{none}");
        let (missing, err) = search_text(
            &Fake(true),
            &root,
            json!({"query": "no_such_name_anywhere"}),
        );
        assert!(!err);
        assert!(missing.contains("No match"), "{missing}");
        let (bad, err) = search_text(
            &Fake(true),
            &root,
            json!({"query": "generate_digest", "path": "../x"}),
        );
        assert!(err, "{bad}");
    }

    #[test]
    fn a_description_still_gets_ranked_code() {
        let root = repo("words");
        let (out, err) = search_text(&Fake(true), &root, json!({"query": "wal replay"}));
        assert!(!err);
        assert!(out.contains("### src/a.rs:3-9"), "{out}");
    }

    #[test]
    fn the_tool_takes_a_path_and_says_what_an_identifier_lookup_returns() {
        let t = tool_list();
        let tool = &t["tools"][0];
        assert!(tool["inputSchema"]["properties"]["path"].is_object());
        let d = tool["description"].as_str().unwrap();
        for needle in [
            "identifier",
            "every line",
            "part of a longer name",
            "enclosing function",
            "definition",
            "callers",
            "tests",
            "complete",
            "Grep for regular expressions",
        ] {
            assert!(d.contains(needle), "description lacks {needle:?}: {d}");
        }
        assert!(INSTRUCTIONS.contains("instead of Grep"), "{INSTRUCTIONS}");
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
