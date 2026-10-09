//! Request capture: one JSON line per ranking the daemon serves (a prompt from the hook, a
//! `search` from the MCP server, a `query` from the CLI), holding what the Laya model saw and
//! decided: the task focus, every candidate in lexical order with its probability, the spans
//! returned and the blocks inlined, and where the time went (`stage_ms`: lexical candidates, the
//! model gate with each forward batch, related lists, render and total; `cached`: probabilities
//! served from the score cache). Joined with Claude Code's own session transcript (by session
//! id), it shows which offered code Claude went on to read, the evidence and training labels the
//! benchmark alone cannot give.
//!
//! On by default and local only: `$LAYA_CODEX_HOME/capture/requests.jsonl` (directory 0700, file
//! 0600), rotated to `requests.1.jsonl` past [`MAX_BYTES`]. Records hold the prompt and code
//! locations, never code text. `laya-codex capture off` (a marker file, so it also reaches the
//! daemon started by the plugin) or `LAYA_CODEX_CAPTURE=0` turns it off. Writing a record never
//! changes a reply: every error is ignored.

use std::path::{Path, PathBuf};

use laya_core::QueryResult;
use laya_rank::CandidateCapture;
use serde_json::{Value, json};

use crate::trace::Tracer;

pub const ENV: &str = "LAYA_CODEX_CAPTURE";
/// Size at which `requests.jsonl` is moved to `requests.1.jsonl` (the previous one is replaced).
pub const MAX_BYTES: u64 = 32 << 20;
/// Prompts are cut to this many characters in a record.
pub const PROMPT_CHARS: usize = 4000;

pub fn dir(home: &Path) -> PathBuf {
    home.join("capture")
}

pub fn off_marker(home: &Path) -> PathBuf {
    dir(home).join("disabled")
}

pub fn default_file(home: &Path) -> PathBuf {
    dir(home).join("requests.jsonl")
}

/// Where to record, or `None` when capture is off. `env` is `LAYA_CODEX_CAPTURE`: `0`/`off`/
/// `false` disables; `1`/`on`/`true` records to the default file even past `capture off`;
/// another value is the file to write; unset or empty records unless `capture off` was run.
pub fn resolve(home: &Path, env: Option<&str>) -> Option<Tracer> {
    let path = match env.map(str::trim) {
        Some("0" | "off" | "false") => return None,
        Some("1" | "on" | "true") => default_file(home),
        None | Some("") if off_marker(home).exists() => return None,
        None | Some("") => default_file(home),
        Some(p) => PathBuf::from(p),
    };
    Some(Tracer::to_file(path, MAX_BYTES))
}

pub fn from_env(home: &Path) -> Option<Tracer> {
    resolve(home, std::env::var(ENV).ok().as_deref())
}

/// The daemon's capture: resolved again at every record (one `stat` of the off marker), so
/// `laya-codex capture off` stops recording at once, without restarting the daemon.
#[derive(Debug, Clone)]
pub struct Capture {
    home: PathBuf,
    env: Option<String>,
}

impl Capture {
    pub fn new(home: PathBuf, env: Option<String>) -> Self {
        Capture { home, env }
    }

    pub fn from_env(home: &Path) -> Self {
        Capture::new(home.to_path_buf(), std::env::var(ENV).ok())
    }

    /// Where records go right now, or `None` when capture is off.
    pub fn target(&self) -> Option<Tracer> {
        resolve(&self.home, self.env.as_deref())
    }

    pub fn record(&self, entry: &Value) {
        if let Some(t) = self.target() {
            t.record(entry);
        }
    }
}

pub fn cmd_off(home: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir(home))?;
    std::fs::write(off_marker(home), "")?;
    match from_env(home) {
        Some(t) => println!("capture still on through {ENV} (file {})", t.path.display()),
        None => println!(
            "request capture off (recorded requests are kept in {}; delete the folder to remove them)",
            dir(home).display()
        ),
    }
    Ok(())
}

pub fn cmd_on(home: &Path) -> anyhow::Result<()> {
    match std::fs::remove_file(off_marker(home)) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    match from_env(home) {
        Some(t) => println!(
            "request capture on: each ranking is recorded in {}",
            t.path.display()
        ),
        None => println!("capture marked on, but {ENV}=0 in this environment turns it off"),
    }
    Ok(())
}

pub fn cmd_status(home: &Path) -> anyhow::Result<()> {
    let t = from_env(home);
    let path = t
        .as_ref()
        .map(|t| t.path.clone())
        .unwrap_or_else(|| default_file(home));
    let lines = |p: &Path| {
        std::fs::read_to_string(p)
            .map(|s| s.lines().count())
            .unwrap_or(0)
    };
    let n = lines(&path) + lines(&path.with_extension("1.jsonl"));
    match &t {
        Some(_) => println!(
            "request capture on ({n} requests recorded in {})",
            path.display()
        ),
        None => println!(
            "request capture off ({n} requests kept in {})",
            path.display()
        ),
    }
    Ok(())
}

/// One ranking, as the daemon served it.
pub struct Request<'a> {
    /// `hook` (a prompt, rendered for injection), `direct` (no session: the MCP `search` tool or
    /// `laya-codex query`) or `query` (a session without rendering).
    pub source: &'a str,
    pub session: Option<&'a str>,
    pub repo: &'a Path,
    pub prompt: &'a str,
    /// What was ranked: the prompt, or the session topic a follow-up was retrieved as.
    pub query: &'a str,
    pub follow_up: bool,
    pub result: &'a QueryResult,
    pub capture: &'a CandidateCapture,
    /// Spans inlined as full code, `(path, start, end)`.
    pub inlined: &'a [(String, u32, u32)],
    pub rendered_chars: Option<usize>,
    /// Milliseconds spent rendering the injection (0 when nothing was rendered).
    pub render_ms: f64,
    /// Milliseconds from the request's arrival to its reply being ready (before this record).
    pub total_ms: f64,
}

pub fn entry(r: &Request) -> Value {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let prompt: String = r.prompt.chars().take(PROMPT_CHARS).collect();
    let stages = &r.capture.stages;
    json!({
        "v": 1,
        "ts": ts,
        "source": r.source,
        "session": r.session,
        "repo": r.repo.display().to_string(),
        "prompt": prompt,
        "query": (r.query != r.prompt).then_some(r.query),
        "follow_up": r.follow_up,
        "focus": r.capture.focus,
        "mode": r.result.mode,
        "elapsed_ms": r.result.elapsed_ms,
        "scored": r.result.scored,
        "offered": r.result.offered,
        "candidates": r.capture.candidates,
        "spans": r.result.spans.iter().map(|s| json!({
            "path": s.path, "start": s.start_line, "end": s.end_line,
            "p": s.p_relevant, "score": s.score,
        })).collect::<Vec<_>>(),
        "inlined": r.inlined,
        "rendered_chars": r.rendered_chars,
        "cached": stages.cached,
        "stage_ms": {
            "lexical": stages.lexical_ms,
            "model": stages.model_ms,
            "batches": stages.batch_ms,
            "related": stages.related_ms,
            "render": r.render_ms,
            "total": r.total_ms,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use laya_core::{RankMode, RankedSpan};
    use laya_rank::CapturedCandidate;

    fn home(tag: &str) -> PathBuf {
        let h = std::env::temp_dir().join(format!("laya-capture-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&h);
        std::fs::create_dir_all(&h).unwrap();
        h
    }

    #[test]
    fn on_by_default_and_the_env_or_capture_off_turns_it_off() {
        let h = home("resolve");
        assert_eq!(resolve(&h, None).unwrap().path, default_file(&h));
        assert!(resolve(&h, Some("0")).is_none());
        std::fs::create_dir_all(dir(&h)).unwrap();
        std::fs::write(off_marker(&h), "").unwrap();
        assert!(
            resolve(&h, None).is_none(),
            "capture off wins over the default"
        );
        assert!(
            resolve(&h, Some("1")).is_some(),
            "the env wins over the marker"
        );
        assert_eq!(
            resolve(&h, Some("/tmp/x.jsonl")).unwrap().path,
            PathBuf::from("/tmp/x.jsonl")
        );
    }

    #[test]
    fn capture_off_takes_effect_without_restarting_the_daemon() {
        let h = home("live");
        let c = Capture::new(h.clone(), None);
        c.record(&json!({"n": 1}));
        cmd_off(&h).unwrap();
        c.record(&json!({"n": 2}));
        cmd_on(&h).unwrap();
        c.record(&json!({"n": 3}));
        let lines = std::fs::read_to_string(default_file(&h)).unwrap();
        assert_eq!(lines.lines().count(), 2, "{lines}");
        assert!(!lines.contains("\"n\":2"));
    }

    #[test]
    fn a_record_holds_every_candidate_the_inlined_blocks_and_no_code() {
        let result = QueryResult {
            spans: vec![RankedSpan {
                path: "src/b.rs".into(),
                start_line: 1,
                end_line: 3,
                symbol: "fn b".into(),
                p_relevant: Some(0.9),
                score: 0.7,
                text: "fn b() { secret_code() }".into(),
            }],
            mode: RankMode::Laya,
            elapsed_ms: 12,
            candidates: 2,
            scored: 1,
            offered: 2,
            related: Vec::new(),
        };
        let cand = |path: &str, rank, p| CapturedCandidate {
            chunk_id: format!("id-{path}"),
            path: path.into(),
            start_line: 1,
            end_line: 3,
            lexical_rank: rank,
            fused: 0.5,
            p,
        };
        let capture = CandidateCapture {
            focus: "alpha beta".into(),
            candidates: vec![cand("src/a.rs", 0, None), cand("src/b.rs", 1, Some(0.9))],
            stages: laya_rank::StageTimes {
                lexical_ms: 1.25,
                model_ms: 260.5,
                batch_ms: vec![130.0, 129.5],
                cached: 3,
                related_ms: 2.0,
            },
        };
        let long = "x".repeat(PROMPT_CHARS + 50);
        let inlined = vec![("src/b.rs".to_string(), 1, 3)];
        let e = entry(&Request {
            source: "hook",
            session: Some("s1"),
            repo: Path::new("/repo"),
            prompt: &long,
            query: &long,
            follow_up: false,
            result: &result,
            capture: &capture,
            inlined: &inlined,
            rendered_chars: Some(120),
            render_ms: 0.75,
            total_ms: 265.0,
        });
        assert_eq!(
            e["stage_ms"],
            json!({"lexical": 1.25, "model": 260.5, "batches": [130.0, 129.5],
                   "related": 2.0, "render": 0.75, "total": 265.0})
        );
        assert_eq!(e["cached"], 3);
        assert_eq!(e["candidates"].as_array().unwrap().len(), 2);
        assert_eq!(e["candidates"][0]["p"], Value::Null);
        assert_eq!(e["candidates"][1]["lexical_rank"], 1);
        assert_eq!(e["inlined"][0][0], "src/b.rs");
        assert_eq!(e["session"], "s1");
        assert_eq!(
            e["query"],
            Value::Null,
            "the query is kept only when it differs"
        );
        assert_eq!(e["prompt"].as_str().unwrap().len(), PROMPT_CHARS);
        assert!(
            !e.to_string().contains("secret_code"),
            "records never hold code text"
        );
    }
}
