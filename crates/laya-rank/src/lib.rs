//! `laya-rank`: the retrieval brain. Turns a Claude Code prompt into the top-N ranked code
//! spans, using only [`laya_core::Store`] and [`laya_core::Scorer`] — the concrete Moon store
//! and Laya model live in other crates and are wired in by the caller (`layad`/`laya-cli`).
//!
//! Pipeline (`docs/architecture.md` §3.2–3.5):
//! 1. [`signals::extract_signals`] pulls BM25 terms, explicit identifiers and file-path
//!    mentions out of the prompt.
//! 2. [`Retriever::query`] fans those out to `Store::bm25` / `chunks_defining` / a path-boosted
//!    BM25 call, fuses the three ranked lists with Reciprocal Rank Fusion
//!    (`fusion::fuse_ranked_lists`), and materializes the top candidates.
//! 3. If a `Scorer` is configured, a budget-bounded Laya gate reranks the candidates (RRF of the
//!    lexical and Laya ranks, then a probability threshold with a `min_keep` floor); on
//!    timeout/error it degrades to `RankMode::Lexical`.
//! 4. `span::shape_spans` merges adjacent/overlapping same-file chunks, keeps the top-N, and
//!    enforces a total-line budget.
//!
//! [`render_context`] and [`read_narrowing`] turn a [`laya_core::QueryResult`] into what the
//! hooks/MCP layer actually sends to Claude Code.

mod config;
mod fusion;
mod read_narrow;
mod related;
mod render;
mod retriever;
mod signals;
mod sizing;
mod span;

#[cfg(test)]
pub(crate) mod fakes;

pub use config::RetrieverConfig;
pub use read_narrow::{ReadPolicy, read_narrowing};
pub use render::{
    FileMatches, IdentMatches, MATCH_MAX_CHARS, MAX_INJECT_CHARS, MatchGroup, MatchLine,
    TRUST_LINE, render_compact, render_compact_opts, render_context, render_context_opts,
    render_matches,
};
pub use retriever::{Retriever, asks_for_non_code};
pub use signals::{
    FollowUpIntent, PromptSignals, content_terms, extract_signals, follow_up_intent, is_follow_up,
};
pub use sizing::{
    Scope, SizeOpts, SizedContext, SizingCaps, SizingPolicy, SpanKey, follow_up_caps,
    is_prose_path, render_sized, render_sized_with_keys, size_context, size_context_opts,
    sized_keys, small_repo_caps,
};
