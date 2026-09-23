//! In-memory `Store`/`Scorer` fakes for `laya-rank` unit tests. Not part of the public API.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use laya_core::{Chunk, Error, Result, Scorer, Store, ident};

/// A tiny in-memory `Store`: BM25 is a deterministic term-substring count (not real BM25, but
/// ranked and reproducible, which is all these tests need), `chunks_defining` matches
/// `Chunk::defines` exactly, and `list_files` returns the configured file list.
#[derive(Default)]
pub struct FakeStore {
    pub chunks: Vec<Chunk>,
    pub files: Vec<String>,
    pub list_files_calls: AtomicUsize,
}

impl FakeStore {
    pub fn new(chunks: Vec<Chunk>) -> Self {
        let files: Vec<String> = {
            let mut fs: Vec<String> = chunks.iter().map(|c| c.path.clone()).collect();
            fs.sort();
            fs.dedup();
            fs
        };
        Self {
            chunks,
            files,
            list_files_calls: AtomicUsize::new(0),
        }
    }
}

impl Store for FakeStore {
    fn ensure_index(&self, _repo_id: &str) -> Result<()> {
        Ok(())
    }
    fn put_file(
        &self,
        _repo_id: &str,
        _path: &str,
        _file_hash: &str,
        _chunks: &[Chunk],
    ) -> Result<()> {
        Ok(())
    }
    fn delete_file(&self, _repo_id: &str, _path: &str) -> Result<()> {
        Ok(())
    }
    fn file_hash(&self, _repo_id: &str, _path: &str) -> Result<Option<String>> {
        Ok(None)
    }
    fn list_files(&self, _repo_id: &str) -> Result<Vec<String>> {
        self.list_files_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.files.clone())
    }
    fn bm25(&self, _repo_id: &str, terms: &[String], limit: usize) -> Result<Vec<(String, f32)>> {
        // Word-tokenized overlap (not raw substring containment, which would false-positive on
        // things like "of" inside "backoff") — good enough to stand in for Moon's real BM25 in
        // unit tests, while still being a ranked, order-sensitive signal.
        let mut scored: Vec<(String, f32)> = self
            .chunks
            .iter()
            .filter_map(|c| {
                let haystack: std::collections::HashSet<String> = ident::terms(&c.path)
                    .into_iter()
                    .chain(ident::terms(&c.text))
                    .collect();
                let score: f32 = terms
                    .iter()
                    .filter(|t| !t.is_empty() && haystack.contains(t.as_str()))
                    .count() as f32;
                (score > 0.0).then(|| (c.id(), score))
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored)
    }
    fn chunks_defining(
        &self,
        _repo_id: &str,
        idents: &[String],
        limit: usize,
    ) -> Result<Vec<String>> {
        let mut ids: Vec<String> = self
            .chunks
            .iter()
            .filter(|c| c.defines.iter().any(|d| idents.contains(d)))
            .map(|c| c.id())
            .collect();
        ids.truncate(limit);
        Ok(ids)
    }
    fn chunks_referencing(
        &self,
        _repo_id: &str,
        idents: &[String],
        limit: usize,
    ) -> Result<Vec<String>> {
        // Contract: "ordered by how many of `idents` they reference (desc), then id."
        let mut scored: Vec<(String, usize)> = self
            .chunks
            .iter()
            .filter_map(|c| {
                let count = c.refs.iter().filter(|r| idents.contains(r)).count();
                (count > 0).then(|| (c.id(), count))
            })
            .collect();
        scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        scored.truncate(limit);
        Ok(scored.into_iter().map(|(id, _)| id).collect())
    }
    fn get_chunks(&self, _repo_id: &str, ids: &[String]) -> Result<Vec<Chunk>> {
        Ok(self
            .chunks
            .iter()
            .filter(|c| ids.contains(&c.id()))
            .cloned()
            .collect())
    }
    fn memo_get(&self, _key: &str) -> Result<Option<String>> {
        Ok(None)
    }
    fn memo_put(&self, _key: &str, _value: &str, _ttl_secs: u64) -> Result<()> {
        Ok(())
    }
}

/// Wraps a [`FakeStore`] but fails `chunks_referencing`, to exercise the one-hop expansion's
/// fail-open path (§6 of the related-expansion brief) without disturbing candidate generation,
/// which never calls `chunks_referencing`.
pub struct FailingReferencingStore(pub FakeStore);

impl Store for FailingReferencingStore {
    fn ensure_index(&self, repo_id: &str) -> Result<()> {
        self.0.ensure_index(repo_id)
    }
    fn put_file(&self, repo_id: &str, path: &str, file_hash: &str, chunks: &[Chunk]) -> Result<()> {
        self.0.put_file(repo_id, path, file_hash, chunks)
    }
    fn delete_file(&self, repo_id: &str, path: &str) -> Result<()> {
        self.0.delete_file(repo_id, path)
    }
    fn file_hash(&self, repo_id: &str, path: &str) -> Result<Option<String>> {
        self.0.file_hash(repo_id, path)
    }
    fn list_files(&self, repo_id: &str) -> Result<Vec<String>> {
        self.0.list_files(repo_id)
    }
    fn bm25(&self, repo_id: &str, terms: &[String], limit: usize) -> Result<Vec<(String, f32)>> {
        self.0.bm25(repo_id, terms, limit)
    }
    fn chunks_defining(
        &self,
        repo_id: &str,
        idents: &[String],
        limit: usize,
    ) -> Result<Vec<String>> {
        self.0.chunks_defining(repo_id, idents, limit)
    }
    fn chunks_referencing(
        &self,
        _repo_id: &str,
        _idents: &[String],
        _limit: usize,
    ) -> Result<Vec<String>> {
        Err(Error::StoreUnavailable("moon down".into()))
    }
    fn get_chunks(&self, repo_id: &str, ids: &[String]) -> Result<Vec<Chunk>> {
        self.0.get_chunks(repo_id, ids)
    }
    fn memo_get(&self, key: &str) -> Result<Option<String>> {
        self.0.memo_get(key)
    }
    fn memo_put(&self, key: &str, value: &str, ttl_secs: u64) -> Result<()> {
        self.0.memo_put(key, value, ttl_secs)
    }
}

/// A `Store` whose `bm25` call always fails, to exercise error propagation from candidate
/// generation.
pub struct FailingStore;
impl Store for FailingStore {
    fn ensure_index(&self, _repo_id: &str) -> Result<()> {
        Ok(())
    }
    fn put_file(
        &self,
        _repo_id: &str,
        _path: &str,
        _file_hash: &str,
        _chunks: &[Chunk],
    ) -> Result<()> {
        Ok(())
    }
    fn delete_file(&self, _repo_id: &str, _path: &str) -> Result<()> {
        Ok(())
    }
    fn file_hash(&self, _repo_id: &str, _path: &str) -> Result<Option<String>> {
        Ok(None)
    }
    fn list_files(&self, _repo_id: &str) -> Result<Vec<String>> {
        Ok(vec![])
    }
    fn bm25(&self, _repo_id: &str, _terms: &[String], _limit: usize) -> Result<Vec<(String, f32)>> {
        Err(Error::StoreUnavailable("moon down".into()))
    }
    fn chunks_defining(
        &self,
        _repo_id: &str,
        _idents: &[String],
        _limit: usize,
    ) -> Result<Vec<String>> {
        Ok(vec![])
    }
    fn get_chunks(&self, _repo_id: &str, _ids: &[String]) -> Result<Vec<Chunk>> {
        Ok(vec![])
    }
    fn memo_get(&self, _key: &str) -> Result<Option<String>> {
        Ok(None)
    }
    fn memo_put(&self, _key: &str, _value: &str, _ttl_secs: u64) -> Result<()> {
        Ok(())
    }
}

/// Scores chunks by an id -> probability map (falling back to `default` for unlisted ids).
pub struct ProbScorer {
    pub probs_by_id: HashMap<String, f32>,
    pub default: f32,
    pub calls: Mutex<usize>,
}

impl ProbScorer {
    pub fn new(probs_by_id: HashMap<String, f32>) -> Self {
        Self {
            probs_by_id,
            default: 0.0,
            calls: Mutex::new(0),
        }
    }
}

impl Scorer for ProbScorer {
    fn score(&self, _task: &str, chunks: &[&Chunk]) -> Result<Vec<f32>> {
        *self.calls.lock().unwrap() += 1;
        Ok(chunks
            .iter()
            .map(|c| *self.probs_by_id.get(&c.id()).unwrap_or(&self.default))
            .collect())
    }
}

/// Wraps another scorer but sleeps first, to exercise the Laya-gate timeout path.
pub struct SleepyScorer {
    pub sleep: Duration,
    pub inner: ProbScorer,
}

impl Scorer for SleepyScorer {
    fn score(&self, task: &str, chunks: &[&Chunk]) -> Result<Vec<f32>> {
        thread::sleep(self.sleep);
        self.inner.score(task, chunks)
    }
}

/// Always fails, to exercise the Laya-gate error path.
pub struct FailingScorer;
impl Scorer for FailingScorer {
    fn score(&self, _task: &str, _chunks: &[&Chunk]) -> Result<Vec<f32>> {
        Err(Error::Model("boom".into()))
    }
}

/// Returns a probability vector of the wrong length, to exercise the defensive length check.
pub struct WrongLengthScorer;
impl Scorer for WrongLengthScorer {
    fn score(&self, _task: &str, _chunks: &[&Chunk]) -> Result<Vec<f32>> {
        Ok(vec![0.9])
    }
}
