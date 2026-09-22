//! The retrieval brain: prompt -> signals -> candidates (BM25 ⊕ defining ⊕ path, RRF-fused) ->
//! Laya gate (budget-bounded) -> shaped spans. See `docs/architecture.md` §3.2–3.3 and
//! `spike/laya_spike.py` for the fusion this ports (BM25⊕Laya RRF: MRR 0.591 vs BM25 0.480).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use laya_core::{Candidate, Chunk, QueryResult, RankMode, Result, Scorer, Store};

use crate::config::RetrieverConfig;
use crate::fusion::fuse_ranked_lists;
use crate::signals::extract_signals;
use crate::span::{Scored, shape_spans};

/// Turns a prompt into ranked, shaped code spans over a `Store` (candidate generation) and an
/// optional `Scorer` (the Laya decision gate). Both are trait objects so `laya-rank` never
/// depends on the concrete Moon store or candle model — only their contracts in `laya-core`.
///
/// `store` is borrowed (candidate generation is synchronous, on the caller's thread), but
/// `scorer` is `Arc`-owned: the Laya gate bounds the scorer call with a detached worker thread
/// (see `call_scorer_bounded`) that must be free to outlive a timed-out `query()` call. Cloning
/// the `Arc` into that thread makes abandoning a hung scorer sound without any lifetime tricks.
pub struct Retriever<'a> {
    store: &'a dyn Store,
    scorer: Option<Arc<dyn Scorer>>,
    cfg: RetrieverConfig,
}

impl<'a> Retriever<'a> {
    pub fn new(
        store: &'a dyn Store,
        scorer: Option<Arc<dyn Scorer>>,
        cfg: RetrieverConfig,
    ) -> Self {
        Self { store, scorer, cfg }
    }

    /// Rank `prompt` against `repo_id`'s index and return the top spans.
    ///
    /// # Panics
    /// Never panics on scorer failure or timeout — those degrade to `RankMode::Lexical`. Can
    /// return `Err` only for `Store` failures (index/BM25/get_chunks), matching the "design for
    /// failure" rule: callers decide the fail-open policy (e.g. an empty hook response).
    pub fn query(&self, repo_id: &str, prompt: &str) -> Result<QueryResult> {
        let start = Instant::now();
        let signals = extract_signals(prompt);

        if signals.terms.is_empty() && signals.identifiers.is_empty() && signals.paths.is_empty() {
            return Ok(empty_result(start));
        }

        self.store.ensure_index(repo_id)?;

        let bm25_limit = self.cfg.k_candidates.saturating_mul(2).max(1);

        let bm25_hits = self.store.bm25(repo_id, &signals.terms, bm25_limit)?;
        let bm25_ids: Vec<String> = bm25_hits.iter().map(|(id, _)| id.clone()).collect();
        let bm25_scores: HashMap<String, f32> = bm25_hits.into_iter().collect();

        let defining_ids: Vec<String> = if signals.identifiers.is_empty() {
            Vec::new()
        } else {
            self.store
                .chunks_defining(repo_id, &signals.identifiers, bm25_limit)?
        };

        let path_ids: Vec<String> = if signals.paths.is_empty() {
            Vec::new()
        } else {
            self.path_signal(repo_id, &signals.paths, bm25_limit)?
        };

        let fused = fuse_ranked_lists(&[&bm25_ids, &defining_ids, &path_ids], self.cfg.rrf_k);
        if fused.is_empty() {
            return Ok(empty_result(start));
        }
        let fused_scores: HashMap<String, f32> = fused.iter().cloned().collect();
        let top_ids: Vec<String> = fused
            .into_iter()
            .take(self.cfg.k_candidates)
            .map(|(id, _)| id)
            .collect();

        let fetched = self.store.get_chunks(repo_id, &top_ids)?;
        let mut by_id: HashMap<String, Chunk> = fetched.into_iter().map(|c| (c.id(), c)).collect();

        // Preserve fused rank order; silently skip ids the store failed to materialize.
        let candidates: Vec<Candidate> = top_ids
            .iter()
            .filter_map(|id| {
                let chunk = by_id.remove(id)?;
                Some(Candidate {
                    chunk_id: id.clone(),
                    bm25: bm25_scores.get(id).copied().unwrap_or(0.0),
                    fused: fused_scores.get(id).copied().unwrap_or(0.0),
                    chunk,
                })
            })
            .collect();

        if candidates.is_empty() {
            return Ok(empty_result(start));
        }
        let candidates = demote_non_code(candidates, prompt);
        let n_candidates = candidates.len();

        let (scored, mode) = self.laya_gate(prompt, candidates);
        let spans = shape_spans(scored, self.cfg.top_n, self.cfg.max_total_lines);

        Ok(QueryResult {
            spans,
            mode,
            elapsed_ms: elapsed_ms(start),
            candidates: n_candidates,
        })
    }

    /// Resolve path mentions against the index (`list_files`, suffix match, cached in this one
    /// call) and use the resolved paths as extra BM25 terms — chunks whose indexed path tokens
    /// match get boosted, without needing a "chunks for path" method on `Store`.
    fn path_signal(&self, repo_id: &str, mentions: &[String], limit: usize) -> Result<Vec<String>> {
        let files = self.store.list_files(repo_id)?;
        let mut resolved: Vec<String> = Vec::new();
        for mention in mentions {
            for f in &files {
                if path_matches(f, mention) && !resolved.contains(f) {
                    resolved.push(f.clone());
                }
            }
        }
        if resolved.is_empty() {
            return Ok(Vec::new());
        }
        let terms: Vec<String> = resolved
            .iter()
            .flat_map(|p| laya_core::ident::terms(p))
            .collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let hits = self.store.bm25(repo_id, &terms, limit)?;
        Ok(hits.into_iter().map(|(id, _)| id).collect())
    }

    /// Laya decision gate: budget-bounded rerank of `candidates` (already in lexical/RRF order).
    /// Never blocks past `cfg.laya_budget`; any timeout, error, disabled scorer, or malformed
    /// response degrades to the lexical order with `RankMode::Lexical`.
    fn laya_gate(&self, prompt: &str, candidates: Vec<Candidate>) -> (Vec<Scored>, RankMode) {
        let lexical_ids: Vec<String> = candidates.iter().map(|c| c.chunk_id.clone()).collect();
        let to_lexical = |cands: Vec<Candidate>| -> Vec<Scored> {
            cands
                .into_iter()
                .map(|c| Scored {
                    chunk: c.chunk,
                    score: c.fused,
                    p_relevant: None,
                })
                .collect()
        };

        let Some(scorer) = self.scorer.clone().filter(|_| self.cfg.use_laya) else {
            return (to_lexical(candidates), RankMode::Lexical);
        };

        let owned_chunks: Vec<Chunk> = candidates.iter().map(|c| c.chunk.clone()).collect();
        let probs = match call_scorer_bounded(
            scorer,
            prompt.to_string(),
            owned_chunks,
            self.cfg.laya_budget,
        ) {
            Some(p) if p.len() == candidates.len() => p,
            _ => return (to_lexical(candidates), RankMode::Lexical),
        };

        let mut laya_order: Vec<usize> = (0..candidates.len()).collect();
        laya_order.sort_by(|&a, &b| {
            probs[b]
                .partial_cmp(&probs[a])
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let laya_ids: Vec<String> = laya_order
            .iter()
            .map(|&i| candidates[i].chunk_id.clone())
            .collect();

        let final_scores: HashMap<String, f32> = match self.cfg.laya_weight {
            None => fuse_ranked_lists(&[&lexical_ids, &laya_ids], self.cfg.rrf_k)
                .into_iter()
                .collect(),
            Some(w) => weighted_scores(&lexical_ids, &probs, w),
        };

        let mut scored: Vec<Scored> = candidates
            .into_iter()
            .enumerate()
            .map(|(i, c)| Scored {
                score: final_scores.get(&c.chunk_id).copied().unwrap_or(0.0),
                p_relevant: Some(probs[i]),
                chunk: c.chunk,
            })
            .collect();
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let passing = scored
            .iter()
            .filter(|s| s.p_relevant.unwrap_or(0.0) >= self.cfg.p_threshold)
            .count();
        if passing < self.cfg.min_keep {
            // Not enough candidates clear the bar: keep the top `min_keep` by fused score anyway
            // rather than starving the caller of context.
            scored.truncate(self.cfg.min_keep.min(scored.len()));
        } else {
            scored.retain(|s| s.p_relevant.unwrap_or(0.0) >= self.cfg.p_threshold);
        }

        (scored, RankMode::Laya)
    }
}

/// `(1-w)·(1 - rank/n) + w·p` per candidate, where `rank` is the lexical (fused) position.
pub(crate) fn weighted_scores(lexical_ids: &[String], probs: &[f32], w: f32) -> HashMap<String, f32> {
    let n = lexical_ids.len().max(1) as f32;
    lexical_ids
        .iter()
        .enumerate()
        .map(|(rank, id)| (id.clone(), (1.0 - w) * (1.0 - rank as f32 / n) + w * probs[rank]))
        .collect()
}

/// Words that signal the task is about prose/config files rather than code.
const NON_CODE_INTENT: &[&str] = &[
    "readme", "doc", "docs", "documentation", "guide", "markdown", "changelog", "config", "configuration", "toml",
    "yaml", "yml", "json", "dockerfile", "makefile", "ci", "workflow",
];

/// Unless the prompt is about docs/config, line-window (`Lang::Text`) chunks halve their fused
/// score and move behind code chunks (stable). Prose matches the task's words without being the
/// code the agent must read, and was 17% of returned spans on the moon dev set.
pub(crate) fn demote_non_code(mut candidates: Vec<Candidate>, prompt: &str) -> Vec<Candidate> {
    let lower = prompt.to_ascii_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_ascii_alphanumeric()).collect();
    if NON_CODE_INTENT.iter().any(|w| words.contains(w)) {
        return candidates;
    }
    for c in candidates.iter_mut().filter(|c| c.chunk.lang == laya_core::Lang::Text) {
        c.fused *= 0.5;
    }
    candidates.sort_by_key(|c| c.chunk.lang == laya_core::Lang::Text);
    candidates
}

fn path_matches(file: &str, mention: &str) -> bool {
    if file == mention || file.ends_with(&format!("/{mention}")) {
        return true;
    }
    if !mention.contains('/') {
        return file.rsplit('/').next() == Some(mention);
    }
    false
}

fn empty_result(start: Instant) -> QueryResult {
    QueryResult {
        spans: Vec::new(),
        mode: RankMode::Lexical,
        elapsed_ms: elapsed_ms(start),
        candidates: 0,
    }
}

fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis() as u64
}

/// Run `scorer.score(task, chunks)` on a detached worker thread, bounded by `budget`. Returns
/// `None` on timeout, a channel error, or the scorer itself returning `Err`. A late result (the
/// thread finishes after `budget` elapses) is simply dropped — the send on a disconnected
/// receiver fails silently and the thread exits.
///
/// `scorer` is an owned `Arc`, so the detached thread (which must be free to keep running after
/// `budget` elapses and this function has returned — that's the whole point of "abandon, don't
/// block") holds its own strong reference. Abandoning a hung scorer this way is sound: nothing
/// this function's caller does afterward (including dropping its own clone of the `Arc`) can
/// invalidate the data the thread is still reading, no `unsafe` lifetime extension required.
fn call_scorer_bounded(
    scorer: Arc<dyn Scorer>,
    task: String,
    chunks: Vec<Chunk>,
    budget: Duration,
) -> Option<Vec<f32>> {
    let (tx, rx) = mpsc::channel::<Result<Vec<f32>>>();

    thread::spawn(move || {
        let refs: Vec<&Chunk> = chunks.iter().collect();
        let result = scorer.score(&task, &refs);
        let _ = tx.send(result);
    });

    match rx.recv_timeout(budget) {
        Ok(Ok(probs)) => Some(probs),
        Ok(Err(_)) => None,
        Err(_timeout_or_disconnected) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::{
        FailingScorer, FailingStore, FakeStore, ProbScorer, SleepyScorer, WrongLengthScorer,
    };
    use laya_core::Lang;
    use std::collections::HashMap as StdHashMap;

    fn cand(path: &str, lang: Lang, fused: f32) -> Candidate {
        let mut c = chunk(path, 1, 10, &[], "x");
        c.lang = lang;
        Candidate { chunk_id: c.id(), chunk: c, bm25: fused, fused }
    }

    #[test]
    fn weighted_scores_blend_lexical_rank_and_probability() {
        let ids = vec!["a".to_string(), "b".to_string()];
        let s = weighted_scores(&ids, &[0.1, 0.9], 0.5);
        assert!((s["a"] - (0.5 * 1.0 + 0.05)).abs() < 1e-6);
        assert!((s["b"] - (0.5 * 0.5 + 0.45)).abs() < 1e-6);
        assert!(s["b"] > s["a"], "a confident Laya answer should overtake one lexical rank");
    }

    #[test]
    fn non_code_chunks_are_demoted_unless_prompt_is_about_docs() {
        let cands = vec![cand("README.md", Lang::Text, 0.9), cand("src/a.rs", Lang::Rust, 0.5)];
        let out = demote_non_code(cands.clone(), "fix the mmap budget review issues");
        assert_eq!(out[0].chunk.path, "src/a.rs");
        assert!((out[1].fused - 0.45).abs() < 1e-6);
        let docs = demote_non_code(cands, "update the README install docs");
        assert_eq!(docs[0].chunk.path, "README.md");
    }

    fn chunk(path: &str, start: u32, end: u32, defines: &[&str], text: &str) -> Chunk {
        Chunk {
            path: path.to_string(),
            start_line: start,
            end_line: end,
            lang: Lang::Rust,
            symbol: String::new(),
            kind: "function_item".to_string(),
            defines: defines.iter().map(|s| s.to_string()).collect(),
            text: text.to_string(),
        }
    }

    fn sample_chunks() -> Vec<Chunk> {
        vec![
            chunk(
                "src/store/retry.rs",
                1,
                20,
                &["retry_with_backoff"],
                "fn retry_with_backoff() {}",
            ),
            chunk(
                "src/store/circuit.rs",
                1,
                20,
                &["CircuitBreaker"],
                "struct CircuitBreaker;",
            ),
            chunk(
                "src/util/format.rs",
                1,
                20,
                &["format_bytes"],
                "fn format_bytes() {}",
            ),
        ]
    }

    #[test]
    fn empty_prompt_returns_empty_lexical_result() {
        let store = FakeStore::new(sample_chunks());
        let r = Retriever::new(&store, None, RetrieverConfig::default());
        let out = r.query("repo", "").unwrap();
        assert!(out.spans.is_empty());
        assert_eq!(out.mode, RankMode::Lexical);
        assert_eq!(out.candidates, 0);
    }

    #[test]
    fn stopword_only_prompt_with_no_hits_returns_empty_result() {
        let store = FakeStore::new(sample_chunks());
        let r = Retriever::new(&store, None, RetrieverConfig::default());
        let out = r.query("repo", "the a of").unwrap();
        assert!(out.spans.is_empty());
        assert_eq!(out.mode, RankMode::Lexical);
    }

    #[test]
    fn bm25_signal_finds_relevant_chunk_without_scorer() {
        let store = FakeStore::new(sample_chunks());
        let r = Retriever::new(&store, None, RetrieverConfig::default());
        let out = r.query("repo", "add retry with backoff").unwrap();
        assert_eq!(out.mode, RankMode::Lexical);
        assert!(out.spans.iter().any(|s| s.path == "src/store/retry.rs"));
    }

    #[test]
    fn identifier_mention_uses_chunks_defining() {
        let store = FakeStore::new(sample_chunks());
        let r = Retriever::new(&store, None, RetrieverConfig::default());
        // "CircuitBreaker" is an explicit (mixed-case) identifier -> chunks_defining signal.
        let out = r
            .query("repo", "why does CircuitBreaker never half-open")
            .unwrap();
        assert!(out.spans.iter().any(|s| s.path == "src/store/circuit.rs"));
    }

    #[test]
    fn path_mention_boosts_matching_file_via_list_files() {
        let store = FakeStore::new(sample_chunks());
        let r = Retriever::new(&store, None, RetrieverConfig::default());
        let out = r.query("repo", "something is off in format.rs").unwrap();
        assert!(out.spans.iter().any(|s| s.path == "src/util/format.rs"));
        assert_eq!(
            store
                .list_files_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }

    #[test]
    fn store_error_propagates() {
        let store = FailingStore;
        let r = Retriever::new(&store, None, RetrieverConfig::default());
        assert!(r.query("repo", "anything at all").is_err());
    }

    #[test]
    fn laya_scores_and_reranks_within_budget() {
        let chunks = sample_chunks();
        let ids: Vec<String> = chunks.iter().map(|c| c.id()).collect();
        let mut probs: StdHashMap<String, f32> = StdHashMap::new();
        // circuit.rs is already the strongest lexical candidate (identifier match +
        // bm25 on "circuit"/"breaker") *and* the clear Laya favorite, so the fused result is
        // unambiguous. (RRF is rank-based: a signal disagreement that merely swaps two items'
        // adjacent ranks between two equally-weighted lists ties by construction — see
        // `fusion::tests::agreement_across_signals_wins` for the "signals agree" case this
        // exercises at the `Retriever` level.)
        probs.insert(ids[0].clone(), 0.2); // retry.rs
        probs.insert(ids[1].clone(), 0.95); // circuit.rs
        probs.insert(ids[2].clone(), 0.1); // format.rs
        let store = FakeStore::new(chunks);
        let scorer: Arc<dyn Scorer> = Arc::new(ProbScorer::new(probs));
        let r = Retriever::new(&store, Some(scorer), RetrieverConfig::default());
        let out = r
            .query(
                "repo",
                "why does CircuitBreaker never back off during retry check format too",
            )
            .unwrap();
        assert_eq!(out.mode, RankMode::Laya);
        assert!(!out.spans.is_empty());
        assert_eq!(out.spans[0].path, "src/store/circuit.rs");
        assert_eq!(out.spans[0].p_relevant, Some(0.95));
    }

    #[test]
    fn scorer_timeout_falls_back_to_lexical_mode() {
        let chunks = sample_chunks();
        let store = FakeStore::new(chunks);
        // The background thread outlives this test's budget on purpose (that's the scenario
        // under test); the `Arc` means it can keep running after `query()` returns without any
        // lifetime hazard.
        let scorer: Arc<dyn Scorer> = Arc::new(SleepyScorer {
            sleep: Duration::from_millis(200),
            inner: ProbScorer::new(StdHashMap::new()),
        });
        let cfg = RetrieverConfig {
            laya_budget: Duration::from_millis(20),
            ..RetrieverConfig::default()
        };
        let r = Retriever::new(&store, Some(scorer), cfg);
        let out = r
            .query("repo", "investigate retry circuit backoff format")
            .unwrap();
        assert_eq!(out.mode, RankMode::Lexical);
        assert!(out.spans.iter().all(|s| s.p_relevant.is_none()));
    }

    #[test]
    fn scorer_error_falls_back_to_lexical_mode() {
        let store = FakeStore::new(sample_chunks());
        let scorer: Arc<dyn Scorer> = Arc::new(FailingScorer);
        let r = Retriever::new(&store, Some(scorer), RetrieverConfig::default());
        let out = r
            .query("repo", "investigate retry circuit backoff format")
            .unwrap();
        assert_eq!(out.mode, RankMode::Lexical);
    }

    #[test]
    fn scorer_returning_wrong_length_falls_back_to_lexical_mode() {
        let store = FakeStore::new(sample_chunks());
        let scorer: Arc<dyn Scorer> = Arc::new(WrongLengthScorer);
        let r = Retriever::new(&store, Some(scorer), RetrieverConfig::default());
        let out = r
            .query("repo", "investigate retry circuit backoff format")
            .unwrap();
        assert_eq!(out.mode, RankMode::Lexical);
    }

    #[test]
    fn use_laya_false_skips_scorer_even_when_present() {
        let store = FakeStore::new(sample_chunks());
        let scorer = Arc::new(ProbScorer::new(StdHashMap::new()));
        let cfg = RetrieverConfig {
            use_laya: false,
            ..RetrieverConfig::default()
        };
        let r = Retriever::new(&store, Some(scorer.clone()), cfg);
        let out = r
            .query("repo", "investigate retry circuit backoff format")
            .unwrap();
        assert_eq!(out.mode, RankMode::Lexical);
        assert_eq!(*scorer.calls.lock().unwrap(), 0);
    }

    #[test]
    fn threshold_drops_low_probability_candidates() {
        let chunks = sample_chunks();
        let ids: Vec<String> = chunks.iter().map(|c| c.id()).collect();
        let mut probs: StdHashMap<String, f32> = StdHashMap::new();
        probs.insert(ids[0].clone(), 0.9); // passes
        probs.insert(ids[1].clone(), 0.1); // dropped
        probs.insert(ids[2].clone(), 0.05); // dropped
        let store = FakeStore::new(chunks);
        let scorer: Arc<dyn Scorer> = Arc::new(ProbScorer::new(probs));
        // low enough that the threshold actually bites
        let cfg = RetrieverConfig {
            min_keep: 1,
            ..RetrieverConfig::default()
        };
        let r = Retriever::new(&store, Some(scorer), cfg);
        let out = r
            .query("repo", "investigate retry circuit backoff format")
            .unwrap();
        assert_eq!(out.mode, RankMode::Laya);
        assert_eq!(out.spans.len(), 1);
        assert_eq!(out.spans[0].path, "src/store/retry.rs");
    }

    #[test]
    fn min_keep_overrides_threshold_when_too_few_would_pass() {
        let chunks = sample_chunks();
        let ids: Vec<String> = chunks.iter().map(|c| c.id()).collect();
        let mut probs: StdHashMap<String, f32> = StdHashMap::new();
        probs.insert(ids[0].clone(), 0.05);
        probs.insert(ids[1].clone(), 0.04);
        probs.insert(ids[2].clone(), 0.03);
        let store = FakeStore::new(chunks);
        let scorer: Arc<dyn Scorer> = Arc::new(ProbScorer::new(probs));
        let cfg = RetrieverConfig {
            min_keep: 3,
            ..RetrieverConfig::default()
        };
        let r = Retriever::new(&store, Some(scorer), cfg);
        let out = r
            .query("repo", "investigate retry circuit backoff format")
            .unwrap();
        assert_eq!(out.mode, RankMode::Laya);
        // Nothing clears p_threshold (0.5), but min_keep=3 forces all 3 through.
        assert_eq!(out.spans.len(), 3);
    }
}
