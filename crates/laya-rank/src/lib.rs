//! `laya-rank`: the retrieval brain. Turns a Claude Code prompt into the top-N ranked code
//! spans, using only [`laya_core::Store`] and [`laya_core::Scorer`] — the concrete Moon store
//! and Laya model live in other crates and are wired in by the caller (`layad`/`laya-cli`).
//!
//! This module builds up in stages (see `docs/build-context.md` for the commit-per-step rule):
//! 1. [`config::RetrieverConfig`] — tunables, with defaults.
//! 2. [`signals::extract_signals`] — BM25 terms, explicit identifiers and file-path mentions
//!    pulled out of a prompt.
//! 3. [`fusion::fuse_ranked_lists`] — the Reciprocal Rank Fusion primitive the spike found best
//!    (`spike/laya_spike.py`: BM25⊕Laya RRF, MRR 0.591 vs BM25 0.480), reused for both candidate
//!    generation and the Laya gate.

mod config;
mod fusion;
mod signals;

pub use config::RetrieverConfig;
pub use signals::{PromptSignals, extract_signals};
