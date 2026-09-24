//! Opt-in trace of what Claude Code and laya-codex actually exchange, for debugging: every hook
//! call (the JSON Claude Code sent on stdin and the JSON laya-codex printed), every MCP message
//! and reply, and every daemon call made while handling them (request, response, time).
//!
//! Off by default: a trace holds prompts and code. `laya-codex trace on` (a marker file, so it
//! also reaches hooks run by the plugin) or `LAYA_CODEX_TRACE=1` / `=<file>` turns it on;
//! `LAYA_CODEX_TRACE=0` forces it off. Entries are JSON lines in
//! `$LAYA_CODEX_HOME/trace/trace.jsonl` (directory 0700, file 0600), rotated to `trace.1.jsonl`
//! past [`MAX_BYTES`]. Tracing never changes what a hook or the MCP server returns: every error
//! while writing a trace is ignored.

use std::cell::RefCell;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{Value, json};

use crate::hook::DaemonApi;
use crate::protocol::{Request, Response};

pub const ENV: &str = "LAYA_CODEX_TRACE";
/// Size at which `trace.jsonl` is moved to `trace.1.jsonl` (the previous one is replaced).
pub const MAX_BYTES: u64 = 64 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `LAYA_CODEX_TRACE` is set.
    Env,
    /// `laya-codex trace on` wrote the marker file.
    Marker,
}

#[derive(Debug)]
pub struct Tracer {
    pub path: PathBuf,
    pub source: Source,
    max_bytes: u64,
}

pub fn dir(home: &Path) -> PathBuf {
    home.join("trace")
}

pub fn marker(home: &Path) -> PathBuf {
    dir(home).join("enabled")
}

pub fn default_file(home: &Path) -> PathBuf {
    dir(home).join("trace.jsonl")
}

fn rotated(path: &Path) -> PathBuf {
    path.with_extension("1.jsonl")
}

impl Tracer {
    /// Whether and where to trace. `env` is `LAYA_CODEX_TRACE`: unset leaves it to the marker
    /// file; `0`/`off`/`false`/empty disables; `1`/`on`/`true` uses the default file; anything
    /// else is the file to write.
    pub fn resolve(home: &Path, env: Option<&str>) -> Option<Tracer> {
        let (path, source) = match env.map(str::trim) {
            Some("0" | "off" | "false" | "") => return None,
            Some("1" | "on" | "true") => (default_file(home), Source::Env),
            Some(p) => (PathBuf::from(p), Source::Env),
            None if marker(home).exists() => (default_file(home), Source::Marker),
            None => return None,
        };
        Some(Tracer {
            path,
            source,
            max_bytes: MAX_BYTES,
        })
    }

    pub fn from_env(home: &Path) -> Option<Tracer> {
        Tracer::resolve(home, std::env::var(ENV).ok().as_deref())
    }

    /// Append `entry` as one JSON line (one write, so concurrent hooks don't interleave).
    pub fn record(&self, entry: &Value) {
        let _ = self.try_record(entry);
    }

    fn try_record(&self, entry: &Value) -> std::io::Result<()> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        if let Some(parent) = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty() && !p.exists())
        {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
        }
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() >= self.max_bytes) {
            std::fs::rename(&self.path, rotated(&self.path))?;
        }
        let mut line = serde_json::to_vec(entry)?;
        line.push(b'\n');
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)?
            .write_all(&line)
    }
}

/// A [`DaemonApi`] that remembers every call it forwards: request, response (or error) and time.
pub struct Recording<'a> {
    inner: &'a dyn DaemonApi,
    calls: RefCell<Vec<Value>>,
}

impl<'a> Recording<'a> {
    pub fn new(inner: &'a dyn DaemonApi) -> Self {
        Recording {
            inner,
            calls: RefCell::new(Vec::new()),
        }
    }

    pub fn take(&self) -> Vec<Value> {
        std::mem::take(&mut self.calls.borrow_mut())
    }
}

impl DaemonApi for Recording<'_> {
    fn call(&self, req: Request) -> anyhow::Result<Response> {
        let t0 = Instant::now();
        let request = serde_json::to_value(&req).unwrap_or(Value::Null);
        let r = self.inner.call(req);
        let mut call = json!({"request": request, "elapsed_ms": t0.elapsed().as_millis() as u64});
        match &r {
            Ok(resp) => call["response"] = serde_json::to_value(resp).unwrap_or(Value::Null),
            Err(e) => call["error"] = json!(e.to_string()),
        }
        self.calls.borrow_mut().push(call);
        r
    }
}

/// A hook exchange: what Claude Code sent, what laya-codex printed (`None` = nothing, the hook
/// stayed out of the way), the handler's action and the daemon calls behind it.
pub fn hook_entry(
    input: &Value,
    output: Option<&Value>,
    action: &str,
    elapsed_ms: u64,
    daemon: Vec<Value>,
) -> Value {
    let mut e = base("hook", elapsed_ms, daemon);
    for key in ["session_id", "cwd"] {
        e[key] = input[key].clone();
    }
    e["event"] = input["hook_event_name"].clone();
    e["tool"] = input["tool_name"].clone();
    e["action"] = json!(action);
    e["request"] = input.clone();
    e["response"] = output.cloned().unwrap_or(Value::Null);
    e
}

fn base(kind: &str, elapsed_ms: u64, daemon: Vec<Value>) -> Value {
    let ts_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    json!({"ts_ms": ts_ms, "kind": kind, "pid": std::process::id(),
        "version": env!("CARGO_PKG_VERSION"), "elapsed_ms": elapsed_ms, "daemon": daemon})
}

/// An MCP exchange: one JSON-RPC message from Claude Code and the reply (`None` for
/// notifications).
pub fn mcp_entry(
    request: &Value,
    response: Option<&Value>,
    elapsed_ms: u64,
    daemon: Vec<Value>,
) -> Value {
    let mut e = base("mcp", elapsed_ms, daemon);
    e["method"] = request["method"].clone();
    e["request"] = request.clone();
    e["response"] = response.cloned().unwrap_or(Value::Null);
    e
}

/// The last `last` entries of the trace at `path` (and its rotated predecessor), oldest first,
/// optionally only those of `session` (prefix match). Unparseable lines are skipped.
pub fn read_entries(path: &Path, session: Option<&str>, last: usize) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for file in [rotated(path), path.to_path_buf()] {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if session.is_none_or_prefix(&v) {
                out.push(v);
            }
        }
    }
    let skip = out.len().saturating_sub(last);
    out.split_off(skip)
}

/// Human-readable view of one entry: a header line, then what was asked, what the daemon did and
/// what went back. `full` appends the complete request and response.
pub fn summarize(entry: &Value, full: bool) -> String {
    let mut s = String::new();
    let what = match entry["kind"].as_str() {
        Some("mcp") => format!("mcp {}", text(&entry["method"])),
        _ => format!("hook {}", text(&entry["event"])),
    };
    let mut head = format!("{}  {what}", utc(entry["ts_ms"].as_u64().unwrap_or(0)));
    if let Some(a) = entry["action"].as_str() {
        head.push_str(&format!("  {a}"));
    }
    head.push_str(&format!(
        "  {} ms",
        entry["elapsed_ms"].as_u64().unwrap_or(0)
    ));
    if let Some(id) = entry["session_id"].as_str() {
        head.push_str(&format!("  session {}", &id[..id.len().min(8)]));
    }
    s.push_str(&head);
    s.push('\n');

    let req = &entry["request"];
    let asked = if entry["kind"] == "mcp" {
        one_line(&req["params"])
    } else if let Some(p) = req["prompt"].as_str() {
        p.to_string()
    } else if !req["tool_name"].is_null() {
        format!(
            "{} {}",
            text(&req["tool_name"]),
            one_line(&req["tool_input"])
        )
    } else {
        one_line(&req["source"])
    };
    if !asked.is_empty() && asked != "null" {
        line(&mut s, "asked", &clip(&asked, 200));
    }
    for call in entry["daemon"].as_array().into_iter().flatten() {
        daemon_lines(&mut s, call);
    }
    line(&mut s, "returned", &returned(&entry["response"]));

    if full {
        s.push_str("  ---- request\n");
        s.push_str(&indent(&pretty(req)));
        let ctx = entry["response"]["hookSpecificOutput"]["additionalContext"].as_str();
        s.push_str("  ---- response\n");
        match ctx {
            Some(c) => {
                let mut shown = entry["response"].clone();
                shown["hookSpecificOutput"]["additionalContext"] = json!("(below)");
                s.push_str(&indent(&pretty(&shown)));
                s.push_str("  ---- additionalContext\n");
                s.push_str(&indent(c));
            }
            None => s.push_str(&indent(&pretty(&entry["response"]))),
        }
    }
    s
}

fn line(s: &mut String, label: &str, value: &str) {
    s.push_str(&format!("  {label:<9} {value}\n"));
}

fn text(v: &Value) -> String {
    v.as_str()
        .map(str::to_string)
        .unwrap_or_else(|| v.to_string())
}

fn one_line(v: &Value) -> String {
    if v.is_null() { String::new() } else { text(v) }
}

fn clip(s: &str, n: usize) -> String {
    let flat = s.replace('\n', " ");
    match flat.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &flat[..i]),
        None => flat,
    }
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

fn indent(t: &str) -> String {
    t.lines().map(|l| format!("    {l}\n")).collect()
}

fn code_blocks(t: &str) -> usize {
    t.lines().filter(|l| l.starts_with("### ")).count()
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// One line per daemon call, plus the top ranked spans of a query.
fn daemon_lines(s: &mut String, call: &Value) {
    let op = text(&call["request"]["op"]);
    let ms = call["elapsed_ms"].as_u64().unwrap_or(0);
    let r = &call["response"];
    let detail = if let Some(e) = call["error"].as_str() {
        format!("error: {e}")
    } else {
        match r["status"].as_str() {
            Some("query") => {
                let res = &r["result"];
                let n = res["spans"].as_array().map_or(0, Vec::len);
                let mut d = format!("{n} spans ({})", text(&res["mode"]));
                if let Some(sc) = r["scope"].as_str() {
                    d.push_str(&format!(", scope {sc}"));
                }
                if let Some(t) = r["rendered"].as_str() {
                    d.push_str(&format!(", rendered {} chars", t.len()));
                }
                d
            }
            Some("read_plan") => match &r["plan"] {
                Value::Null => "no plan: Read passes through".into(),
                p => format!(
                    "lines {}-{} of {} ({})",
                    p["offset"],
                    p["offset"].as_u64().unwrap_or(0) + p["limit"].as_u64().unwrap_or(0).max(1) - 1,
                    p["total_lines"],
                    text(&p["basis"])
                ),
            },
            Some("error") => format!("error: {}", text(&r["message"])),
            Some("count") => format!("count {}", r["count"]),
            Some(other) => other.to_string(),
            None => String::new(),
        }
    };
    line(s, "daemon", &format!("{op} {ms} ms: {detail}"));
    for sp in r["result"]["spans"]
        .as_array()
        .into_iter()
        .flatten()
        .take(5)
    {
        let p = sp["p_relevant"]
            .as_f64()
            .map(|p| format!(" p={p:.2}"))
            .unwrap_or_default();
        line(
            s,
            "",
            &format!(
                "  {}:{}-{} {}{p}",
                text(&sp["path"]),
                sp["start_line"],
                sp["end_line"],
                text(&sp["symbol"])
            ),
        );
    }
}

/// What went back to Claude Code, in one line.
fn returned(resp: &Value) -> String {
    if resp.is_null() {
        return "nothing (Claude Code proceeds unchanged)".into();
    }
    if let Some(out) = resp.get("hookSpecificOutput") {
        let mut parts = Vec::new();
        if let Some(c) = out["additionalContext"].as_str() {
            parts.push(format!(
                "additionalContext {} chars, {}",
                c.len(),
                plural(code_blocks(c), "code block")
            ));
        }
        if !out["updatedInput"].is_null() {
            parts.push(format!("updatedInput {}", one_line(&out["updatedInput"])));
        }
        if let Some(d) = out["permissionDecision"].as_str() {
            parts.push(format!("permissionDecision {d}"));
        }
        return parts.join("; ");
    }
    if let Some(e) = resp.get("error") {
        return format!("error {}", one_line(e));
    }
    let content = &resp["result"]["content"];
    if let Some(items) = content.as_array() {
        let t: String = items.iter().filter_map(|i| i["text"].as_str()).collect();
        return format!(
            "{} chars, {}{}",
            t.len(),
            plural(code_blocks(&t), "code block"),
            if resp["result"]["isError"] == true {
                " (isError)"
            } else {
                ""
            }
        );
    }
    clip(&one_line(resp), 200)
}

/// `YYYY-MM-DD HH:MM:SS.mmmZ` for a Unix time in milliseconds (UTC, proleptic Gregorian).
fn utc(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        ms % 1000
    )
}

// ---- `laya-codex trace ...` ----

pub fn cmd_on(home: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir(home))?;
    std::fs::write(marker(home), "")?;
    let t = Tracer::from_env(home);
    match &t {
        Some(t) => println!(
            "tracing on: every hook call and MCP message is recorded in {}",
            t.path.display()
        ),
        None => println!("tracing marked on, but {ENV}=0 in this environment turns it off"),
    }
    println!("the trace contains your prompts and code; stays on this machine");
    println!("view: laya-codex trace show [--full] [--follow]   stop: laya-codex trace off");
    Ok(())
}

pub fn cmd_off(home: &Path) -> anyhow::Result<()> {
    match std::fs::remove_file(marker(home)) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    match Tracer::from_env(home) {
        Some(t) => println!("tracing still on through {ENV} (file {})", t.path.display()),
        None => println!("tracing off (the trace file is kept; laya-codex trace clear deletes it)"),
    }
    Ok(())
}

pub fn cmd_status(home: &Path) -> anyhow::Result<()> {
    let t = Tracer::from_env(home);
    let path = t
        .as_ref()
        .map(|t| t.path.clone())
        .unwrap_or_else(|| default_file(home));
    match &t {
        Some(t) if t.source == Source::Env => println!("tracing on ({ENV})"),
        Some(_) => println!("tracing on (laya-codex trace on)"),
        None => println!("tracing off"),
    }
    let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let bytes = size(&path) + size(&rotated(&path));
    let n = read_entries(&path, None, usize::MAX).len();
    println!(
        "file: {} ({n} entries, {:.1} MiB)",
        path.display(),
        bytes as f64 / (1 << 20) as f64
    );
    Ok(())
}

pub fn cmd_show(
    home: &Path,
    session: Option<&str>,
    last: usize,
    full: bool,
    json: bool,
    follow: bool,
) -> anyhow::Result<()> {
    let path = Tracer::from_env(home)
        .map(|t| t.path)
        .unwrap_or_else(|| default_file(home));
    let print = |e: &Value| {
        if json {
            println!("{e}");
        } else {
            println!("{}", summarize(e, full));
        }
    };
    let entries = read_entries(&path, session, last);
    if entries.is_empty() && !follow {
        match session {
            Some(sid) => println!("no trace entries for session {sid} in {}", path.display()),
            None => println!(
                "no trace entries in {} (turn tracing on with: laya-codex trace on)",
                path.display()
            ),
        }
    }
    entries.iter().for_each(print);
    if !follow {
        return Ok(());
    }
    // Follow: print lines appended after what was shown; start over if the file was rotated.
    let mut offset = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if (bytes.len() as u64) < offset {
            offset = 0;
        }
        let new = &bytes[offset as usize..];
        let Some(end) = new.iter().rposition(|&b| b == b'\n') else {
            continue;
        };
        for line in String::from_utf8_lossy(&new[..=end]).lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let wanted = session.is_none_or_prefix(&v);
            if wanted {
                print(&v);
            }
        }
        offset += end as u64 + 1;
    }
}

trait SessionFilter {
    fn is_none_or_prefix(&self, v: &Value) -> bool;
}

impl SessionFilter for Option<&str> {
    fn is_none_or_prefix(&self, v: &Value) -> bool {
        match self {
            Some(s) => v["session_id"].as_str().is_some_and(|id| id.starts_with(s)),
            None => true,
        }
    }
}

pub fn cmd_clear(home: &Path) -> anyhow::Result<()> {
    let path = Tracer::from_env(home)
        .map(|t| t.path)
        .unwrap_or_else(|| default_file(home));
    for p in [path.clone(), rotated(&path)] {
        match std::fs::remove_file(&p) {
            Ok(()) => println!("deleted {}", p.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("laya-trace-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn tracing_is_off_unless_asked_for_and_the_env_wins_over_the_marker() {
        let home = scratch("resolve");
        assert!(Tracer::resolve(&home, None).is_none(), "off by default");
        let t = Tracer::resolve(&home, Some("1")).unwrap();
        assert_eq!((t.path, t.source), (default_file(&home), Source::Env));
        let t = Tracer::resolve(&home, Some("/tmp/x.jsonl")).unwrap();
        assert_eq!(t.path, PathBuf::from("/tmp/x.jsonl"));

        std::fs::create_dir_all(dir(&home)).unwrap();
        std::fs::write(marker(&home), "").unwrap();
        let t = Tracer::resolve(&home, None).unwrap();
        assert_eq!((t.path, t.source), (default_file(&home), Source::Marker));
        for off in ["0", "off", "false", ""] {
            assert!(Tracer::resolve(&home, Some(off)).is_none(), "{off:?}");
        }
    }

    #[test]
    fn record_appends_private_json_lines_and_rotates() {
        let home = scratch("record");
        let mut t = Tracer::resolve(&home, Some("on")).unwrap();
        t.record(&json!({"n": 1}));
        t.record(&json!({"n": 2}));
        let text = std::fs::read_to_string(&t.path).unwrap();
        assert_eq!(text, "{\"n\":1}\n{\"n\":2}\n");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir(&home)), 0o700);
        assert_eq!(mode(&t.path), 0o600);

        t.max_bytes = 10;
        t.record(&json!({"n": 3}));
        assert_eq!(
            std::fs::read_to_string(rotated(&t.path)).unwrap(),
            text,
            "the full file moved aside"
        );
        assert_eq!(std::fs::read_to_string(&t.path).unwrap(), "{\"n\":3}\n");
        let all = read_entries(&t.path, None, 10);
        let ns: Vec<i64> = all.iter().map(|e| e["n"].as_i64().unwrap()).collect();
        assert_eq!(ns, vec![1, 2, 3], "reads span the rotation, oldest first");
    }

    #[test]
    fn a_trace_that_cannot_be_written_is_ignored() {
        let t = Tracer::resolve(Path::new("/"), Some("/proc/nonexistent/dir/t.jsonl")).unwrap();
        t.record(&json!({"n": 1})); // must not panic
    }

    struct Fixed;
    impl DaemonApi for Fixed {
        fn call(&self, req: Request) -> anyhow::Result<Response> {
            match req {
                Request::Ping => Ok(Response::Ok),
                _ => anyhow::bail!("daemon unavailable"),
            }
        }
    }

    #[test]
    fn recording_keeps_requests_responses_and_errors() {
        let rec = Recording::new(&Fixed);
        assert!(rec.call(Request::Ping).is_ok());
        assert!(rec.call(Request::IndexRepo { repo: "/r".into() }).is_err());
        let calls = rec.take();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["request"]["op"], "ping");
        assert_eq!(calls[0]["response"]["status"], "ok");
        assert!(calls[0]["elapsed_ms"].is_u64());
        assert_eq!(calls[1]["request"]["op"], "index_repo");
        assert_eq!(calls[1]["error"], "daemon unavailable");
        assert!(rec.take().is_empty(), "take drains");
    }

    fn prompt_entry(session: &str) -> Value {
        let input = json!({"hook_event_name": "UserPromptSubmit", "session_id": session,
            "cwd": "/r", "prompt": "where is the WAL replayed"});
        let ctx =
            "<!-- laya-codex -->\n### src/wal.rs:1-9 — fn replay\n```rust\nfn replay() {}\n```\n";
        let output = json!({"hookSpecificOutput": {"hookEventName": "UserPromptSubmit",
            "additionalContext": ctx}});
        let daemon = vec![json!({
            "request": {"op": "query", "prompt": "where is the WAL replayed"},
            "response": {"status": "query", "scope": "file", "rendered": ctx,
                "result": {"mode": "laya", "elapsed_ms": 812, "candidates": 24,
                    "spans": [{"path": "src/wal.rs", "start_line": 1, "end_line": 9,
                        "symbol": "fn replay", "p_relevant": 0.83, "score": 0.9, "text": ""}]}},
            "elapsed_ms": 830})];
        hook_entry(&input, Some(&output), "inject", 845, daemon)
    }

    #[test]
    fn hook_entries_keep_the_exchange_verbatim() {
        let e = prompt_entry("3214aec2-bf09");
        assert_eq!(e["kind"], "hook");
        assert_eq!(e["event"], "UserPromptSubmit");
        assert_eq!(e["session_id"], "3214aec2-bf09");
        assert_eq!(e["request"]["prompt"], "where is the WAL replayed");
        assert!(
            e["response"]["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap()
                .contains("fn replay")
        );
        assert_eq!(
            (e["action"].as_str(), e["elapsed_ms"].as_u64()),
            (Some("inject"), Some(845))
        );
        assert_eq!(e["daemon"][0]["request"]["op"], "query");
        assert!(e["ts_ms"].as_u64().unwrap() > 1_700_000_000_000);
        assert_eq!(e["version"], env!("CARGO_PKG_VERSION"));

        let skipped = hook_entry(&json!({"hook_event_name": "Stop"}), None, "skip", 1, vec![]);
        assert!(skipped["response"].is_null());
    }

    #[test]
    fn mcp_entries_keep_message_and_reply() {
        let req = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "search", "arguments": {"query": "wal replay"}}});
        let reply = json!({"jsonrpc": "2.0", "id": 3, "result": {"content": [{"type": "text", "text": "### a.rs:1-2"}]}});
        let e = mcp_entry(&req, Some(&reply), 40, vec![]);
        assert_eq!(e["kind"], "mcp");
        assert_eq!(e["request"]["method"], "tools/call");
        assert_eq!(e["response"]["id"], 3);
    }

    #[test]
    fn read_entries_filters_by_session_prefix_and_keeps_the_last_n() {
        let home = scratch("read");
        let t = Tracer::resolve(&home, Some("on")).unwrap();
        for s in ["aaa1", "bbb2", "aaa3", "aaa4"] {
            t.record(&prompt_entry(s));
        }
        std::fs::OpenOptions::new()
            .append(true)
            .open(&t.path)
            .map(|mut f| std::io::Write::write_all(&mut f, b"not json\n"))
            .unwrap()
            .unwrap();
        let ids = |v: Vec<Value>| -> Vec<String> {
            v.iter()
                .map(|e| e["session_id"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(
            ids(read_entries(&t.path, Some("aaa"), 2)),
            vec!["aaa3", "aaa4"]
        );
        assert_eq!(ids(read_entries(&t.path, None, 10)).len(), 4);
        assert!(read_entries(&home.join("missing.jsonl"), None, 10).is_empty());
    }

    #[test]
    fn utc_formats_unix_milliseconds() {
        assert_eq!(utc(0), "1970-01-01 00:00:00.000Z");
        assert_eq!(utc(1_790_163_592_331), "2026-09-23 11:39:52.331Z");
        assert_eq!(utc(951_782_400_000), "2000-02-29 00:00:00.000Z");
    }

    #[test]
    fn summary_says_what_was_asked_what_the_daemon_did_and_what_went_back() {
        let s = summarize(&prompt_entry("3214aec2-bf09"), false);
        let head = s.lines().next().unwrap();
        assert!(head.contains("hook UserPromptSubmit"), "{s}");
        assert!(head.contains("inject") && head.contains("845 ms"), "{s}");
        assert!(head.contains("3214aec2"), "{s}");
        assert!(s.contains("where is the WAL replayed"), "{s}");
        assert!(
            s.contains("query")
                && s.contains("830 ms")
                && s.contains("1 spans")
                && s.contains("laya"),
            "{s}"
        );
        assert!(s.contains("scope file"), "{s}");
        assert!(s.contains("src/wal.rs:1-9") && s.contains("p=0.83"), "{s}");
        assert!(s.contains("1 code block"), "{s}");
        assert!(
            !s.contains("fn replay() {}"),
            "full text only with --full: {s}"
        );
        let full = summarize(&prompt_entry("x"), true);
        assert!(full.contains("fn replay() {}"), "{full}");
    }
}
