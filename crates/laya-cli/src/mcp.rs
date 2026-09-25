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
        match lookup(api, root, &idents, scope.as_deref(), budget_ms) {
            // One plain word ("retry") that names nothing may still describe code: rank it.
            Ok(found) if found.lines == 0 && idents.len() == 1 && !code_shaped(&idents[0]) => {}
            Ok(found) => return text_result(found.text, false),
            Err(e) => return text_result(e, true),
        }
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
/// name, names listed with `|`, or several code-shaped names (`snake_case`, `CamelCase`, digits,
/// `a-b`, `a/b`; separated by spaces or commas) are a lookup; plain words ("wal replay", "where is
/// handler defined, and who calls it") are a description. Framing words ("callers of x", "def x")
/// are skipped unless the names are listed with `|`.
fn identifier_query(query: &str) -> Option<Vec<String>> {
    let listed = query.contains('|');
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

/// Everything a lookup does (walk, reads, parses) happens before this deadline; past it the
/// answer says what it did not cover and never claims to be complete. `serve` handles one request
/// at a time, so a lookup must never hold the server longer than this.
const LOOKUP_DEADLINE: Duration = Duration::from_secs(3);
/// Matching lines are cut to this many characters.
const MATCH_LINE_CHARS: usize = 160;
/// A definition's code is shown only for chunks up to this many lines.
const DEFINITION_MAX_LINES: u32 = 40;
/// Chunking for enclosing symbols, one parse per file: one chunk per line, so each chunk's symbol
/// is the deepest definition containing that line (e.g. `class Client > def stream`) and its
/// `defines` the names declared on it. Coarser chunks pack small sibling functions (and the
/// imports above them) into one chunk; per MiB this pass costs 1.0-1.4x a default chunking.
const SYMBOL_CHUNKS: ChunkConfig = ChunkConfig {
    min_lines: 1,
    max_lines: 1,
    text_window_lines: 1,
};
/// Parse cost assumed before any parse was timed: 1 µs per byte, about 1 s per MiB (the worst
/// case seen). Blended with measured parses, it decides whether a parse still fits the deadline.
const PRIOR_PARSE_NS_PER_BYTE: u64 = 1_000;
const PRIOR_PARSE_BYTES: u64 = 256 << 10;

/// One file with at least one matching line. Only line numbers are kept: the text of the lines
/// that are shown is read again when they are rendered, so a lookup over many files holds no
/// file contents.
struct Hit {
    rel: String,
    abs: PathBuf,
    /// 1-based numbers of the matching lines.
    lines: Vec<u32>,
}

struct Scan {
    files_read: usize,
    /// Files in scope that were not searched: over 1 MiB, unreadable or not UTF-8.
    files_skipped: usize,
    complete: bool,
    hits: Vec<Hit>,
}

/// Read every indexable file under `scope` (the files laya-codex indexes: code, docs, config;
/// `.gitignore` respected, at most [`laya_parse::MAX_FILE_BYTES`]) and keep the numbers of the
/// lines containing a name (see [`has_name`]), until `deadline`.
/// A named file is searched only if the indexer would index it ([`crate::indexer::walk_admits`]:
/// never secrets such as `.env`, hidden, ignored or unknown-type files, nor over 1 MiB).
fn scan(
    root: &Path,
    scope: Option<&str>,
    idents: &[String],
    deadline: Instant,
) -> Result<Scan, String> {
    let base = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let walked = match scope {
        Some(s) if base.join(s).is_file() => Walked {
            files: vec![crate::indexer::walk_admits(&base, s).ok_or_else(|| not_indexed(s))?],
            too_big: 0,
            complete: true,
        },
        dir => walk_scope(&base, dir, deadline),
    };
    let mut out = Scan {
        files_read: 0,
        files_skipped: walked.too_big,
        complete: walked.complete,
        hits: Vec::new(),
    };
    for path in walked.files {
        if Instant::now() > deadline {
            out.complete = false;
            break;
        }
        let Ok(source) = std::fs::read_to_string(&path) else {
            out.files_skipped += 1;
            continue;
        };
        out.files_read += 1;
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
                abs: path,
                lines,
            });
        }
    }
    Ok(out)
}

/// At most this many files are walked for one lookup.
const MAX_WALK_FILES: usize = 100_000;

/// The files the index would hold under `dir` (all of them when `None`).
struct Walked {
    files: Vec<PathBuf>,
    /// Files of a known type skipped for size (over [`laya_parse::MAX_FILE_BYTES`]).
    too_big: usize,
    /// The walk reached every file (no deadline or [`MAX_WALK_FILES`] cut).
    complete: bool,
}

/// Walk from the repository root with exactly [`laya_parse::walk_repo`]'s settings (hidden
/// entries, `.gitignore`/`.ignore`/git excludes, no symlinks), pruned to `dir`, and admit a file
/// only if `lang_for_path` accepts its repo-relative path (so `vendor/`, `node_modules/` and
/// secrets stay out, as in the index) and it is at most 1 MiB. Bounded by `deadline` and
/// [`MAX_WALK_FILES`].
fn walk_scope(base: &Path, dir: Option<&str>, deadline: Instant) -> Walked {
    let mut builder = ignore::WalkBuilder::new(base);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .ignore(true)
        .parents(true)
        .require_git(false)
        .follow_links(false)
        .sort_by_file_name(|a, b| a.cmp(b));
    if let Some(d) = dir {
        let scope = base.join(d);
        builder.filter_entry(move |e| scope.starts_with(e.path()) || e.path().starts_with(&scope));
    }
    let mut out = Walked {
        files: Vec::new(),
        too_big: 0,
        complete: true,
    };
    for entry in builder.build().flatten() {
        if out.files.len() >= MAX_WALK_FILES || Instant::now() > deadline {
            out.complete = false;
            break;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let rel = entry.path().strip_prefix(base).unwrap_or(entry.path());
        if laya_parse::lang_for_path(&rel.to_string_lossy().replace('\\', "/")).is_none() {
            continue;
        }
        match entry.metadata() {
            Ok(m) if m.len() <= laya_parse::MAX_FILE_BYTES => out.files.push(entry.into_path()),
            _ => out.too_big += 1,
        }
    }
    out
}

fn not_indexed(path: &str) -> String {
    format!(
        "`{path}` is not an indexed file (secrets such as .env, hidden, ignored, binary or \
unknown-type files and files over 1 MiB are never read); search does not look inside it."
    )
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

/// Decides whether a parse still fits before the deadline, from the parse speed seen so far
/// (starting from [`PRIOR_PARSE_NS_PER_BYTE`]).
struct ParseClock {
    deadline: Instant,
    ns: u64,
    bytes: u64,
}

impl ParseClock {
    fn new(deadline: Instant) -> Self {
        ParseClock {
            deadline,
            ns: PRIOR_PARSE_NS_PER_BYTE * PRIOR_PARSE_BYTES,
            bytes: PRIOR_PARSE_BYTES,
        }
    }

    fn allows(&self, len: usize) -> bool {
        let estimate = Duration::from_nanos(self.ns / self.bytes.max(1) * len as u64);
        Instant::now() + estimate < self.deadline
    }

    fn record(&mut self, len: usize, took: Duration) {
        self.ns += took.as_nanos() as u64;
        self.bytes += len as u64;
    }
}

/// First line of `chunk` containing `ident`.
fn first_line_with(chunk: &laya_core::Chunk, ident: &str) -> Option<u32> {
    chunk
        .text
        .lines()
        .position(|l| has_name(l, ident))
        .map(|k| chunk.start_line + k as u32)
}

/// The matching lines of one file, grouped by innermost enclosing definition (one tree-sitter
/// parse, as indexed; JS/TS test blocks `describe('x') > it('y')` by name) when the parse fits
/// the `clock`, else listed flat. A line is a definition when its chunk declares the name there.
/// With `with_code`, also returns the chunk holding the first definition, when short enough.
fn file_matches(
    hit: &Hit,
    idents: &[String],
    with_code: bool,
    clock: &mut ParseClock,
) -> (FileMatches, Option<RankedSpan>) {
    let source = std::fs::read_to_string(&hit.abs).unwrap_or_default();
    let text: Vec<&str> = source.lines().collect();
    let line_text = |n: u32| -> String {
        text.get(n as usize - 1)
            .map(|l| l.trim().chars().take(MATCH_LINE_CHARS).collect())
            .unwrap_or_default()
    };
    if !clock.allows(source.len()) {
        let lines = hit
            .lines
            .iter()
            .map(|&n| MatchLine {
                line: n,
                text: line_text(n),
                definition: false,
            })
            .collect();
        return (
            FileMatches {
                path: hit.rel.clone(),
                grouped: false,
                groups: vec![MatchGroup {
                    symbol: String::new(),
                    lines,
                }],
            },
            None,
        );
    }
    let t0 = Instant::now();
    let chunks = laya_parse::chunk_source_with(&SYMBOL_CHUNKS, &hit.rel, &source);
    clock.record(source.len(), t0.elapsed());
    let js = is_js(&hit.rel);
    let mut groups: Vec<MatchGroup> = Vec::new();
    let mut first_def: Option<&laya_core::Chunk> = None;
    for &n in &hit.lines {
        let at = chunks.partition_point(|c| c.end_line < n);
        let chunk = chunks.get(at).filter(|c| c.start_line <= n);
        let definition = chunk.is_some_and(|c| {
            idents
                .iter()
                .any(|i| c.defines.contains(i) && first_line_with(c, i) == Some(n))
        });
        if definition && first_def.is_none() {
            first_def = chunk;
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
            text: line_text(n),
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
    let code = first_def
        .filter(|_| with_code)
        .and_then(|c| definition_code(&chunks, &text, c, &hit.rel));
    (
        FileMatches {
            path: hit.rel.clone(),
            grouped: true,
            groups,
        },
        code,
    )
}

/// The code of the definition starting at `def` (a line chunk): the following lines whose
/// symbol is the definition's own or nested in it, if that is at most [`DEFINITION_MAX_LINES`].
fn definition_code(
    chunks: &[laya_core::Chunk],
    text: &[&str],
    def: &laya_core::Chunk,
    rel: &str,
) -> Option<RankedSpan> {
    if def.symbol.is_empty() {
        return None;
    }
    let nested = format!("{} >", def.symbol);
    let start = chunks.partition_point(|c| c.start_line < def.start_line);
    let end = chunks[start..]
        .iter()
        .take_while(|c| c.symbol == def.symbol || c.symbol.starts_with(&nested))
        .last()?
        .end_line;
    if end - def.start_line >= DEFINITION_MAX_LINES {
        return None;
    }
    let body = text
        .get(def.start_line as usize - 1..end as usize)?
        .join("\n");
    Some(RankedSpan {
        path: rel.to_string(),
        start_line: def.start_line,
        end_line: end,
        symbol: def.symbol.clone(),
        p_relevant: None,
        score: 0.0,
        text: body,
    })
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

/// A file past the renderer's cap: its lines are counted, never shown, so it is not read again.
fn counted_only(hit: &Hit) -> FileMatches {
    FileMatches {
        path: hit.rel.clone(),
        grouped: false,
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

/// Exact lookup: every line naming one of `idents` in the files on disk under `scope`, ranked
/// and rendered (see [`laya_rank::render_matches`]), within [`LOOKUP_DEADLINE`]. Fails only for
/// a named file the indexer would not admit; the daemon only orders files.
fn lookup(
    api: &dyn DaemonApi,
    root: &Path,
    idents: &[String],
    scope: Option<&str>,
    budget_ms: u64,
) -> Result<Lookup, String> {
    let deadline = Instant::now() + LOOKUP_DEADLINE;
    let mut s = scan(root, scope, idents, deadline)?;
    rank_hits(api, root, idents, &mut s.hits, budget_ms);
    // Only files that can still be shown are read again and parsed. Every line renders to at
    // least its text plus a few chars, so once the lines taken so far exceed the cap, later
    // files can only be counted.
    let mut clock = ParseClock::new(deadline);
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
        let with_code = idents.len() == 1 && definition.is_none();
        let (f, def) = file_matches(hit, idents, with_code, &mut clock);
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
    let lines = s.hits.iter().map(|h| h.lines.len()).sum();
    let text = render_matches(&IdentMatches {
        idents: idents.to_vec(),
        scope: scope.map(str::to_string),
        files_read: s.files_read,
        files_skipped: s.files_skipped,
        scan_complete: s.complete,
        files,
        definition,
    });
    Ok(Lookup { text, lines })
}

/// A rendered lookup and how many matching lines it found.
struct Lookup {
    text: String,
    lines: usize,
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

    fn add(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    /// About `bytes` of Python that uses `generate_digest` once, at the end: every such file
    /// has a line to show, so each one would be parsed for its enclosing function.
    fn big_python(bytes: usize) -> String {
        let block = "def handler_{i}(stream, other):\n    value = compute(stream) + compute(other)\n    return [value, stream]\n\n\n";
        let mut s = String::new();
        let mut i = 0;
        while s.len() + 200 < bytes {
            s.push_str(&block.replace("{i}", &i.to_string()));
            i += 1;
        }
        s.push_str("def last(stream):\n    return generate_digest(stream)\n");
        s
    }

    #[test]
    fn a_lookup_answers_within_its_time_budget_on_large_files() {
        // Review: parsing sat outside the scan deadline; a 15 MiB file named in `path` took 251 s
        // and blocked every later request.
        let root = repo("large");
        add(&root, "big/huge.py", &big_python(2 << 20));
        for k in 0..20 {
            add(&root, &format!("src/part{k}.py"), &big_python(1_000_000));
        }
        let t0 = Instant::now();
        let (out, _) = search_text(
            &Fake(true),
            &root,
            json!({"query": "generate_digest", "path": "big/huge.py"}),
        );
        assert!(
            t0.elapsed() < Duration::from_millis(3_500),
            "{:?}",
            t0.elapsed()
        );
        assert!(!out.contains("Complete"), "{}", &out[..out.len().min(600)]);

        let t0 = Instant::now();
        let (out, _) = search_text(&Fake(true), &root, json!({"query": "generate_digest"}));
        assert!(
            t0.elapsed() < Duration::from_millis(3_500),
            "{:?}",
            t0.elapsed()
        );
        assert!(
            out.contains("src/part0.py"),
            "{}",
            &out[..out.len().min(900)]
        );
        assert!(out.len() <= laya_rank::MAX_INJECT_CHARS);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_the_indexer_would_not_admit_is_never_read_even_when_named() {
        // Review: `{"query":"API_KEY","path":".env"}` returned the secret. A named file must pass
        // the indexer's own admission (secrets, hidden, ignored, unknown type, size).
        let root = repo("secrets");
        add(&root, ".env", "API_KEY=sk-live-0123456789\n");
        add(&root, "config/prod.env", "API_KEY=sk-prod-0123456789\n");
        add(
            &root,
            "certs/server.key",
            "-----BEGIN PRIVATE KEY-----\nMIIsecretMIIsecret\n",
        );
        add(
            &root,
            "id_rsa",
            "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk\n",
        );
        add(&root, ".gitignore", "local.py\n");
        add(&root, "local.py", "API_KEY = 'sk-local-0123456789'\n");
        for (query, path) in [
            ("API_KEY", ".env"),
            ("API_KEY", "config/prod.env"),
            ("PRIVATE", "certs/server.key"),
            ("PRIVATE", "id_rsa"),
            ("API_KEY", "local.py"),
        ] {
            let (out, _) = search_text(&Fake(true), &root, json!({"query": query, "path": path}));
            assert!(
                !out.contains("sk-") && !out.contains("MII") && !out.contains("b3Bl"),
                "{path}: {out}"
            );
            assert!(out.contains("not an indexed file"), "{path}: {out}");
        }
        let (all, _) = search_text(&Fake(true), &root, json!({"query": "API_KEY|PRIVATE"}));
        assert!(
            !all.contains("sk-") && !all.contains("PRIVATE KEY"),
            "{all}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_directory_scope_keeps_the_indexers_skip_and_ignore_rules() {
        // Review: `path:"vendor"` walked from inside vendor/, so SKIP_DIRS and gitignored
        // directories leaked.
        let root = repo("dirscope");
        add(&root, "vendor/lib.py", "x = generate_digest(1)\n");
        add(&root, "node_modules/pkg/index.js", "generate_digest(1)\n");
        add(&root, ".gitignore", "out/\n");
        add(&root, "out/gen.py", "y = generate_digest(2)\n");
        for dir in ["vendor", "node_modules", "node_modules/pkg", "out"] {
            let (text, _) = search_text(
                &Fake(true),
                &root,
                json!({"query": "generate_digest", "path": dir}),
            );
            assert!(text.contains("No match"), "{dir}: {text}");
        }
        let (all, _) = search_text(&Fake(true), &root, json!({"query": "generate_digest"}));
        assert!(
            !all.contains("vendor/")
                && !all.contains("node_modules")
                && !all.contains("out/gen.py"),
            "{all}"
        );
        let (pkg, _) = search_text(
            &Fake(true),
            &root,
            json!({"query": "generate_digest", "path": "pkg"}),
        );
        assert!(
            pkg.contains("pkg/etag.py") && !pkg.contains("tests/"),
            "{pkg}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn files_not_searched_are_counted_and_rule_out_complete() {
        // Review: unreadable and non-UTF-8 files were counted as searched, and files over 1 MiB
        // went unmentioned, so "Complete" could be false.
        let root = repo("skipped");
        let p = root.join("pkg/latin.py");
        std::fs::write(&p, b"x = generate_digest(1)  # caf\xe9\n").unwrap();
        add(&root, "pkg/huge.py", &"# generate_digest\n".repeat(70_000));
        let (out, _) = search_text(&Fake(true), &root, json!({"query": "generate_digest"}));
        assert!(!out.contains("Complete"), "{out}");
        assert!(out.contains("2 files not searched"), "{out}");
        assert!(
            out.contains("in the 5 files read"),
            "the header counts files read: {out}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn only_a_bar_makes_a_list_of_names() {
        // Review: "where is handler defined, and who calls it" became a lookup of 8 names.
        assert_eq!(
            identifier_query("where is handler defined, and who calls it"),
            None
        );
        assert_eq!(identifier_query("foo, bar"), None);
        assert_eq!(
            identifier_query("iter_text, TextChunker"),
            Some(vec!["iter_text".to_string(), "TextChunker".to_string()]),
            "code-shaped names separated by commas are still names"
        );
        assert_eq!(
            identifier_query("callers of fetch, tests of parse_url"),
            None,
            "`fetch` is a plain word"
        );
    }

    #[test]
    fn one_plain_word_without_a_match_falls_back_to_the_ranked_search() {
        // Review: "retry" got a definitive "No match" where a description search would help.
        let root = repo("plainword");
        let (out, err) = search_text(&Fake(true), &root, json!({"query": "retry"}));
        assert!(!err);
        assert!(out.contains("### src/a.rs:3-9"), "{out}");
        let (code, _) = search_text(&Fake(true), &root, json!({"query": "retry_count"}));
        assert!(
            code.contains("No match"),
            "a code-shaped name keeps the answer: {code}"
        );
        let (found, _) = search_text(&Fake(true), &root, json!({"query": "stream"}));
        assert!(
            found.contains("pkg/digest.py"),
            "a plain word with matches is a lookup: {found}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_parse_that_would_end_past_the_deadline_is_not_started() {
        let mut clock = ParseClock::new(Instant::now() + Duration::from_millis(500));
        assert!(
            clock.allows(100 << 10),
            "100 KiB at the 1 µs/byte prior fits"
        );
        assert!(!clock.allows(1 << 20), "1 MiB at 1 µs/byte does not");
        // A measured fast parse raises the estimated speed.
        clock.record(8 << 20, Duration::from_millis(400));
        assert!(clock.allows(1 << 20));
        let late = ParseClock::new(Instant::now());
        assert!(!late.allows(1));
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
            out.contains("### pkg/digest.py:1-2 — def generate_digest\n```python\ndef generate_digest(stream):\n    return stream\n```"),
            "the definition's code follows, and only its own lines: {out}"
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
