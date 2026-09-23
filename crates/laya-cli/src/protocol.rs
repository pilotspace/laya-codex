//! Wire protocol between the short-lived `laya hook`/`laya mcp` clients and the `laya daemon`.
//! One JSON object per line over a unix socket; one response line per request.

use laya_core::{QueryResult, RankedSpan};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Ping,
    /// Rank spans for a prompt. `session` scopes the working set; `budget_ms` bounds Laya time.
    /// With `render`, the daemon also sizes and renders the context (scope, calibrated P, and the
    /// session's already-sent spans) and records what it rendered as sent.
    Query {
        repo: String,
        session: Option<String>,
        prompt: String,
        budget_ms: Option<u64>,
        top_n: Option<usize>,
        #[serde(default)]
        render: Option<RenderReq>,
    },
    /// Count a Read of `path` in `session`; returns the count after incrementing.
    /// `full` = the whole file was read (no offset/limit), so all of it is in the agent's context.
    NoteRead {
        session: String,
        path: String,
        #[serde(default)]
        full: bool,
    },
    /// Last query result and working set of a session.
    /// `reset` = the agent's context was compacted or cleared: forget which spans it has.
    Session {
        session: String,
        #[serde(default)]
        reset: bool,
    },
    /// Re-index one file (after an edit). Relative or absolute path.
    ReindexFile {
        repo: String,
        path: String,
    },
    /// Incremental index of the whole repo (hash-skipped); runs in the background.
    IndexRepo {
        repo: String,
    },
    /// Plan the first whole-file Read of `path` (repo-relative or absolute): the region to show
    /// and an outline of the file. Answered with `Response::ReadPlan`; `plan: None` (or an error
    /// from an older daemon) means pass the Read through untouched.
    ReadPlan {
        repo: String,
        session: String,
        path: String,
    },
}

/// A narrowed first Read: show `limit` lines from `offset` (1-based) of a `total_lines`-line
/// file, and tell the agent what else is in it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadPlan {
    pub offset: u32,
    pub limit: u32,
    pub total_lines: u32,
    /// Why this region: `ranking` (the session's ranked spans) or `lexical` (task terms).
    pub basis: String,
    /// One line per item of the file (`* start-end label`, `*` = inside the shown region).
    pub outline: String,
}

/// Daemon-side rendering request for `Query`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderReq {
    pub budget_tokens: usize,
    /// Append the "Related by references" section.
    pub related: bool,
    /// Size adaptively (scope + calibrated P) and skip spans already sent in this session;
    /// `false` = the fixed compact format.
    pub adaptive: bool,
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
    Pong {
        model_ready: bool,
        version: String,
    },
    Query {
        result: QueryResult,
        #[serde(default)]
        rendered: Option<String>,
        /// Task scope predicted by the Laya classifier (`function`, `file`, `module`, `cross`).
        #[serde(default)]
        scope: Option<String>,
    },
    Count {
        count: u32,
    },
    Session {
        view: SessionView,
    },
    ReadPlan {
        #[serde(default)]
        plan: Option<ReadPlan>,
    },
    Ok,
    Error {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrips_as_tagged_json() {
        let r = Request::NoteRead {
            session: "s".into(),
            path: "src/a.rs".into(),
            full: true,
        };
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(
            s,
            r#"{"op":"note_read","session":"s","path":"src/a.rs","full":true}"#
        );
        assert_eq!(serde_json::from_str::<Request>(&s).unwrap(), r);
        // Older clients omit the new fields.
        let old: Request =
            serde_json::from_str(r#"{"op":"note_read","session":"s","path":"a"}"#).unwrap();
        assert_eq!(
            old,
            Request::NoteRead {
                session: "s".into(),
                path: "a".into(),
                full: false
            }
        );
        let q: Request = serde_json::from_str(r#"{"op":"query","repo":"/r","session":null,"prompt":"p","budget_ms":null,"top_n":null}"#).unwrap();
        assert!(matches!(q, Request::Query { render: None, .. }));
        let resp: Response = serde_json::from_str(r#"{"status":"query","result":{"spans":[],"mode":"lexical","elapsed_ms":1,"candidates":0}}"#).unwrap();
        assert!(matches!(
            resp,
            Response::Query {
                rendered: None,
                scope: None,
                ..
            }
        ));
    }

    #[test]
    fn read_plan_roundtrips_and_an_empty_plan_is_null() {
        let r = Request::ReadPlan {
            repo: "/r".into(),
            session: "s".into(),
            path: "src/a.rs".into(),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(
            s,
            r#"{"op":"read_plan","repo":"/r","session":"s","path":"src/a.rs"}"#
        );
        assert_eq!(serde_json::from_str::<Request>(&s).unwrap(), r);
        let p = Response::ReadPlan {
            plan: Some(ReadPlan {
                offset: 10,
                limit: 50,
                total_lines: 400,
                basis: "lexical".into(),
                outline: "  1-9 fn a".into(),
            }),
        };
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&s).unwrap(), p);
        let none: Response = serde_json::from_str(r#"{"status":"read_plan","plan":null}"#).unwrap();
        assert_eq!(none, Response::ReadPlan { plan: None });
        let bare: Response = serde_json::from_str(r#"{"status":"read_plan"}"#).unwrap();
        assert_eq!(bare, Response::ReadPlan { plan: None });
    }

    #[test]
    fn response_error_shape() {
        let s = serde_json::to_string(&Response::Error {
            message: "x".into(),
        })
        .unwrap();
        assert_eq!(s, r#"{"status":"error","message":"x"}"#);
    }
}
