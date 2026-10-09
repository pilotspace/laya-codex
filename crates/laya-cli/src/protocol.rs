//! Wire protocol between the short-lived `laya-codex hook`/`laya-codex mcp` clients and the `laya-codex daemon`.
//! One JSON object per line over a unix socket; one response line per request.
//! The daemon bounds the connection (see `daemon::Limits`): at most 64 at once, request lines
//! up to 1 MiB, 30 s idle and 10 s per reply write; size fields are clamped (constants below).

use laya_core::{QueryResult, RankedSpan};
use serde::{Deserialize, Serialize};

/// Bounds the daemon applies to `Request::Query` size parameters, whatever the client sent
/// (MCP `search` documents the same `top_n` range).
pub const MAX_TOP_N: usize = 20;
/// Upper bound on `budget_ms` (the Laya time budget of one query).
pub const MAX_BUDGET_MS: u64 = 60_000;
/// Upper bound on `RenderReq::budget_tokens` (the hook asks for a few thousand).
pub const MAX_RENDER_TOKENS: usize = 32_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Ping,
    /// Rank spans for a prompt. `session` scopes the working set; `budget_ms` bounds Laya time.
    /// With `render`, the daemon also sizes and renders the context (top-ranked files in full,
    /// minus the session's already-sent spans) and records what it rendered as sent.
    Query {
        repo: String,
        session: Option<String>,
        prompt: String,
        budget_ms: Option<u64>,
        top_n: Option<usize>,
        #[serde(default)]
        render: Option<RenderReq>,
        /// Namespaces the daemon's probability cache: a query reads only probabilities cached
        /// under the same salt (`None` = the shared cache). Benchmarks set one per session
        /// (`LAYA_CODEX_MEMO_SALT`), so a follow-up reuses its own session's scores but no run
        /// reads another's. Left off the wire when unset.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        memo_salt: Option<String>,
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
    /// Ask the daemon to exit (`laya-codex stop`). Answered with `Response::Ok` before it exits; the
    /// Moon it supervises keeps running. Older daemons answer "bad request".
    Shutdown,
}

impl Request {
    /// This request with `salt` as a query's memo salt; other requests, and a `None` salt, leave
    /// it as it is.
    pub fn with_memo_salt(mut self, salt: Option<&str>) -> Self {
        if let (Request::Query { memo_salt, .. }, Some(salt)) = (&mut self, salt) {
            *memo_salt = Some(salt.to_string());
        }
        self
    }
}

/// Daemon-side rendering request for `Query`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderReq {
    pub budget_tokens: usize,
    /// Append the "Related by references" section.
    pub related: bool,
    /// Size adaptively (by rank, smaller for follow-ups) and skip spans already sent in this
    /// session; `false` = the fixed compact format.
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
        /// The model directory the daemon serves (absent from older daemons and when it serves
        /// no model).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    Query {
        result: QueryResult,
        #[serde(default)]
        rendered: Option<String>,
        /// Always `None`: the task-scope classifier that set it was removed. The field stays so
        /// replies keep the shape older clients and logs expect.
        #[serde(default)]
        scope: Option<String>,
    },
    Count {
        count: u32,
    },
    Session {
        view: SessionView,
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
        // Older hooks still send `read_plan`: it is refused, and they fail open on the error.
        assert!(
            serde_json::from_str::<Request>(
                r#"{"op":"read_plan","repo":"/r","session":"s","path":"a"}"#
            )
            .is_err()
        );
    }

    fn query(memo_salt: Option<&str>) -> Request {
        Request::Query {
            repo: "/r".into(),
            session: Some("s".into()),
            prompt: "p".into(),
            budget_ms: None,
            top_n: None,
            render: None,
            memo_salt: memo_salt.map(str::to_string),
        }
    }

    #[test]
    fn a_query_without_a_memo_salt_keeps_its_wire_form() {
        let s = serde_json::to_string(&query(None)).unwrap();
        assert_eq!(
            s,
            r#"{"op":"query","repo":"/r","session":"s","prompt":"p","budget_ms":null,"top_n":null,"render":null}"#
        );
        let salted = serde_json::to_string(&query(Some("a/b"))).unwrap();
        assert!(salted.ends_with(r#","memo_salt":"a/b"}"#), "{salted}");
        assert_eq!(
            serde_json::from_str::<Request>(&salted).unwrap(),
            query(Some("a/b"))
        );
    }

    #[test]
    fn only_queries_take_the_memo_salt() {
        assert_eq!(query(None).with_memo_salt(Some("x")), query(Some("x")));
        assert_eq!(query(Some("x")).with_memo_salt(None), query(Some("x")));
        assert_eq!(Request::Ping.with_memo_salt(Some("x")), Request::Ping);
    }

    #[test]
    fn shutdown_is_a_bare_op() {
        let s = serde_json::to_string(&Request::Shutdown).unwrap();
        assert_eq!(s, r#"{"op":"shutdown"}"#);
        assert_eq!(
            serde_json::from_str::<Request>(&s).unwrap(),
            Request::Shutdown
        );
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
