//! [`MoonStore`]: `laya_core::Store` over Moon.

use std::collections::{HashMap, HashSet};

use laya_core::{Chunk, Error, Lang, Result, Store, ident};
use redis::{RedisResult, Value};

use crate::breaker::BreakerState;
use crate::config::StoreConfig;
use crate::conn::{Executor, OpKind};
use crate::keys;
use crate::query::{
    fuse_scores, is_benign_term_error, parse_search_reply, prepare_terms, select_terms,
};

/// Candidate terms considered per bm25 call before document-frequency selection.
const MAX_CANDIDATE_TERMS: usize = 256;

/// Separator for identifier lists inside hashes. Identifiers never contain a newline, while
/// some languages allow `,`-bearing names (`operator,`).
const IDENT_SEP: char = '\n';

/// Moon-backed store. Cheap to share (`Arc<MoonStore>`); all methods take `&self`.
///
/// Construction does no IO: a dead sidecar surfaces as `Error::StoreUnavailable` on use, so
/// callers can fail open.
///
/// Concurrency: every method is safe to call from many threads. Writes to *different* paths may
/// run concurrently; `put_file`/`delete_file` for the *same* path must be serialized by the
/// caller (read-modify-write of the file record, no server-side transaction, see MOON_NOTES #7).
pub struct MoonStore {
    exec: Executor,
}

impl MoonStore {
    pub fn new(cfg: StoreConfig) -> Result<Self> {
        Ok(Self {
            exec: Executor::new(cfg)?,
        })
    }

    #[must_use]
    pub fn config(&self) -> &StoreConfig {
        self.exec.config()
    }

    #[must_use]
    pub fn breaker_state(&self) -> BreakerState {
        self.exec.breaker_state()
    }

    /// Read `chunks`, `defines` and `refs` of a file record.
    fn file_record(&self, repo: &str, path: &str) -> Result<FileRecord> {
        let key = keys::file(repo, path);
        let (chunks, defs, refs): (Option<String>, Option<String>, Option<String>) =
            self.exec.run(OpKind::Bulk, |c| {
                redis::cmd("HMGET")
                    .arg(&key)
                    .arg("chunks")
                    .arg("defines")
                    .arg("refs")
                    .query(c)
            })?;
        let chunks = chunks
            .as_deref()
            .map(|s| keys::split_list(s).map(str::to_string).collect())
            .unwrap_or_default();
        Ok(FileRecord {
            ids: chunks,
            defines: defs.as_deref().map(split_idents).unwrap_or_default(),
            refs: refs.as_deref().map(split_idents).unwrap_or_default(),
        })
    }

    /// Number of chunks defining each identifier (0 = unknown), aligned with `idents`.
    /// Lets rank expansion tell unique definitions from ambiguous ones (`new`, `run`, ...).
    /// One pipelined `SCARD` round trip.
    pub fn definition_counts(&self, repo_id: &str, idents: &[String]) -> Result<Vec<usize>> {
        if idents.is_empty() {
            return Ok(Vec::new());
        }
        let mut pipe = redis::pipe();
        for i in idents {
            pipe.cmd("SCARD").arg(keys::defines(repo_id, i));
        }
        self.exec.run(OpKind::Query, |c| pipe.query(c))
    }

    /// Chunks whose ident set (`key(ident)`) contains any of `idents`, with
    /// `(match count, index of the first requested ident that matched)`.
    /// One pipelined `SMEMBERS` round trip; duplicates and empty idents are ignored.
    fn ident_matches(
        &self,
        idents: &[String],
        key: impl Fn(&str) -> String,
    ) -> Result<Vec<(String, (usize, usize))>> {
        let mut seen = HashSet::new();
        let idents: Vec<&String> = idents
            .iter()
            .filter(|i| !i.is_empty() && seen.insert(i.as_str()))
            .collect();
        if idents.is_empty() {
            return Ok(Vec::new());
        }
        let mut pipe = redis::pipe();
        for i in &idents {
            pipe.cmd("SMEMBERS").arg(key(i));
        }
        let sets: Vec<Vec<String>> = self.exec.run(OpKind::Query, |c| pipe.query(c))?;
        let mut acc: HashMap<String, (usize, usize)> = HashMap::new();
        for (order, set) in sets.into_iter().enumerate() {
            for id in set {
                acc.entry(id).and_modify(|e| e.0 += 1).or_insert((1, order));
            }
        }
        Ok(acc.into_iter().collect())
    }

    /// Indexed `terms` of stored chunks (`None` for missing ones), for df bookkeeping.
    fn stored_terms(&self, repo: &str, ids: &[&str]) -> Result<Vec<Option<String>>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut pipe = redis::pipe();
        for id in ids {
            pipe.cmd("HGET").arg(keys::chunk(repo, id)).arg("terms");
        }
        self.exec.run(OpKind::Bulk, |c| pipe.query(c))
    }

    /// Queue removal of chunks from a pipeline. Moon's `DEL` does not remove a hash from its text
    /// index (see MOON_NOTES.md), so the indexed field is blanked first: the `HSET` re-index drops
    /// the postings, then `DEL` removes the data.
    fn queue_chunk_removal(pipe: &mut redis::Pipeline, repo: &str, ids: &[&str], old: &FileRecord) {
        for id in ids {
            let k = keys::chunk(repo, id);
            pipe.cmd("HSET").arg(&k).arg("terms").arg("").ignore();
            pipe.cmd("DEL").arg(&k).ignore();
        }
        queue_set_removal(pipe, repo, ids, old);
    }
}

/// What a file record says about the chunks currently stored for a path.
#[derive(Debug, Default)]
struct FileRecord {
    ids: Vec<String>,
    /// Union of the `defines` of those chunks.
    defines: Vec<String>,
    /// Union of the `refs` of those chunks.
    refs: Vec<String>,
}

/// Queue `SREM ids` from every definition and reference set the record lists.
fn queue_set_removal(pipe: &mut redis::Pipeline, repo: &str, ids: &[&str], old: &FileRecord) {
    if ids.is_empty() {
        return;
    }
    for d in &old.defines {
        pipe.cmd("SREM")
            .arg(keys::defines(repo, d))
            .arg(ids)
            .ignore();
    }
    for r in &old.refs {
        pipe.cmd("SREM").arg(keys::refs(repo, r)).arg(ids).ignore();
    }
}

/// Valid identifiers of a list (non-empty, no separator), deduplicated in order.
fn clean_idents(v: &[String]) -> Vec<&str> {
    let mut seen = HashSet::new();
    v.iter()
        .map(String::as_str)
        .filter(|d| !d.is_empty() && !d.contains(IDENT_SEP) && seen.insert(*d))
        .collect()
}

/// Sort `(id, (count, first))` by count desc, then `first` asc if `by_first`, then id asc.
fn rank_matches(mut v: Vec<(String, (usize, usize))>, by_first: bool, limit: usize) -> Vec<String> {
    v.sort_by(|a, b| {
        let o = b.1.0.cmp(&a.1.0);
        let o = if by_first {
            o.then(a.1.1.cmp(&b.1.1))
        } else {
            o
        };
        o.then_with(|| a.0.cmp(&b.0))
    });
    v.into_iter().take(limit).map(|(id, _)| id).collect()
}

/// Add `sign` to the document frequency of every distinct term of one chunk.
fn count_terms(deltas: &mut HashMap<String, i64>, terms: &str, sign: i64) {
    let distinct: HashSet<&str> = terms.split_ascii_whitespace().collect();
    for t in distinct {
        *deltas.entry(t.to_string()).or_insert(0) += sign;
    }
}

/// Queue net document-frequency changes (zero deltas are skipped).
fn queue_df_deltas(pipe: &mut redis::Pipeline, repo: &str, deltas: &HashMap<String, i64>) {
    let key = keys::df(repo);
    for (t, d) in deltas {
        if *d != 0 {
            pipe.cmd("HINCRBY").arg(&key).arg(t).arg(*d).ignore();
        }
    }
}

fn split_idents(s: &str) -> Vec<String> {
    s.split(IDENT_SEP)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

fn lang_from_str(s: &str) -> Lang {
    match s {
        "rust" => Lang::Rust,
        "python" => Lang::Python,
        "typescript" => Lang::TypeScript,
        "tsx" => Lang::Tsx,
        "javascript" => Lang::JavaScript,
        "go" => Lang::Go,
        "java" => Lang::Java,
        "c" => Lang::C,
        "cpp" => Lang::Cpp,
        "csharp" => Lang::CSharp,
        "ruby" => Lang::Ruby,
        "php" => Lang::Php,
        "kotlin" => Lang::Kotlin,
        "swift" => Lang::Swift,
        _ => Lang::Text,
    }
}

/// Indexed text of a chunk: normalized terms of path + symbol + body, space-joined, with
/// repetitions kept (up to `max_tf` per term, 0 = unlimited) so term frequency counts.
fn index_terms(c: &Chunk, max_tf: u32) -> String {
    let mut s = String::with_capacity(c.text.len());
    let mut tf: HashMap<String, u32> = HashMap::new();
    for src in [c.path.as_str(), c.symbol.as_str(), c.text.as_str()] {
        for t in ident::terms(src) {
            if max_tf > 0 {
                let n = tf.entry(t.clone()).or_insert(0);
                if *n >= max_tf {
                    continue;
                }
                *n += 1;
            }
            if !s.is_empty() {
                s.push(' ');
            }
            s.push_str(&t);
        }
    }
    s
}

fn chunk_from_hash(mut h: HashMap<String, String>) -> Option<Chunk> {
    let mut take = |k: &str| h.remove(k);
    Some(Chunk {
        path: take("path")?,
        start_line: take("start")?.parse().ok()?,
        end_line: take("end")?.parse().ok()?,
        lang: lang_from_str(&take("lang").unwrap_or_default()),
        symbol: take("symbol").unwrap_or_default(),
        kind: take("kind").unwrap_or_default(),
        defines: take("defines")
            .as_deref()
            .map(split_idents)
            .unwrap_or_default(),
        refs: take("refs")
            .as_deref()
            .map(split_idents)
            .unwrap_or_default(),
        text: take("text").unwrap_or_default(),
    })
}

fn server_error_msg(v: &Value) -> Option<String> {
    match v {
        Value::ServerError(e) => Some(format!("{} {}", e.code(), e.details().unwrap_or_default())),
        _ => None,
    }
}

impl Store for MoonStore {
    fn ensure_index(&self, repo_id: &str) -> Result<()> {
        let (idx, prefix) = (keys::index(repo_id), keys::chunk_prefix(repo_id));
        self.exec.run(OpKind::Query, |c| {
            let r: RedisResult<()> = redis::cmd("FT.CREATE")
                .arg(&idx)
                .arg(&["ON", "HASH", "PREFIX", "1"])
                .arg(&prefix)
                .arg(&["SCHEMA", "terms", "TEXT"])
                .query(c);
            index_ready(r, || {
                match redis::cmd("FT.INFO").arg(&idx).query::<redis::Value>(c) {
                    Ok(_) => Ok(true),
                    Err(e) if e.to_string().to_ascii_lowercase().contains("unknown index") => {
                        Ok(false)
                    }
                    Err(e) => Err(e),
                }
            })
        })
    }

    fn put_file(&self, repo_id: &str, path: &str, file_hash: &str, chunks: &[Chunk]) -> Result<()> {
        let old = self.file_record(repo_id, path)?;
        let old_ids = &old.ids;

        let mut new_ids: Vec<String> = Vec::with_capacity(chunks.len());
        let mut seen = HashSet::new();
        let mut new_chunks = Vec::with_capacity(chunks.len());
        for c in chunks {
            let id = c.id();
            if seen.insert(id.clone()) {
                new_ids.push(id);
                new_chunks.push(c);
            }
        }
        let removed: Vec<&str> = old_ids
            .iter()
            .map(String::as_str)
            .filter(|id| !seen.contains(*id))
            .collect();

        // Document frequencies change only for chunks that appear or disappear; content-addressed
        // ids make unchanged chunks cancel out.
        let old_set: HashSet<&str> = old_ids.iter().map(String::as_str).collect();
        let new_terms: Vec<String> = new_chunks
            .iter()
            .map(|c| index_terms(c, self.exec.config().max_tf))
            .collect();
        let mut deltas: HashMap<String, i64> = HashMap::new();
        for t in self.stored_terms(repo_id, &removed)?.iter().flatten() {
            count_terms(&mut deltas, t, -1);
        }
        for (id, t) in new_ids.iter().zip(&new_terms) {
            if !old_set.contains(id.as_str()) {
                count_terms(&mut deltas, t, 1);
            }
        }

        let mut pipe = redis::pipe();
        // 1. Drop chunks that disappeared, and the definition/reference memberships of every old
        //    chunk (kept chunks are re-added below; SREM before SADD is order-safe in a pipeline).
        Self::queue_chunk_removal(&mut pipe, repo_id, &removed, &FileRecord::default());
        let all_old: Vec<&str> = old_ids.iter().map(String::as_str).collect();
        queue_set_removal(&mut pipe, repo_id, &all_old, &old);
        // 2. Write the new chunks (content-addressed: an unchanged chunk is rewritten in place).
        let mut file_defs: Vec<&str> = Vec::new();
        let mut def_seen = HashSet::new();
        let mut file_refs: Vec<&str> = Vec::new();
        let mut ref_seen = HashSet::new();
        for ((id, c), terms) in new_ids.iter().zip(&new_chunks).zip(&new_terms) {
            let defs = clean_idents(&c.defines);
            let refs = clean_idents(&c.refs);
            pipe.cmd("HSET")
                .arg(keys::chunk(repo_id, id))
                .arg("path")
                .arg(&c.path)
                .arg("start")
                .arg(c.start_line)
                .arg("end")
                .arg(c.end_line)
                .arg("lang")
                .arg(c.lang.as_str())
                .arg("symbol")
                .arg(&c.symbol)
                .arg("kind")
                .arg(&c.kind)
                .arg("defines")
                .arg(defs.join("\n"))
                .arg("refs")
                .arg(refs.join("\n"))
                .arg("terms")
                .arg(terms)
                .arg("text")
                .arg(&c.text)
                .ignore();
            for d in defs {
                pipe.cmd("SADD")
                    .arg(keys::defines(repo_id, d))
                    .arg(id)
                    .ignore();
                if def_seen.insert(d) {
                    file_defs.push(d);
                }
            }
            for r in refs {
                pipe.cmd("SADD")
                    .arg(keys::refs(repo_id, r))
                    .arg(id)
                    .ignore();
                if ref_seen.insert(r) {
                    file_refs.push(r);
                }
            }
        }
        queue_df_deltas(&mut pipe, repo_id, &deltas);
        // 3. File record last: if anything above fails, the stale hash makes the indexer retry.
        pipe.cmd("HSET")
            .arg(keys::file(repo_id, path))
            .arg("hash")
            .arg(file_hash)
            .arg("chunks")
            .arg(new_ids.join(","))
            .arg("defines")
            .arg(file_defs.join("\n"))
            .arg("refs")
            .arg(file_refs.join("\n"))
            .ignore();
        pipe.cmd("SADD")
            .arg(keys::files(repo_id))
            .arg(path)
            .ignore();

        // Every command except HINCRBY is idempotent, so a retried pipeline converges to the same
        // state; a replayed HINCRBY only skews query planning (df), never stored data or scores.
        self.exec.run(OpKind::Bulk, |c| pipe.query::<()>(c))
    }

    fn delete_file(&self, repo_id: &str, path: &str) -> Result<()> {
        let old = self.file_record(repo_id, path)?;
        let ids: Vec<&str> = old.ids.iter().map(String::as_str).collect();
        let mut deltas: HashMap<String, i64> = HashMap::new();
        for t in self.stored_terms(repo_id, &ids)?.iter().flatten() {
            count_terms(&mut deltas, t, -1);
        }
        let mut pipe = redis::pipe();
        Self::queue_chunk_removal(&mut pipe, repo_id, &ids, &old);
        queue_df_deltas(&mut pipe, repo_id, &deltas);
        pipe.cmd("DEL").arg(keys::file(repo_id, path)).ignore();
        pipe.cmd("SREM")
            .arg(keys::files(repo_id))
            .arg(path)
            .ignore();
        self.exec.run(OpKind::Bulk, |c| pipe.query::<()>(c))
    }

    fn file_hash(&self, repo_id: &str, path: &str) -> Result<Option<String>> {
        let key = keys::file(repo_id, path);
        self.exec.run(OpKind::Query, |c| {
            redis::cmd("HGET").arg(&key).arg("hash").query(c)
        })
    }

    fn list_files(&self, repo_id: &str) -> Result<Vec<String>> {
        let key = keys::files(repo_id);
        let mut v: Vec<String> = self
            .exec
            .run(OpKind::Query, |c| redis::cmd("SMEMBERS").arg(&key).query(c))?;
        v.sort_unstable();
        Ok(v)
    }

    fn bm25(&self, repo_id: &str, terms: &[String], limit: usize) -> Result<Vec<(String, f32)>> {
        let cfg = self.exec.config();
        let candidates = prepare_terms(terms, MAX_CANDIDATE_TERMS);
        if candidates.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        // Plan: one HMGET of document frequencies, then search only the rarest terms that fit
        // the cost budget. Terms never indexed are dropped without a search.
        let df_key = keys::df(repo_id);
        let dfs: Vec<Option<i64>> = self.exec.run(OpKind::Query, |c| {
            redis::cmd("HMGET").arg(&df_key).arg(&candidates).query(c)
        })?;
        let terms = select_terms(&candidates, &dfs, cfg.max_terms, cfg.df_sq_budget);
        if terms.is_empty() {
            tracing::debug!(candidates = candidates.len(), "bm25: no term within budget");
            return Ok(Vec::new());
        }
        let per_term = limit.max(cfg.per_term_limit);
        let idx = keys::index(repo_id);
        let mut pipe = redis::pipe();
        pipe.ignore_errors();
        for t in &terms {
            pipe.cmd("FT.SEARCH")
                .arg(&idx)
                .arg(t)
                .arg("NOCONTENT")
                .arg("LIMIT")
                .arg(0)
                .arg(per_term);
        }
        let replies: Vec<Value> = self.exec.run(OpKind::Query, |c| pipe.query(c))?;

        let prefix = keys::chunk_prefix(repo_id);
        let mut per_term_hits = Vec::with_capacity(replies.len());
        let mut hard_errors = Vec::new();
        for (t, v) in terms.iter().zip(&replies) {
            if let Some(msg) = server_error_msg(v) {
                if !is_benign_term_error(&msg) {
                    tracing::warn!(term = %t, error = %msg, "FT.SEARCH term failed");
                    hard_errors.push(msg);
                }
                continue;
            }
            let hits = parse_search_reply(v).map_err(Error::Store)?;
            per_term_hits.push(
                hits.into_iter()
                    .filter_map(|(k, s)| k.strip_prefix(&prefix).map(|id| (id.to_string(), s)))
                    .collect::<Vec<_>>(),
            );
        }
        if per_term_hits.is_empty()
            && let Some(first) = hard_errors.into_iter().next()
        {
            return Err(Error::Store(first));
        }
        Ok(fuse_scores(&per_term_hits, limit))
    }

    fn chunks_defining(
        &self,
        repo_id: &str,
        idents: &[String],
        limit: usize,
    ) -> Result<Vec<String>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        // Rank by how many requested identifiers a chunk defines, then the first requested
        // ident it matched, then id: fully deterministic.
        let m = self.ident_matches(idents, |i| keys::defines(repo_id, i))?;
        Ok(rank_matches(m, true, limit))
    }

    /// Callers/users of `idents`: chunks whose `refs` contain any of them, ordered by how many
    /// they reference (desc), then id (asc). One pipelined `SMEMBERS` round trip.
    fn chunks_referencing(
        &self,
        repo_id: &str,
        idents: &[String],
        limit: usize,
    ) -> Result<Vec<String>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let m = self.ident_matches(idents, |i| keys::refs(repo_id, i))?;
        Ok(rank_matches(m, false, limit))
    }

    fn get_chunks(&self, repo_id: &str, ids: &[String]) -> Result<Vec<Chunk>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut pipe = redis::pipe();
        for id in ids {
            pipe.cmd("HGETALL").arg(keys::chunk(repo_id, id));
        }
        let maps: Vec<HashMap<String, String>> = self.exec.run(OpKind::Query, |c| pipe.query(c))?;
        Ok(maps
            .into_iter()
            .zip(ids)
            .filter(|(m, _)| !m.is_empty())
            .filter_map(|(m, id)| {
                let c = chunk_from_hash(m);
                if c.is_none() {
                    tracing::warn!(chunk = %id, "skipping malformed chunk hash");
                }
                c
            })
            .collect())
    }

    /// The file record's chunk list, then one pipelined `HGETALL`: two round trips.
    fn chunks_of_file(&self, repo_id: &str, path: &str) -> Result<Vec<Chunk>> {
        let ids = self.file_record(repo_id, path)?.ids;
        let mut chunks = self.get_chunks(repo_id, &ids)?;
        chunks.sort_by_key(|c| (c.start_line, c.end_line));
        Ok(chunks)
    }

    fn memo_get(&self, key: &str) -> Result<Option<String>> {
        let key = keys::memo(key);
        self.exec
            .run(OpKind::Query, |c| redis::cmd("GET").arg(&key).query(c))
    }

    fn memo_put(&self, key: &str, value: &str, ttl_secs: u64) -> Result<()> {
        let key = keys::memo(key);
        self.exec.run(OpKind::Query, |c| {
            let mut cmd = redis::cmd("SET");
            cmd.arg(&key).arg(value);
            if ttl_secs > 0 {
                cmd.arg("EX").arg(ttl_secs);
            }
            cmd.query(c)
        })
    }
}

/// Whether `FT.CREATE`'s outcome leaves the index usable. "Already exists" is fine. When Moon
/// has paused writes (low-disk guard: `MOONERR diskfull`), the create is rejected before Moon
/// looks for the index, so `exists` (a read, `FT.INFO`) decides: an existing index can still
/// serve queries. Otherwise the error stands.
fn index_ready(
    create: RedisResult<()>,
    exists: impl FnOnce() -> RedisResult<bool>,
) -> RedisResult<()> {
    match create {
        Err(e)
            if e.to_string()
                .to_ascii_lowercase()
                .contains("already exists") =>
        {
            Ok(())
        }
        Err(e) if is_writes_paused(&e.to_string()) => match exists() {
            Ok(true) => Ok(()),
            _ => Err(e),
        },
        other => other,
    }
}

/// Moon's reply when its low-disk guard has paused writes.
pub fn is_writes_paused(message: &str) -> bool {
    message.to_ascii_lowercase().contains("diskfull")
}

/// A non-transient Moon reply as a store error. Paused writes read as one short message: a
/// pipelined write otherwise repeats Moon's reply once per command.
pub(crate) fn store_error(e: &redis::RedisError) -> Error {
    let message = e.to_string();
    if is_writes_paused(&message) {
        return Error::Store(
            "Moon has paused writes because its disk is nearly full (MOONERR diskfull); free \
             disk space, or see `laya-codex doctor`"
                .into(),
        );
    }
    Error::Store(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diskfull() -> redis::RedisError {
        redis::make_extension_error(
            "MOONERR".into(),
            Some("diskfull: writes paused until free space recovers".into()),
        )
    }

    #[test]
    fn paused_writes_read_as_one_short_error() {
        let e = store_error(&diskfull());
        assert_eq!(
            e.to_string(),
            "store error: Moon has paused writes because its disk is nearly full (MOONERR \
             diskfull); free disk space, or see `laya-codex doctor`"
        );
        let other = redis::make_extension_error("ERR".into(), Some("syntax error".into()));
        assert!(store_error(&other).to_string().contains("syntax error"));
    }

    #[test]
    fn an_existing_index_is_ready_while_moon_pauses_writes() {
        assert!(index_ready(Err(diskfull()), || Ok(true)).is_ok());
    }

    #[test]
    fn a_missing_index_still_reports_the_paused_writes() {
        let e = index_ready(Err(diskfull()), || Ok(false)).unwrap_err();
        assert!(e.to_string().contains("diskfull"), "{e}");
    }

    #[test]
    fn index_creation_outcomes_other_than_paused_writes_are_unchanged() {
        let never = || -> RedisResult<bool> { panic!("no existence check needed") };
        assert!(index_ready(Ok(()), never).is_ok());
        let exists = redis::make_extension_error("ERR".into(), Some("Index already exists".into()));
        assert!(index_ready(Err(exists), never).is_ok());
        let other = redis::make_extension_error("ERR".into(), Some("syntax error".into()));
        assert!(index_ready(Err(other), never).is_err());
    }

    #[test]
    fn index_terms_cover_path_symbol_and_text_with_tf() {
        let c = Chunk {
            path: "src/wal_writer.rs".into(),
            start_line: 1,
            end_line: 1,
            lang: Lang::Rust,
            symbol: "impl WalWriter".into(),
            kind: "impl_item".into(),
            defines: vec![],
            refs: vec![],
            text: "flush flush".into(),
        };
        let t = index_terms(&c, 0);
        assert!(t.contains("wal_writer") && t.contains("walwriter"));
        assert_eq!(t.matches("flush").count(), 2);
    }

    #[test]
    fn index_terms_caps_term_frequency() {
        let c = Chunk {
            path: "a.rs".into(),
            start_line: 1,
            end_line: 1,
            lang: Lang::Rust,
            symbol: String::new(),
            kind: String::new(),
            defines: vec![],
            refs: vec![],
            text: "flush flush flush flush other".into(),
        };
        assert_eq!(index_terms(&c, 2), "rs flush flush other");
        assert_eq!(index_terms(&c, 1), "rs flush other");
        assert_eq!(
            index_terms(&c, 0).matches("flush").count(),
            4,
            "0 = unlimited"
        );
    }

    #[test]
    fn lang_roundtrips_through_as_str() {
        for l in [Lang::Rust, Lang::Tsx, Lang::CSharp, Lang::Text, Lang::Swift] {
            assert_eq!(lang_from_str(l.as_str()), l);
        }
    }

    #[test]
    fn malformed_hash_is_rejected() {
        let mut h = HashMap::new();
        h.insert("path".to_string(), "a".to_string());
        h.insert("start".to_string(), "x".to_string());
        h.insert("end".to_string(), "1".to_string());
        assert!(chunk_from_hash(h).is_none());
    }
}
