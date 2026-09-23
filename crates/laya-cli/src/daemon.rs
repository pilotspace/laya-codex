//! `laya daemon`: long-lived process that keeps Moon supervised, the Laya model warm and
//! per-session state in memory. Clients speak the JSON-lines protocol in `protocol.rs`.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use laya_core::QueryResult;
use laya_core::{Chunk, Scorer, Store};
use laya_rank::{Retriever, RetrieverConfig, Scope, SizingPolicy, SpanKey};

use crate::config::{Config, rel_path, repo_root};
use crate::indexer;
use crate::protocol::{ReadPlan, RenderReq, Request, Response};
use crate::session::Sessions;

/// Caches Laya probabilities in the store, keyed by (task, chunk content), so repeated or
/// follow-up prompts in a session skip inference for chunks already judged.
pub struct MemoScorer {
    inner: Arc<dyn Scorer>,
    store: Arc<dyn Store>,
    model_tag: String,
    /// Set while a model run is in flight. A query that finds the model busy degrades to
    /// lexical ranking instead of queueing behind abandoned (timed-out) runs.
    busy: Arc<std::sync::atomic::AtomicBool>,
    /// `false` (LAYA_MEMO=0): never serve cached probabilities, so every prompt pays the model
    /// run as a new prompt does in real use (benchmarks compare arms under equal, cold scoring).
    read_cache: bool,
}

struct BusyGuard<'a>(&'a std::sync::atomic::AtomicBool);
impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

impl MemoScorer {
    pub fn new(inner: Arc<dyn Scorer>, store: Arc<dyn Store>, model_tag: &str) -> Self {
        MemoScorer {
            inner,
            store,
            model_tag: model_tag.to_string(),
            busy: Default::default(),
            read_cache: true,
        }
    }

    /// Score every call with the model (results are still written, reads are skipped).
    pub fn without_cache_reads(mut self) -> Self {
        self.read_cache = false;
        self
    }

    /// The model's busy flag, shared with other users of the same model (the scope classifier).
    pub fn busy_flag(&self) -> Arc<std::sync::atomic::AtomicBool> {
        Arc::clone(&self.busy)
    }

    fn key(&self, task: &str, chunk: &Chunk) -> String {
        let mut h = blake3::Hasher::new();
        h.update(self.model_tag.as_bytes());
        h.update(&[0]);
        h.update(task.as_bytes());
        h.update(&[0]);
        h.update(chunk.id().as_bytes());
        format!("laya:{}", &h.finalize().to_hex()[..32])
    }
}

impl Scorer for MemoScorer {
    fn score(&self, task: &str, chunks: &[&Chunk]) -> laya_core::Result<Vec<f32>> {
        let keys: Vec<String> = chunks.iter().map(|c| self.key(task, c)).collect();
        let mut out = vec![f32::NAN; chunks.len()];
        let mut miss = Vec::new();
        for (i, k) in keys.iter().enumerate() {
            let cached = if self.read_cache {
                self.store.memo_get(k).ok().flatten()
            } else {
                None
            };
            match cached.and_then(|v| v.parse::<f32>().ok()) {
                Some(p) => out[i] = p,
                None => miss.push(i),
            }
        }
        if !miss.is_empty() {
            use std::sync::atomic::Ordering;
            if self
                .busy
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return Err(laya_core::Error::Model("model busy".into()));
            }
            let _guard = BusyGuard(&self.busy);
            let todo: Vec<&Chunk> = miss.iter().map(|&i| chunks[i]).collect();
            let t0 = std::time::Instant::now();
            let ps = self.inner.score(task, &todo)?;
            eprintln!(
                "[laya] scored {} chunks ({} cached) in {:?}",
                todo.len(),
                chunks.len() - todo.len(),
                t0.elapsed()
            );
            for (&i, p) in miss.iter().zip(ps) {
                out[i] = p;
                let _ = self.store.memo_put(&keys[i], &format!("{p:.5}"), 86_400);
            }
        }
        Ok(out)
    }
}

/// Predicts how much code a prompt needs (function / file / module / cross-module), which sets
/// the adaptive sizing caps. `None` = unknown; sizing then uses the default caps.
pub trait ScopeClassifier: Send + Sync {
    fn classify(&self, prompt: &str) -> Option<Scope>;
}

const SCOPES: [Scope; 4] = [Scope::Function, Scope::File, Scope::Module, Scope::Cross];

/// Laya `choice` wording for the scope question; the criteria order matches `SCOPES`.
pub const SCOPE_QUESTION: &str = "What is the scope of the code change needed for: \"{task}\"?";
pub const SCOPE_CRITERIA: [(&str, &str); 4] = [
    ("function", "the edit stays inside one function or method"),
    ("file", "the edit is confined to one file"),
    ("module", "a few related files in one module"),
    ("cross", "many files across several modules"),
];

/// Argmax scope if its probability reaches `min_p`, else `None` (not confident enough to shrink
/// or grow the context).
pub fn scope_from_probs(probs: &[f32], min_p: f32) -> Option<Scope> {
    if probs.len() != SCOPES.len() {
        return None;
    }
    let (i, &p) = probs.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;
    (p >= min_p).then_some(SCOPES[i])
}

/// Scope classifier on the resident Laya model. Memoized per prompt in the store; skipped (→
/// `None`) while the model is busy so a prompt never queues behind an abandoned scoring run.
pub struct LayaScope {
    pub scorer: Arc<laya_model::LayaScorer>,
    pub store: Arc<dyn Store>,
    pub busy: Arc<std::sync::atomic::AtomicBool>,
    pub model_tag: String,
    pub min_p: f32,
}

impl LayaScope {
    fn probs(&self, prompt: &str) -> Option<Vec<f32>> {
        let key = format!(
            "scope:{}",
            &blake3::hash(format!("{}\0{prompt}", self.model_tag).as_bytes()).to_hex()[..32]
        );
        if let Some(v) = self.store.memo_get(&key).ok().flatten() {
            return v.split(',').map(|x| x.parse().ok()).collect();
        }
        use std::sync::atomic::Ordering;
        if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }
        let _guard = BusyGuard(&self.busy);
        let criteria: Vec<(&str, Option<&str>)> =
            SCOPE_CRITERIA.iter().map(|(n, d)| (*n, Some(*d))).collect();
        let question = SCOPE_QUESTION.replace("{task}", prompt);
        let probs = self
            .scorer
            .model
            .choice(&question, &criteria, &[prompt.to_string()])
            .ok()?
            .pop()?;
        let v: Vec<String> = probs.iter().map(|p| format!("{p:.4}")).collect();
        let _ = self.store.memo_put(&key, &v.join(","), 86_400);
        Some(probs)
    }
}

impl ScopeClassifier for LayaScope {
    fn classify(&self, prompt: &str) -> Option<Scope> {
        let t0 = std::time::Instant::now();
        let probs = self.probs(prompt)?;
        let scope = scope_from_probs(&probs, self.min_p);
        eprintln!("[laya] scope {scope:?} p={probs:?} in {:?}", t0.elapsed());
        scope
    }
}

fn scope_name(s: Scope) -> String {
    serde_json::to_value(s)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

pub struct Daemon {
    pub store: Arc<dyn Store>,
    pub scorer: RwLock<Option<Arc<dyn Scorer>>>,
    pub scope: RwLock<Option<Arc<dyn ScopeClassifier>>>,
    pub sizing: SizingPolicy,
    pub sessions: Mutex<Sessions>,
    pub indexing: Mutex<HashSet<String>>,
    pub base_cfg: RetrieverConfig,
}

impl Daemon {
    #[cfg(test)]
    pub fn new(store: Arc<dyn Store>, base_cfg: RetrieverConfig) -> Arc<Self> {
        Self::with_sizing(store, base_cfg, SizingPolicy::default())
    }

    pub fn with_sizing(
        store: Arc<dyn Store>,
        base_cfg: RetrieverConfig,
        sizing: SizingPolicy,
    ) -> Arc<Self> {
        Arc::new(Daemon {
            store,
            scorer: RwLock::new(None),
            scope: RwLock::new(None),
            sizing,
            sessions: Mutex::new(Sessions::default()),
            indexing: Mutex::new(HashSet::new()),
            base_cfg,
        })
    }

    /// Render `result` for injection. Adaptive: classify scope, size by scope and calibrated P,
    /// skip what the session already has, and record what was inlined — under one session lock
    /// so concurrent prompts cannot both send the same span.
    fn render(
        &self,
        session: Option<&str>,
        prompt: &str,
        result: &QueryResult,
        req: &RenderReq,
    ) -> (String, Option<Scope>) {
        if !req.adaptive {
            return (
                laya_rank::render_compact_opts(result, 3, req.budget_tokens, req.related),
                None,
            );
        }
        let scope = self
            .scope
            .read()
            .ok()
            .and_then(|c| c.clone())
            .and_then(|c| c.classify(prompt));
        let Ok(mut sessions) = self.sessions.lock() else {
            return (
                laya_rank::render_compact_opts(result, 3, req.budget_tokens, req.related),
                scope,
            );
        };
        let already: Vec<SpanKey> = session
            .map(|s| sessions.already(s))
            .unwrap_or_default()
            .into_iter()
            .map(|(path, start_line, end_line)| SpanKey {
                path,
                start_line,
                end_line,
            })
            .collect();
        let mut ctx = laya_rank::size_context(result, scope, &self.sizing, &already);
        if !req.related {
            ctx.related.clear();
        }
        if ctx.full.is_empty() && ctx.map.is_empty() && ctx.related.is_empty() {
            return (String::new(), scope); // everything relevant is already in context
        }
        let (text, keys) = laya_rank::render_sized_with_keys(&ctx, req.budget_tokens);
        if let Some(s) = session {
            let keys: Vec<(String, u32, u32)> = keys
                .into_iter()
                .map(|k| (k.path, k.start_line, k.end_line))
                .collect();
            sessions.mark_sent(s, &keys);
        }
        (text, scope)
    }

    fn repo(&self, repo: &str) -> (PathBuf, String) {
        let root = repo_root(Path::new(repo));
        let id = laya_store::repo_id(&root);
        (root, id)
    }

    pub fn handle(self: &Arc<Self>, req: Request) -> Response {
        match req {
            Request::Ping => Response::Pong {
                model_ready: self.scorer.read().map(|s| s.is_some()).unwrap_or(false),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            Request::Query {
                repo,
                session,
                prompt,
                budget_ms,
                top_n,
                render,
            } => {
                let (_, id) = self.repo(&repo);
                let mut cfg = self.base_cfg.clone();
                if let Some(b) = budget_ms {
                    cfg.laya_budget = Duration::from_millis(b);
                    if b == 0 {
                        cfg.use_laya = false; // lexical-only request (used by the ablation arm)
                    }
                }
                if let Some(n) = top_n {
                    cfg.top_n = n;
                }
                let scorer = self.scorer.read().ok().and_then(|s| s.clone());
                let retriever = Retriever::new(self.store.as_ref(), scorer, cfg);
                let query = match (&session, self.sessions.lock()) {
                    (Some(s), Ok(mut sessions)) => sessions.effective_query(s, &prompt),
                    _ => prompt.clone(),
                };
                match retriever.query(&id, &query) {
                    Ok(result) => {
                        if let (Some(s), Ok(mut sessions)) = (&session, self.sessions.lock()) {
                            sessions.record_query(s, &result);
                        }
                        let (rendered, scope) = match &render {
                            Some(r) => {
                                let (text, scope) =
                                    self.render(session.as_deref(), &prompt, &result, r);
                                (Some(text), scope.map(scope_name))
                            }
                            None => (None, None),
                        };
                        Response::Query {
                            result,
                            rendered,
                            scope,
                        }
                    }
                    Err(e) => Response::Error {
                        message: e.to_string(),
                    },
                }
            }
            Request::NoteRead {
                session,
                path,
                full,
            } => match self.sessions.lock() {
                Ok(mut s) => Response::Count {
                    count: s.note_read(&session, &path, full),
                },
                Err(_) => Response::Error {
                    message: "session lock poisoned".into(),
                },
            },
            Request::Session { session, reset } => match self.sessions.lock() {
                Ok(mut s) => {
                    if reset {
                        s.reset_context(&session);
                    }
                    Response::Session {
                        view: s.view(&session),
                    }
                }
                Err(_) => Response::Error {
                    message: "session lock poisoned".into(),
                },
            },
            Request::ReindexFile { repo, path } => {
                let (root, id) = self.repo(&repo);
                let Some(rel) = rel_path(&root, &path) else {
                    return Response::Ok;
                };
                match indexer::index_file(&root, self.store.as_ref(), &id, &rel) {
                    Ok(_) => Response::Ok,
                    Err(e) => Response::Error {
                        message: e.to_string(),
                    },
                }
            }
            Request::IndexRepo { repo } => {
                let (root, id) = self.repo(&repo);
                let fresh = self
                    .indexing
                    .lock()
                    .map(|mut s| s.insert(id.clone()))
                    .unwrap_or(false);
                if fresh {
                    let me = Arc::clone(self);
                    std::thread::spawn(move || {
                        let r = indexer::index_repo(&root, me.store.as_ref(), &id);
                        eprintln!("[laya] background index {}: {r:?}", root.display());
                        if let Ok(mut s) = me.indexing.lock() {
                            s.remove(&id);
                        }
                    });
                }
                Response::Ok
            }
            Request::ReadPlan {
                repo,
                session,
                path,
            } => Response::ReadPlan {
                plan: self.read_plan(&repo, &session, &path),
            },
        }
    }

    /// Plan the first whole-file Read of `path`: the region the session's last ranking (else its
    /// last query's terms) points at, plus an outline of the file's items. `None` (pass the Read
    /// through) unless the file is large, inside the repo, indexed, and unchanged since indexing
    /// (its chunks' line numbers must describe the bytes the agent will get), and something
    /// actually points into it. A plan is recorded as a partial read of the session.
    fn read_plan(&self, repo: &str, session: &str, path: &str) -> Option<ReadPlan> {
        let (root, id) = self.repo(repo);
        let rel = rel_path(&root, path)?;
        let abs = root.join(&rel);
        // Cheap checks before reading: the indexer never indexes files over this size.
        if std::fs::metadata(&abs).ok()?.len() > laya_parse::MAX_FILE_BYTES {
            return None;
        }
        let indexed = self.store.file_hash(&id, &rel).ok()??;
        let bytes = std::fs::read(&abs).ok()?;
        let total_lines = crate::hook::line_count(&bytes);
        let policy = laya_rank::ReadPolicy::default();
        if total_lines < policy.min_file_lines {
            return None;
        }
        if indexed != laya_parse::file_hash(&bytes) {
            return None; // edited since indexing: chunk line numbers may be wrong
        }
        let chunks = self.store.chunks_of_file(&id, &rel).ok()?;
        if chunks.is_empty() {
            return None;
        }
        let (ranking, query) = self.sessions.lock().ok()?.read_context(session);
        let signals = laya_rank::extract_signals(query.as_deref().unwrap_or_default());
        let (offset, limit, basis) =
            policy.read_region(ranking.as_ref(), &rel, &chunks, &signals, total_lines)?;
        let outline = policy.outline(&chunks, &signals, (offset, limit));
        self.sessions
            .lock()
            .ok()?
            .narrowed_read(session, &rel, offset, offset + limit - 1);
        Some(ReadPlan {
            offset,
            limit,
            total_lines,
            basis: basis.to_string(),
            outline,
        })
    }
}

fn env_num<T: std::str::FromStr>(key: &str) -> Option<T> {
    std::env::var(key).ok()?.parse().ok()
}

fn serve_conn(daemon: Arc<Daemon>, stream: UnixStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { return };
        let resp = match serde_json::from_str::<Request>(&line) {
            Ok(req) => daemon.handle(req),
            Err(e) => Response::Error {
                message: format!("bad request: {e}"),
            },
        };
        let Ok(mut s) = serde_json::to_string(&resp) else {
            return;
        };
        s.push('\n');
        if writer.write_all(s.as_bytes()).is_err() {
            return;
        }
    }
}

/// Bind the socket, refusing to start a second daemon when one already answers.
fn bind_single(socket: &Path) -> anyhow::Result<Option<UnixListener>> {
    if UnixStream::connect(socket).is_ok() {
        return Ok(None);
    }
    let _ = std::fs::remove_file(socket);
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    Ok(Some(UnixListener::bind(socket)?))
}

pub fn run(cfg: &Config) -> anyhow::Result<()> {
    let Some(listener) = bind_single(&cfg.socket_path())? else {
        eprintln!(
            "[laya] daemon already running at {}",
            cfg.socket_path().display()
        );
        return Ok(());
    };
    crate::config::ensure_moon(cfg)?;
    let store: Arc<dyn Store> = Arc::new(laya_store::MoonStore::new(
        laya_store::StoreConfig::local(cfg.moon_port),
    )?);
    // Defaults are the configuration that won the paired benchmark (bench/results/claude-v2):
    // weighted fusion w=0.5, no probability gate, 128 state tokens. Env vars override them
    // (read once at daemon start; see bench/sweep.py). LAYA_WEIGHT=rrf selects rank fusion.
    let mut base = RetrieverConfig {
        laya_budget: Duration::from_millis(cfg.budget_ms),
        use_laya: cfg.use_model,
        laya_weight: match std::env::var("LAYA_WEIGHT").as_deref() {
            Ok("rrf") => None,
            Ok(v) => v.parse().ok().or(Some(0.5)),
            Err(_) => Some(0.5),
        },
        p_threshold: env_num::<f32>("LAYA_P_THRESHOLD").unwrap_or(0.0),
        ..RetrieverConfig::default()
    };
    if let Some(k) = env_num::<usize>("LAYA_K") {
        base.k_candidates = k;
    }
    if let Some(m) = env_num::<usize>("LAYA_MIN_KEEP") {
        base.min_keep = m;
    }
    let state_tokens = env_num::<usize>("LAYA_STATE_TOKENS").unwrap_or(128);
    eprintln!("[laya] retriever config {base:?} state_tokens={state_tokens}");
    // Rank-based by default (thresholds 0 = full code for the top spans by fused rank, capped by
    // scope). Laya's P scale shifts with prompt wording, so on agent-wrapped prompts every P
    // threshold lost gold coverage vs the fused rank at equal code volume (bench/size_sweep.py on
    // --template bench/alt dumps). Adaptive's gain is the session delta, not P thresholds.
    let sizing = SizingPolicy {
        tau_full: env_num::<f32>("LAYA_TAU_FULL").unwrap_or(0.0),
        tau_map: env_num::<f32>("LAYA_TAU_MAP").unwrap_or(0.0),
        ..SizingPolicy::default()
    };
    let scope_p = env_num::<f32>("LAYA_SCOPE_P").unwrap_or(0.4);
    let use_scope = std::env::var("LAYA_SCOPE")
        .map(|v| v != "0")
        .unwrap_or(true);
    eprintln!("[laya] sizing {sizing:?} scope={use_scope} scope_p={scope_p}");
    let daemon = Daemon::with_sizing(Arc::clone(&store), base, sizing);

    if let (true, Some(dir)) = (cfg.use_model, cfg.model_dir.clone()) {
        let d = Arc::clone(&daemon);
        std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            match laya_model::LayaModel::load(&dir, laya_model::DeviceKind::Auto) {
                Ok(model) => {
                    let name = dir
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let tag = format!("{name}-s{state_tokens}");
                    let mut scorer = laya_model::LayaScorer::new(model);
                    scorer.max_state_tokens = state_tokens;
                    let scorer = Arc::new(scorer);
                    let inner: Arc<dyn Scorer> = scorer.clone();
                    let mut memo = MemoScorer::new(inner, Arc::clone(&d.store), &tag);
                    if std::env::var("LAYA_MEMO").is_ok_and(|v| v == "0") {
                        memo = memo.without_cache_reads();
                    }
                    if use_scope {
                        let scope = LayaScope {
                            scorer,
                            store: Arc::clone(&d.store),
                            busy: memo.busy_flag(),
                            model_tag: name.clone(),
                            min_p: scope_p,
                        };
                        if let Ok(mut s) = d.scope.write() {
                            *s = Some(Arc::new(scope));
                        }
                    }
                    if let Ok(mut s) = d.scorer.write() {
                        *s = Some(Arc::new(memo));
                    }
                    eprintln!("[laya] model {} ready in {:?}", dir.display(), t0.elapsed());
                }
                Err(e) => eprintln!("[laya] model load failed ({e}); serving lexical ranking"),
            }
        });
    }
    eprintln!("[laya] daemon listening on {}", cfg.socket_path().display());
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let d = Arc::clone(&daemon);
                std::thread::spawn(move || serve_conn(d, s));
            }
            Err(e) => eprintln!("[laya] accept error: {e}"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::mem::MemStore;
    use laya_core::Lang;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingScorer(AtomicUsize);
    impl Scorer for CountingScorer {
        fn score(&self, _task: &str, chunks: &[&Chunk]) -> laya_core::Result<Vec<f32>> {
            self.0.fetch_add(chunks.len(), Ordering::SeqCst);
            Ok(chunks.iter().map(|c| c.start_line as f32 / 100.0).collect())
        }
    }

    /// MemStore with a working memo map.
    #[derive(Default)]
    struct MemoStore {
        inner: MemStore,
        memo: Mutex<std::collections::HashMap<String, String>>,
        /// Chunks by id, searchable by any-term match (enough for daemon-level tests).
        chunks: Mutex<Vec<Chunk>>,
    }
    impl Store for MemoStore {
        fn ensure_index(&self, r: &str) -> laya_core::Result<()> {
            self.inner.ensure_index(r)
        }
        fn put_file(&self, r: &str, p: &str, h: &str, c: &[Chunk]) -> laya_core::Result<()> {
            self.chunks.lock().unwrap().extend(c.iter().cloned());
            self.inner.put_file(r, p, h, c)
        }
        fn delete_file(&self, r: &str, p: &str) -> laya_core::Result<()> {
            self.inner.delete_file(r, p)
        }
        fn file_hash(&self, r: &str, p: &str) -> laya_core::Result<Option<String>> {
            self.inner.file_hash(r, p)
        }
        fn list_files(&self, r: &str) -> laya_core::Result<Vec<String>> {
            self.inner.list_files(r)
        }
        fn bm25(&self, _: &str, t: &[String], l: usize) -> laya_core::Result<Vec<(String, f32)>> {
            let chunks = self.chunks.lock().unwrap();
            let mut hits: Vec<(String, f32)> = chunks
                .iter()
                .map(|c| {
                    (
                        c.id(),
                        t.iter()
                            .filter(|w| c.text.to_lowercase().contains(w.as_str()))
                            .count() as f32,
                    )
                })
                .filter(|(_, n)| *n > 0.0)
                .collect();
            hits.sort_by(|a, b| b.1.total_cmp(&a.1));
            hits.truncate(l);
            Ok(hits)
        }
        fn chunks_defining(
            &self,
            r: &str,
            i: &[String],
            l: usize,
        ) -> laya_core::Result<Vec<String>> {
            self.inner.chunks_defining(r, i, l)
        }
        fn get_chunks(&self, _: &str, ids: &[String]) -> laya_core::Result<Vec<Chunk>> {
            let chunks = self.chunks.lock().unwrap();
            Ok(ids
                .iter()
                .filter_map(|id| chunks.iter().find(|c| &c.id() == id).cloned())
                .collect())
        }
        fn memo_get(&self, k: &str) -> laya_core::Result<Option<String>> {
            Ok(self.memo.lock().unwrap().get(k).cloned())
        }
        fn memo_put(&self, k: &str, v: &str, _: u64) -> laya_core::Result<()> {
            self.memo.lock().unwrap().insert(k.into(), v.into());
            Ok(())
        }
    }

    fn chunk(start: u32) -> Chunk {
        Chunk {
            path: "a.rs".into(),
            start_line: start,
            end_line: start + 9,
            lang: Lang::Rust,
            symbol: String::new(),
            kind: "function_item".into(),
            defines: vec![],
            refs: vec![],
            text: format!("fn f{start}() {{}}"),
        }
    }

    #[test]
    fn memo_scorer_scores_each_chunk_once_per_task() {
        let inner = Arc::new(CountingScorer(AtomicUsize::new(0)));
        let store: Arc<dyn Store> = Arc::new(MemoStore::default());
        let m = MemoScorer::new(inner.clone(), store, "laya-code");
        let (a, b) = (chunk(10), chunk(20));
        let p1 = m.score("task", &[&a, &b]).unwrap();
        let p2 = m.score("task", &[&b, &a]).unwrap();
        assert_eq!(inner.0.load(Ordering::SeqCst), 2);
        assert!((p1[0] - p2[1]).abs() < 1e-4 && (p1[1] - p2[0]).abs() < 1e-4);
        m.score("other task", &[&a]).unwrap();
        assert_eq!(inner.0.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn memo_scorer_without_cache_reads_scores_every_call() {
        let inner = Arc::new(CountingScorer(AtomicUsize::new(0)));
        let store: Arc<dyn Store> = Arc::new(MemoStore::default());
        let m = MemoScorer::new(inner.clone(), store, "laya-code").without_cache_reads();
        let a = chunk(10);
        m.score("task", &[&a]).unwrap();
        m.score("task", &[&a]).unwrap();
        assert_eq!(
            inner.0.load(Ordering::SeqCst),
            2,
            "every prompt is scored cold"
        );
    }

    struct FixedScope(Option<Scope>);
    impl ScopeClassifier for FixedScope {
        fn classify(&self, _prompt: &str) -> Option<Scope> {
            self.0
        }
    }

    fn daemon_with_code() -> (Arc<Daemon>, String) {
        let d = Daemon::new(Arc::new(MemoStore::default()), RetrieverConfig::default());
        let repo = std::env::temp_dir().to_string_lossy().into_owned();
        let (_, id) = d.repo(&repo);
        let chunks: Vec<Chunk> = (0..4)
            .map(|i| Chunk {
                path: format!("src/wal{i}.rs"),
                start_line: 1,
                end_line: 20,
                lang: Lang::Rust,
                symbol: format!("fn replay_wal{i}"),
                kind: "function_item".into(),
                defines: vec![format!("replay_wal{i}")],
                refs: vec![],
                text: format!("fn replay_wal{i}() {{ /* replay wal segment */ }}"),
            })
            .collect();
        for c in &chunks {
            d.store
                .put_file(&id, &c.path, "h", std::slice::from_ref(c))
                .unwrap();
        }
        (d, repo)
    }

    fn query(d: &Arc<Daemon>, repo: &str, adaptive: bool) -> Response {
        d.handle(Request::Query {
            repo: repo.into(),
            session: Some("s".into()),
            prompt: "replay wal segment".into(),
            budget_ms: Some(0),
            top_n: None,
            render: Some(RenderReq {
                budget_tokens: 3000,
                related: true,
                adaptive,
            }),
        })
    }

    #[test]
    fn adaptive_render_skips_code_already_sent_in_the_session() {
        let (d, repo) = daemon_with_code();
        let Response::Query {
            rendered: Some(first),
            result,
            ..
        } = query(&d, &repo, true)
        else {
            panic!("no render")
        };
        assert!(
            !result.spans.is_empty(),
            "retrieval found nothing: {result:?}"
        );
        assert!(first.contains("```"), "first prompt inlines code: {first}");
        let sent = d.sessions.lock().unwrap().already("s");
        assert!(!sent.is_empty());
        let Response::Query {
            rendered: Some(second),
            ..
        } = query(&d, &repo, true)
        else {
            panic!("no render")
        };
        for (path, start, end) in &sent {
            assert!(
                !second.contains(&format!("### {path}:{start}-{end}")),
                "{path} re-sent: {second}"
            );
        }
    }

    #[test]
    fn session_reset_forgets_sent_spans() {
        let (d, repo) = daemon_with_code();
        query(&d, &repo, true);
        assert!(!d.sessions.lock().unwrap().already("s").is_empty());
        d.handle(Request::Session {
            session: "s".into(),
            reset: true,
        });
        assert!(d.sessions.lock().unwrap().already("s").is_empty());
    }

    #[test]
    fn scope_is_classified_and_reported_only_for_adaptive_renders() {
        let (d, repo) = daemon_with_code();
        *d.scope.write().unwrap() = Some(Arc::new(FixedScope(Some(Scope::Function))));
        assert!(
            matches!(query(&d, &repo, true), Response::Query { scope: Some(ref s), .. } if s == "function")
        );
        assert!(matches!(
            query(&d, &repo, false),
            Response::Query {
                scope: None,
                rendered: Some(_),
                ..
            }
        ));
        let plain = d.handle(Request::Query {
            repo,
            session: None,
            prompt: "replay wal".into(),
            budget_ms: Some(0),
            top_n: None,
            render: None,
        });
        assert!(matches!(plain, Response::Query { rendered: None, .. }));
    }

    #[test]
    fn scope_from_probs_needs_confidence() {
        assert_eq!(
            scope_from_probs(&[0.1, 0.7, 0.1, 0.1], 0.4),
            Some(Scope::File)
        );
        assert_eq!(scope_from_probs(&[0.3, 0.3, 0.2, 0.2], 0.4), None);
        assert_eq!(scope_from_probs(&[0.5], 0.4), None);
    }

    #[test]
    fn daemon_tracks_reads_and_sessions() {
        let d = Daemon::new(Arc::new(MemoStore::default()), RetrieverConfig::default());
        assert_eq!(
            d.handle(Request::NoteRead {
                session: "s".into(),
                path: "a.rs".into(),
                full: false
            }),
            Response::Count { count: 1 }
        );
        assert_eq!(
            d.handle(Request::NoteRead {
                session: "s".into(),
                path: "a.rs".into(),
                full: false
            }),
            Response::Count { count: 2 }
        );
        assert!(matches!(
            d.handle(Request::Ping),
            Response::Pong {
                model_ready: false,
                ..
            }
        ));
        assert!(matches!(
            d.handle(Request::Session {
                session: "s".into(),
                reset: false
            }),
            Response::Session { .. }
        ));
    }

    /// A repo with an indexed 390-line `src/big.rs` (30 thirteen-line fns; `handler_17`, at
    /// lines 222-234, calls `replay_wal_segment`) and an indexed 10-line `src/small.rs`.
    fn plan_repo(tag: &str) -> (Arc<Daemon>, PathBuf, String) {
        let root = std::env::temp_dir().join(format!("laya-plan-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        let mut big = String::new();
        for i in 0..30 {
            let call = if i == 17 {
                "replay_wal_segment(x)"
            } else {
                "x + 1"
            };
            big.push_str(&format!(
                "/// Handler {i}.\npub fn handler_{i}(x: u32) -> u32 {{\n    let y = {call};\n"
            ));
            for k in 0..7 {
                big.push_str(&format!("    let y = y.wrapping_mul({k});\n"));
            }
            big.push_str("    y\n}\n\n");
        }
        assert_eq!(big.lines().count(), 390);
        std::fs::write(root.join("src/big.rs"), &big).unwrap();
        std::fs::write(root.join("src/small.rs"), "fn tiny() {}\n".repeat(10)).unwrap();
        let d = Daemon::new(Arc::new(MemStore::default()), RetrieverConfig::default());
        let repo = root.to_string_lossy().into_owned();
        let (root, id) = d.repo(&repo);
        for f in ["src/big.rs", "src/small.rs"] {
            indexer::index_file(&root, d.store.as_ref(), &id, f).unwrap();
        }
        (d, root, repo)
    }

    fn plan(d: &Arc<Daemon>, repo: &str, session: &str, path: &str) -> Option<ReadPlan> {
        match d.handle(Request::ReadPlan {
            repo: repo.into(),
            session: session.into(),
            path: path.into(),
        }) {
            Response::ReadPlan { plan } => plan,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn read_plan_shows_the_task_region_with_an_outline() {
        let (d, root, repo) = plan_repo("lex");
        assert_eq!(
            plan(&d, &repo, "s", "src/big.rs"),
            None,
            "no task yet: nothing to aim at"
        );
        d.handle(Request::Query {
            repo: repo.clone(),
            session: Some("s".into()),
            prompt: "fix the ordering bug in replay_wal_segment".into(),
            budget_ms: Some(0),
            top_n: None,
            render: None,
        });
        d.handle(Request::NoteRead {
            session: "s".into(),
            path: "src/big.rs".into(),
            full: true,
        });
        let abs = root.join("src/big.rs").to_string_lossy().into_owned();
        let p = plan(&d, &repo, "s", &abs).expect("absolute paths are planned too");
        assert_eq!((p.basis.as_str(), p.total_lines), ("lexical", 390));
        let end = p.offset + p.limit - 1;
        assert!(p.offset <= 224 && end >= 224, "{p:?}");
        assert!(p.limit <= 200, "{p:?}");
        assert!(
            p.outline.contains("handler_17") && p.outline.contains('*'),
            "{}",
            p.outline
        );
        assert!(p.outline.len() <= 1600, "{}", p.outline.len());
        // Only the window is in the agent's context now, not the whole file.
        let already = d.sessions.lock().unwrap().already("s");
        assert_eq!(already, vec![("src/big.rs".to_string(), p.offset, end)]);
    }

    #[test]
    fn read_plan_prefers_the_sessions_ranked_spans() {
        let (d, _, repo) = plan_repo("rank");
        let span = laya_core::RankedSpan {
            path: "src/big.rs".into(),
            start_line: 40,
            end_line: 52,
            symbol: "fn handler_3".into(),
            p_relevant: Some(0.1),
            score: 0.1,
            text: String::new(),
        };
        d.sessions.lock().unwrap().record_query(
            "s",
            &QueryResult {
                spans: vec![span],
                mode: laya_core::RankMode::Laya,
                elapsed_ms: 1,
                candidates: 1,
                related: vec![],
            },
        );
        let p = plan(&d, &repo, "s", "src/big.rs").unwrap();
        assert_eq!((p.basis.as_str(), p.offset, p.limit), ("ranking", 35, 23));
    }

    #[test]
    fn read_plan_fails_open_on_small_unindexed_stale_or_missing_files() {
        let (d, root, repo) = plan_repo("open");
        let span = |path: &str| laya_core::RankedSpan {
            path: path.into(),
            start_line: 1,
            end_line: 5,
            symbol: String::new(),
            p_relevant: None,
            score: 1.0,
            text: String::new(),
        };
        d.sessions.lock().unwrap().record_query(
            "s",
            &QueryResult {
                spans: vec![
                    span("src/small.rs"),
                    span("src/other.rs"),
                    span("src/big.rs"),
                ],
                mode: laya_core::RankMode::Lexical,
                elapsed_ms: 1,
                candidates: 3,
                related: vec![],
            },
        );
        assert!(plan(&d, &repo, "s", "src/big.rs").is_some());
        assert_eq!(plan(&d, &repo, "s", "src/small.rs"), None, "small");
        let big = std::fs::read_to_string(root.join("src/big.rs")).unwrap();
        std::fs::write(root.join("src/other.rs"), &big).unwrap();
        assert_eq!(plan(&d, &repo, "s", "src/other.rs"), None, "not indexed");
        assert_eq!(plan(&d, &repo, "s", "src/gone.rs"), None, "missing");
        assert_eq!(plan(&d, &repo, "s", "/etc/hosts"), None, "outside the repo");
        std::fs::write(root.join("src/big.rs"), format!("// edited\n{big}")).unwrap();
        assert_eq!(plan(&d, &repo, "s", "src/big.rs"), None, "stale index");
        // Huge (generated/minified) files are not even read.
        let huge = "x".repeat(1 << 14) + "\n";
        std::fs::write(root.join("src/huge.rs"), huge.repeat(300)).unwrap();
        indexer::index_file(&root, d.store.as_ref(), &d.repo(&repo).1, "src/huge.rs").unwrap();
        assert_eq!(plan(&d, &repo, "s", "src/huge.rs"), None, "huge");
    }
}
