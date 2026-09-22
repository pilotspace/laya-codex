//! Wire protocol between the short-lived `laya hook`/`laya mcp` clients and the `laya daemon`.
//! One JSON object per line over a unix socket; one response line per request.

use laya_core::{QueryResult, RankedSpan};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Ping,
    /// Rank spans for a prompt. `session` scopes the working set; `budget_ms` bounds Laya time.
    Query { repo: String, session: Option<String>, prompt: String, budget_ms: Option<u64>, top_n: Option<usize> },
    /// Count a Read of `path` in `session`; returns the count after incrementing.
    NoteRead { session: String, path: String },
    /// Last query result and working set of a session.
    Session { session: String },
    /// Re-index one file (after an edit). Relative or absolute path.
    ReindexFile { repo: String, path: String },
    /// Incremental index of the whole repo (hash-skipped); runs in the background.
    IndexRepo { repo: String },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionView {
    pub last: Option<QueryResult>,
    /// Spans injected or read during the session, most relevant first (deduplicated).
    pub working_set: Vec<RankedSpan>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Pong { model_ready: bool, version: String },
    Query { result: QueryResult },
    Count { count: u32 },
    Session { view: SessionView },
    Ok,
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrips_as_tagged_json() {
        let r = Request::NoteRead { session: "s".into(), path: "src/a.rs".into() };
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(s, r#"{"op":"note_read","session":"s","path":"src/a.rs"}"#);
        assert_eq!(serde_json::from_str::<Request>(&s).unwrap(), r);
    }

    #[test]
    fn response_error_shape() {
        let s = serde_json::to_string(&Response::Error { message: "x".into() }).unwrap();
        assert_eq!(s, r#"{"status":"error","message":"x"}"#);
    }
}
