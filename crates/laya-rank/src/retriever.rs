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
use crate::related::expand_related;
use crate::signals::{extract_signals, task_focus};
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
        // BM25 terms and the Laya scorer see the task, not the instructions wrapped around it;
        // identifiers and path mentions still come from the whole prompt.
        let focus = task_focus(prompt);
        let mut signals = extract_signals(prompt);
        signals.terms = extract_signals(&focus).terms;

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
        let candidates = demote_non_code(candidates, &focus);
        let n_candidates = candidates.len();

        let (scored, mode) = self.laya_gate(&focus, candidates);
        // Seeds for one-hop expansion are the top 3 *scored chunks*, captured before shaping
        // merges same-file spans together (a merge loses `defines`/`refs`).
        let seeds: Vec<Chunk> = scored.iter().take(3).map(|s| s.chunk.clone()).collect();
        let spans = shape_spans(scored, self.cfg.top_n, self.cfg.max_total_lines);
        let mut related = expand_related(self.store, repo_id, &seeds, &spans, self.cfg.max_related)
            .unwrap_or_default();
        if self.cfg.max_related > 0 {
            // Task-named identifiers first, then what the top chunks define. Fails open.
            let mut idents: Vec<String> = signals.identifiers.clone();
            for d in seeds.iter().flat_map(|s| s.defines.iter()) {
                if !idents.contains(d) {
                    idents.push(d.clone());
                }
            }
            related.extend(
                crate::related::usage_list(self.store, repo_id, &idents).unwrap_or_default(),
            );
        }

        Ok(QueryResult {
            spans,
            mode,
            elapsed_ms: elapsed_ms(start),
            candidates: n_candidates,
            related,
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
    /// Never blocks past `cfg.laya_budget`. The scorer works through the candidates in order
    /// and stops inside the budget, so a slow machine re-ranks fewer of them rather than none;
    /// only when nothing was scored (timeout, error, disabled scorer, malformed response) does
    /// the lexical order stand with `RankMode::Lexical`.
    fn laya_gate(&self, prompt: &str, candidates: Vec<Candidate>) -> (Vec<Scored>, RankMode) {
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
            Some(p) if p.len() == candidates.len() && p.iter().any(Option::is_some) => p,
            _ => return (to_lexical(candidates), RankMode::Lexical),
        };

        // The model re-ranks the candidates it scored in time; the ones the budget did not reach
        // (the lexical tail: candidates are scored most promising first) stay below them in
        // lexical order.
        let (reached, unreached): (Vec<(Candidate, Option<f32>)>, Vec<_>) = candidates
            .into_iter()
            .zip(probs)
            .partition(|(_, p)| p.is_some());
        let lexical_ids: Vec<String> = reached.iter().map(|(c, _)| c.chunk_id.clone()).collect();
        let probs: Vec<f32> = reached.iter().map(|(_, p)| p.unwrap_or(0.0)).collect();

        let mut laya_order: Vec<usize> = (0..reached.len()).collect();
        laya_order.sort_by(|&a, &b| {
            probs[b]
                .partial_cmp(&probs[a])
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let laya_ids: Vec<String> = laya_order.iter().map(|&i| lexical_ids[i].clone()).collect();

        let final_scores: HashMap<String, f32> = match self.cfg.laya_weight {
            None => fuse_ranked_lists(&[&lexical_ids, &laya_ids], self.cfg.rrf_k)
                .into_iter()
                .collect(),
            Some(w) => weighted_scores(&lexical_ids, &probs, w),
        };

        let mut scored: Vec<Scored> = reached
            .into_iter()
            .map(|(c, p)| Scored {
                score: final_scores.get(&c.chunk_id).copied().unwrap_or(0.0),
                p_relevant: p,
                chunk: c.chunk,
            })
            .collect();
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.extend(unreached.into_iter().map(|(c, _)| Scored {
            chunk: c.chunk,
            score: 0.0,
            p_relevant: None,
        }));

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
pub(crate) fn weighted_scores(
    lexical_ids: &[String],
    probs: &[f32],
    w: f32,
) -> HashMap<String, f32> {
    let n = lexical_ids.len().max(1) as f32;
    lexical_ids
        .iter()
        .enumerate()
        .map(|(rank, id)| {
            (
                id.clone(),
                (1.0 - w) * (1.0 - rank as f32 / n) + w * probs[rank],
            )
        })
        .collect()
}

/// Words that signal the task is about prose/config files rather than code.
const NON_CODE_INTENT: &[&str] = &[
    "readme",
    "doc",
    "docs",
    "documentation",
    "guide",
    "markdown",
    "changelog",
    "config",
    "configuration",
    "toml",
    "yaml",
    "yml",
    "json",
    "dockerfile",
    "makefile",
    "ci",
    "workflow",
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
    for c in candidates
        .iter_mut()
        .filter(|c| c.chunk.lang == laya_core::Lang::Text)
    {
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
        related: Vec::new(),
    }
}

fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis() as u64
}

/// Share of the Laya budget the scorer may spend before it must stop scoring.
const SCORER_SHARE_OF_BUDGET: f32 = 0.85;

/// Run `scorer.score_within(task, chunks, deadline)` on a detached worker thread, bounded by
/// `budget`. Returns `None` on timeout, a channel error, or the scorer itself returning `Err`.
/// A late result (the thread finishes after `budget` elapses) is simply dropped — the send on a
/// disconnected receiver fails silently and the thread exits.
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
) -> Option<Vec<Option<f32>>> {
    let (tx, rx) = mpsc::channel::<Result<Vec<Option<f32>>>>();
    // The scorer stops itself inside the budget, leaving room to hand the result back; the
    // `recv_timeout` below stays the hard stop for scorers that cannot.
    let deadline = Instant::now() + budget.mul_f32(SCORER_SHARE_OF_BUDGET);

    thread::spawn(move || {
        let refs: Vec<&Chunk> = chunks.iter().collect();
        let result = scorer.score_within(&task, &refs, deadline);
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
        FailingReferencingStore, FailingScorer, FailingStore, FakeStore, ProbScorer, SleepyScorer,
        WrongLengthScorer,
    };
    use laya_core::Lang;
    use std::collections::HashMap as StdHashMap;
    use std::sync::Mutex;

    const WRAPPED: &str = "In this repository, find the source code that implements or would need \
        to change for the following change, and briefly explain how it works:\n\n\"Enforce the \
        mmap budget when sealing vector segments\"\n\nBe efficient: read only what you need. End \
        your answer with one line exactly of the form\nFILES: <comma-separated repo-relative \
        paths of the most relevant source files>";

    fn wrapper_trap_chunks() -> Vec<Chunk> {
        vec![
            chunk(
                "src/release_notes.rs",
                1,
                3,
                &[],
                "// change log: source code files, relative paths, form\nfn notes() {}\n",
            ),
            chunk(
                "src/vector/mmap_budget.rs",
                1,
                3,
                &["enforce_budget"],
                "fn enforce_budget() {\n    // mmap budget checked when sealing segments\n}\n",
            ),
        ]
    }

    #[test]
    fn wrapper_words_do_not_outrank_the_quoted_task() {
        let store = FakeStore::new(wrapper_trap_chunks());
        let r = Retriever::new(&store, None, RetrieverConfig::default());
        let out = r.query("repo", WRAPPED).unwrap();
        assert_eq!(out.spans[0].path, "src/vector/mmap_budget.rs");
    }

    /// Answers `score_within` with fixed probabilities (`None` = the budget ran out before that
    /// chunk) and records the deadline it was given. A plain `score` call is a bug: the
    /// retriever must ask for a bounded score.
    struct PartialScorer {
        probs: Vec<Option<f32>>,
        deadline: Mutex<Option<Instant>>,
    }

    impl PartialScorer {
        fn new(probs: Vec<Option<f32>>) -> Self {
            Self {
                probs,
                deadline: Mutex::new(None),
            }
        }
    }

    impl Scorer for PartialScorer {
        fn score(&self, _task: &str, _chunks: &[&Chunk]) -> Result<Vec<f32>> {
            panic!("unbounded score call")
        }
        fn score_within(
            &self,
            _task: &str,
            chunks: &[&Chunk],
            deadline: Instant,
        ) -> Result<Vec<Option<f32>>> {
            *self.deadline.lock().unwrap() = Some(deadline);
            Ok((0..chunks.len())
                .map(|i| self.probs.get(i).copied().flatten())
                .collect())
        }
    }

    /// Three chunks whose lexical order for "alpha beta gamma" is a, b, c.
    fn abc_chunks() -> Vec<Chunk> {
        vec![
            chunk("src/a.rs", 1, 3, &[], "fn a() { alpha beta gamma }\n"),
            chunk("src/b.rs", 1, 3, &[], "fn b() { alpha beta }\n"),
            chunk("src/c.rs", 1, 3, &[], "fn c() { alpha }\n"),
        ]
    }

    fn weighted_cfg() -> RetrieverConfig {
        RetrieverConfig {
            laya_weight: Some(0.5),
            ..RetrieverConfig::default()
        }
    }

    #[test]
    fn candidates_the_budget_did_not_reach_keep_lexical_order_below_the_scored_ones() {
        let store = FakeStore::new(abc_chunks());
        let scorer = Arc::new(PartialScorer::new(vec![Some(0.1), Some(0.9), None]));
        let r = Retriever::new(&store, Some(scorer), weighted_cfg());
        let out = r.query("repo", "alpha beta gamma").unwrap();
        assert_eq!(out.mode, RankMode::Laya, "a partial model run still ranks");
        let paths: Vec<&str> = out.spans.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(paths, ["src/b.rs", "src/a.rs", "src/c.rs"]);
        assert_eq!(out.spans[2].p_relevant, None, "c was not scored");
    }

    #[test]
    fn nothing_scored_in_time_is_lexical() {
        let store = FakeStore::new(abc_chunks());
        let scorer = Arc::new(PartialScorer::new(vec![None, None, None]));
        let r = Retriever::new(&store, Some(scorer), weighted_cfg());
        let out = r.query("repo", "alpha beta gamma").unwrap();
        assert_eq!(out.mode, RankMode::Lexical);
        let paths: Vec<&str> = out.spans.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(paths, ["src/a.rs", "src/b.rs", "src/c.rs"]);
    }

    #[test]
    fn the_scorer_is_told_to_stop_inside_the_budget() {
        let store = FakeStore::new(abc_chunks());
        let scorer = Arc::new(PartialScorer::new(vec![Some(0.5); 3]));
        let budget = Duration::from_millis(1000);
        let cfg = RetrieverConfig {
            laya_budget: budget,
            ..weighted_cfg()
        };
        let r = Retriever::new(&store, Some(scorer.clone()), cfg);
        let before = Instant::now();
        r.query("repo", "alpha beta gamma").unwrap();
        let deadline = scorer.deadline.lock().unwrap().expect("bounded call");
        assert!(deadline > before && deadline < before + budget);
    }

    struct TaskRecorder(Mutex<Vec<String>>);

    impl Scorer for TaskRecorder {
        fn score(&self, task: &str, chunks: &[&Chunk]) -> Result<Vec<f32>> {
            self.0.lock().unwrap().push(task.to_string());
            Ok(vec![0.5; chunks.len()])
        }
    }

    #[test]
    fn the_laya_scorer_sees_the_task_not_the_wrapper() {
        let store = FakeStore::new(wrapper_trap_chunks());
        let rec = Arc::new(TaskRecorder(Mutex::new(Vec::new())));
        let r = Retriever::new(&store, Some(rec.clone()), RetrieverConfig::default());
        r.query("repo", WRAPPED).unwrap();
        assert_eq!(
            rec.0.lock().unwrap().as_slice(),
            ["Enforce the mmap budget when sealing vector segments"]
        );
    }

    fn cand(path: &str, lang: Lang, fused: f32) -> Candidate {
        let mut c = chunk(path, 1, 10, &[], "x");
        c.lang = lang;
        Candidate {
            chunk_id: c.id(),
            chunk: c,
            bm25: fused,
            fused,
        }
    }

    #[test]
    fn weighted_scores_blend_lexical_rank_and_probability() {
        let ids = vec!["a".to_string(), "b".to_string()];
        let s = weighted_scores(&ids, &[0.1, 0.9], 0.5);
        assert!((s["a"] - (0.5 * 1.0 + 0.05)).abs() < 1e-6);
        assert!((s["b"] - (0.5 * 0.5 + 0.45)).abs() < 1e-6);
        assert!(
            s["b"] > s["a"],
            "a confident Laya answer should overtake one lexical rank"
        );
    }

    #[test]
    fn non_code_chunks_are_demoted_unless_prompt_is_about_docs() {
        let cands = vec![
            cand("README.md", Lang::Text, 0.9),
            cand("src/a.rs", Lang::Rust, 0.5),
        ];
        let out = demote_non_code(cands.clone(), "fix the mmap budget review issues");
        assert_eq!(out[0].chunk.path, "src/a.rs");
        assert!((out[1].fused - 0.45).abs() < 1e-6);
        let docs = demote_non_code(cands, "update the README install docs");
        assert_eq!(docs[0].chunk.path, "README.md");
    }

    fn chunk(path: &str, start: u32, end: u32, defines: &[&str], text: &str) -> Chunk {
        chunk_with_refs(path, start, end, defines, &[], text)
    }

    fn chunk_with_refs(
        path: &str,
        start: u32,
        end: u32,
        defines: &[&str],
        refs: &[&str],
        text: &str,
    ) -> Chunk {
        Chunk {
            path: path.to_string(),
            start_line: start,
            end_line: end,
            lang: Lang::Rust,
            symbol: String::new(),
            kind: "function_item".to_string(),
            defines: defines.iter().map(|s| s.to_string()).collect(),
            refs: refs.iter().map(|s| s.to_string()).collect(),
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
    fn query_expands_related_from_top_scored_chunk_refs() {
        // `jitter.rs` shares no bm25 terms with the prompt, so it can only surface via the
        // ref-expansion path (not lexical search) — isolating what this test checks.
        let retry = chunk_with_refs(
            "src/store/retry.rs",
            1,
            20,
            &["retry_with_backoff"],
            &["compute_jitter"],
            "fn retry_with_backoff() { compute_jitter(); }",
        );
        let jitter = chunk_with_refs(
            "src/store/jitter.rs",
            1,
            10,
            &["compute_jitter"],
            &[],
            "fn compute_jitter() -> u32 { 0 }",
        );
        let store = FakeStore::new(vec![retry, jitter]);
        let r = Retriever::new(&store, None, RetrieverConfig::default());
        let out = r.query("repo", "fix retry_with_backoff flow").unwrap();
        assert!(out.spans.iter().any(|s| s.path == "src/store/retry.rs"));
        assert!(!out.spans.iter().any(|s| s.path == "src/store/jitter.rs"));
        assert!(
            out.related
                .iter()
                .any(|rel| rel.path == "src/store/jitter.rs"
                    && rel.relation == "defines `compute_jitter` (used by #1)"),
            "related: {:?}",
            out.related
        );
    }

    #[test]
    fn query_lists_definition_and_use_lines_of_task_identifiers() {
        let def = chunk_with_refs(
            "src/shard/recovery.rs",
            10,
            14,
            &["recover_shard_v3"],
            &[],
            "/// Replays the WAL.\npub fn recover_shard_v3(dir: &Path) -> Lsn {\n    todo!()\n}\n",
        );
        let caller = chunk_with_refs(
            "src/shard/mod.rs",
            200,
            204,
            &["open_shard"],
            &["recover_shard_v3"],
            "fn open_shard(dir: &Path) {\n    let cfg = load();\n    let lsn = recover_shard_v3(&dir)?;\n    start(lsn);\n}\n",
        );
        let store = FakeStore::new(vec![def, caller]);
        let r = Retriever::new(&store, None, RetrieverConfig::default());
        let out = r
            .query("repo", "wire recover_shard_v3 to honor target_lsn")
            .unwrap();
        let line = |path: &str, n: u32| {
            out.related
                .iter()
                .find(|x| x.path == path && x.start_line == n && x.end_line == n)
        };
        let d = line("src/shard/recovery.rs", 11)
            .unwrap_or_else(|| panic!("definition line missing: {:?}", out.related));
        assert_eq!(d.symbol, "pub fn recover_shard_v3(dir: &Path) -> Lsn {");
        assert_eq!(d.relation, "definition of `recover_shard_v3`");
        let u = line("src/shard/mod.rs", 202)
            .unwrap_or_else(|| panic!("use line missing: {:?}", out.related));
        assert_eq!(u.symbol, "let lsn = recover_shard_v3(&dir)?;");
        assert_eq!(u.relation, "use of `recover_shard_v3`");
    }

    #[test]
    fn max_related_zero_disables_expansion_in_query() {
        let retry = chunk_with_refs(
            "src/store/retry.rs",
            1,
            20,
            &["retry_with_backoff"],
            &["compute_jitter"],
            "fn retry_with_backoff() { compute_jitter(); }",
        );
        let jitter = chunk_with_refs(
            "src/store/jitter.rs",
            1,
            10,
            &["compute_jitter"],
            &[],
            "fn compute_jitter() -> u32 { 0 }",
        );
        let store = FakeStore::new(vec![retry, jitter]);
        let cfg = RetrieverConfig {
            max_related: 0,
            ..RetrieverConfig::default()
        };
        let r = Retriever::new(&store, None, cfg);
        let out = r.query("repo", "fix retry_with_backoff flow").unwrap();
        assert!(out.related.is_empty());
    }

    #[test]
    fn expansion_store_error_does_not_fail_the_query() {
        let retry = chunk_with_refs(
            "src/store/retry.rs",
            1,
            20,
            &["retry_with_backoff"],
            &[],
            "fn retry_with_backoff() {}",
        );
        let inner = FakeStore::new(vec![retry]);
        let store = FailingReferencingStore(inner);
        let r = Retriever::new(&store, None, RetrieverConfig::default());
        let out = r.query("repo", "fix retry_with_backoff flow").unwrap();
        assert!(!out.spans.is_empty());
        assert!(out.related.is_empty());
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
