//! Shared types and contracts for laya-codex.
//!
//! Every other crate depends on these definitions; changing them is a contract change.

use serde::{Deserialize, Serialize};

pub mod ident;

/// Language a chunk was parsed with. `Text` means the line-window fallback chunker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    Rust,
    Python,
    TypeScript,
    Tsx,
    JavaScript,
    Go,
    Java,
    C,
    Cpp,
    CSharp,
    Ruby,
    Php,
    Kotlin,
    Swift,
    Text,
}

impl Lang {
    pub fn as_str(self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::Python => "python",
            Lang::TypeScript => "typescript",
            Lang::Tsx => "tsx",
            Lang::JavaScript => "javascript",
            Lang::Go => "go",
            Lang::Java => "java",
            Lang::C => "c",
            Lang::Cpp => "cpp",
            Lang::CSharp => "csharp",
            Lang::Ruby => "ruby",
            Lang::Php => "php",
            Lang::Kotlin => "kotlin",
            Lang::Swift => "swift",
            Lang::Text => "text",
        }
    }
}

/// A contiguous span of a source file, produced by the chunker. Lines are 1-based, inclusive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    /// Repo-relative path with `/` separators.
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub lang: Lang,
    /// Enclosing symbol path, e.g. `impl Store for MoonStore > fn get` or `class Foo > def bar`; empty if top-level.
    pub symbol: String,
    /// Syntax kind of the dominant node, e.g. `function_item`, `class_definition`, `window`.
    pub kind: String,
    /// Identifiers *defined* in this chunk (function/type/const names).
    pub defines: Vec<String>,
    /// Identifiers *referenced* in this chunk (callees, macros, used types, imported names),
    /// excluding its own `defines`; first-occurrence order, deduplicated, capped. Extracted with
    /// tree-sitter; resolved to definitions at query time via `Store::chunks_defining`.
    #[serde(default)]
    pub refs: Vec<String>,
    /// Exact source text of the span.
    pub text: String,
}

impl Chunk {
    /// Content-addressed id: blake3(path \0 start \0 end \0 text), hex, 32 chars.
    pub fn id(&self) -> String {
        let mut h = blake3::Hasher::new();
        h.update(self.path.as_bytes());
        h.update(&[0]);
        h.update(&self.start_line.to_le_bytes());
        h.update(&self.end_line.to_le_bytes());
        h.update(&[0]);
        h.update(self.text.as_bytes());
        h.finalize().to_hex()[..32].to_string()
    }

    pub fn line_count(&self) -> u32 {
        self.end_line.saturating_sub(self.start_line) + 1
    }
}

/// A retrieval candidate with the signals that produced it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub chunk_id: String,
    pub chunk: Chunk,
    /// Summed BM25 score (0 when the candidate came only from symbol/path signals).
    pub bm25: f32,
    /// Rank-fusion score from candidate generation (higher is better).
    pub fused: f32,
}

/// Final ranked output returned to hooks, MCP and the CLI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RankedSpan {
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub symbol: String,
    /// Laya P(relevant) when the model ran, otherwise `None`.
    pub p_relevant: Option<f32>,
    /// Final ordering score (higher is better).
    pub score: f32,
    pub text: String,
}

/// How the ranking was produced; reported so benchmarks can attribute effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RankMode {
    /// Laya scored every candidate within budget.
    Laya,
    /// Laya missed its deadline or is unavailable; candidate-generation order was used.
    Lexical,
}

/// A location reached from a ranked span by one reference hop (no code text; shown as a pointer).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Related {
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub symbol: String,
    /// Human-readable edge, e.g. "defines `WalTailReader` (used by #1)" or "calls `fanout_tick` (#2)".
    pub relation: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryResult {
    pub spans: Vec<RankedSpan>,
    pub mode: RankMode,
    pub elapsed_ms: u64,
    pub candidates: usize,
    /// One-hop reference neighbours of the top spans (callees' definitions and callers).
    #[serde(default)]
    pub related: Vec<Related>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("store unavailable: {0}")]
    StoreUnavailable(String),
    #[error("store error: {0}")]
    Store(String),
    #[error("model error: {0}")]
    Model(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("deadline exceeded")]
    Deadline,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Persistence + lexical search contract. v1 = Moon over RESP, v2 = Moon embedded.
pub trait Store: Send + Sync {
    /// Idempotently register the repo index (creates FT index if missing).
    fn ensure_index(&self, repo_id: &str) -> Result<()>;
    /// Replace all chunks of `path` with `chunks` and record the file content hash.
    fn put_file(&self, repo_id: &str, path: &str, file_hash: &str, chunks: &[Chunk]) -> Result<()>;
    /// Remove a file and its chunks.
    fn delete_file(&self, repo_id: &str, path: &str) -> Result<()>;
    /// Stored content hash of a file, if indexed.
    fn file_hash(&self, repo_id: &str, path: &str) -> Result<Option<String>>;
    /// All indexed paths for the repo.
    fn list_files(&self, repo_id: &str) -> Result<Vec<String>>;
    /// OR-semantics BM25 over normalized terms; returns (chunk_id, summed score), best first.
    fn bm25(&self, repo_id: &str, terms: &[String], limit: usize) -> Result<Vec<(String, f32)>>;
    /// Chunks whose `defines` contain any of `idents` (exact, case-sensitive).
    fn chunks_defining(
        &self,
        repo_id: &str,
        idents: &[String],
        limit: usize,
    ) -> Result<Vec<String>>;
    /// Chunks whose `refs` contain any of `idents` (exact, case-sensitive): the callers/users.
    /// Ordered by how many of `idents` they reference (desc), then id. Default: unsupported → empty.
    fn chunks_referencing(
        &self,
        _repo_id: &str,
        _idents: &[String],
        _limit: usize,
    ) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    fn get_chunks(&self, repo_id: &str, ids: &[String]) -> Result<Vec<Chunk>>;
    /// Every chunk of one indexed file, ordered by `start_line` (empty if the file is not
    /// indexed). Backs file outlines. Default: unsupported → empty, which callers treat as
    /// "not indexed" and fail open.
    fn chunks_of_file(&self, _repo_id: &str, _path: &str) -> Result<Vec<Chunk>> {
        Ok(Vec::new())
    }
    /// Generic memo cache (Laya scores, query results). TTL in seconds, 0 = no expiry.
    fn memo_get(&self, key: &str) -> Result<Option<String>>;
    fn memo_put(&self, key: &str, value: &str, ttl_secs: u64) -> Result<()>;
}

/// Relevance scorer contract (Laya). Returns P(relevant) per chunk, same order as input.
pub trait Scorer: Send + Sync {
    fn score(&self, task: &str, chunks: &[&Chunk]) -> Result<Vec<f32>>;
}
