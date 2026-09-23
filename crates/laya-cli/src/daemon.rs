//! `laya-codex daemon`: long-lived process that keeps Moon supervised, the Laya model warm and
//! per-session state in memory. Clients speak the JSON-lines protocol in `protocol.rs`.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::Duration;

use laya_core::QueryResult;
use laya_core::{Chunk, Scorer, Store};
use laya_rank::{Retriever, RetrieverConfig, Scope, SizingPolicy, SpanKey};

use crate::config::{Config, rel_path, repo_root};
use crate::indexer;
use crate::protocol::{
    MAX_BUDGET_MS, MAX_RENDER_TOKENS, MAX_TOP_N, ReadPlan, RenderReq, Request, Response,
};
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
    /// `false` (LAYA_CODEX_MEMO=0): never serve cached probabilities, so every prompt pays the model
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
                "[laya-codex] scored {} chunks ({} cached) in {:?}",
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
        eprintln!(
            "[laya-codex] scope {scope:?} p={probs:?} in {:?}",
            t0.elapsed()
        );
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
        let mut sessions = self.sessions();
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

    /// The session table. A panic under the lock (see `serve_conn`) leaves it poisoned; the
    /// table is only a cache of what each session has seen, so it is used as is rather than
    /// failing every later request.
    fn sessions(&self) -> MutexGuard<'_, Sessions> {
        lock(&self.sessions)
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
                    cfg.laya_budget = Duration::from_millis(b.min(MAX_BUDGET_MS));
                    if b == 0 {
                        cfg.use_laya = false; // lexical-only request (used by the ablation arm)
                    }
                }
                if let Some(n) = top_n {
                    cfg.top_n = n.clamp(1, MAX_TOP_N);
                }
                let scorer = self.scorer.read().ok().and_then(|s| s.clone());
                let retriever = Retriever::new(self.store.as_ref(), scorer, cfg);
                let query = match &session {
                    Some(s) => self.sessions().effective_query(s, &prompt),
                    None => prompt.clone(),
                };
                match retriever.query(&id, &query) {
                    Ok(result) => {
                        if let Some(s) = &session {
                            self.sessions().record_query(s, &result);
                        }
                        let (rendered, scope) = match &render {
                            Some(r) => {
                                let r = RenderReq {
                                    budget_tokens: r.budget_tokens.min(MAX_RENDER_TOKENS),
                                    ..r.clone()
                                };
                                let (text, scope) =
                                    self.render(session.as_deref(), &prompt, &result, &r);
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
            } => Response::Count {
                count: self.sessions().note_read(&session, &path, full),
            },
            Request::Session { session, reset } => {
                let mut s = self.sessions();
                if reset {
                    s.reset_context(&session);
                }
                Response::Session {
                    view: s.view(&session),
                }
            }
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
                if !root.is_dir() {
                    return Response::Error {
                        message: format!("{} is not a directory", root.display()),
                    };
                }
                let fresh = lock(&self.indexing).insert(id.clone());
                if fresh {
                    let slot = IndexingSlot {
                        daemon: Arc::clone(self),
                        id,
                    };
                    std::thread::spawn(move || {
                        // `slot` clears the indexing flag when this thread ends, panic or not.
                        let store = slot.daemon.store.as_ref();
                        match catch_unwind(AssertUnwindSafe(|| {
                            indexer::index_repo(&root, store, &slot.id)
                        })) {
                            Ok(r) => {
                                eprintln!("[laya-codex] background index {}: {r:?}", root.display())
                            }
                            Err(p) => eprintln!(
                                "[laya-codex] background index {} panicked: {}",
                                root.display(),
                                panic_message(p.as_ref())
                            ),
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
            // Acknowledged here; `serve_conn` exits the process once the reply is written.
            Request::Shutdown => Response::Ok,
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
        let (ranking, query) = self.sessions().read_context(session);
        let signals = laya_rank::extract_signals(query.as_deref().unwrap_or_default());
        let (offset, limit, basis) =
            policy.read_region(ranking.as_ref(), &rel, &chunks, &signals, total_lines)?;
        let outline = policy.outline(&chunks, &signals, (offset, limit));
        self.sessions()
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

/// Lock `m`, recovering the guard if a panicking thread poisoned it.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| {
        m.clear_poison();
        e.into_inner()
    })
}

/// A repo marked as being indexed; unmarked on drop, including when the index job panics.
struct IndexingSlot {
    daemon: Arc<Daemon>,
    id: String,
}

impl Drop for IndexingSlot {
    fn drop(&mut self) {
        lock(&self.daemon.indexing).remove(&self.id);
    }
}

/// The message of a caught panic payload (`panic!` with a literal or a formatted string).
fn panic_message(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".into())
}

/// Handle one request, turning a panic (a parser, store or model bug on a strange input) into
/// an error reply so the connection, and the daemon, keep serving.
fn handle_guarded(daemon: &Arc<Daemon>, req: Request) -> Response {
    catch_unwind(AssertUnwindSafe(|| daemon.handle(req))).unwrap_or_else(|p| {
        let message = format!("internal error: {}", panic_message(p.as_ref()));
        eprintln!("[laya-codex] request panicked: {message}");
        Response::Error { message }
    })
}

fn env_num<T: std::str::FromStr>(key: &str) -> Option<T> {
    std::env::var(key).ok()?.parse().ok()
}

/// Longest request line the daemon reads (the newline excluded). A prompt is the only large
/// field; a longer line is refused before it is buffered.
pub const MAX_REQUEST_BYTES: usize = 1 << 20;

/// Per-connection resource bounds of the daemon's socket server.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Connections served at once; more get an immediate "busy" error and are closed.
    pub max_conns: usize,
    /// Longest request line, see [`MAX_REQUEST_BYTES`].
    pub max_request_bytes: usize,
    /// How long a connection may sit without sending (idle clients are dropped).
    pub read_timeout: Duration,
    /// How long one reply may take to write (a client that never reads is dropped).
    pub write_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_conns: 64,
            max_request_bytes: MAX_REQUEST_BYTES,
            read_timeout: Duration::from_secs(30),
            write_timeout: Duration::from_secs(10),
        }
    }
}

/// Write one response line; `false` if the client is gone or too slow to read it.
fn send(mut w: &UnixStream, resp: &Response) -> bool {
    let Ok(mut s) = serde_json::to_string(resp) else {
        return false;
    };
    s.push('\n');
    w.write_all(s.as_bytes()).and_then(|()| w.flush()).is_ok()
}

/// Serve one client connection. After acknowledging `Request::Shutdown` it calls `shutdown`
/// (which exits the process in the real daemon).
fn serve_conn(daemon: Arc<Daemon>, stream: UnixStream, shutdown: &dyn Fn(), limits: &Limits) {
    if stream.set_read_timeout(Some(limits.read_timeout)).is_err()
        || stream
            .set_write_timeout(Some(limits.write_timeout))
            .is_err()
    {
        return;
    }
    let mut reader = BufReader::new(&stream);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        // Read at most one byte past the cap: enough to tell an oversized line from a full one.
        let cap = limits.max_request_bytes as u64 + 1;
        match reader.by_ref().take(cap).read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return, // closed, reset, or idle past the read timeout
            Ok(_) => {}
        }
        if buf.last() == Some(&b'\n') {
            buf.pop();
        } else if buf.len() > limits.max_request_bytes {
            let message = format!(
                "request too long (over {} bytes); connection closed",
                limits.max_request_bytes
            );
            eprintln!("[laya-codex] {message}");
            send(&stream, &Response::Error { message });
            return;
        }
        let (resp, stop) = match serde_json::from_slice::<Request>(&buf) {
            Ok(req) => {
                let stop = req == Request::Shutdown;
                (handle_guarded(&daemon, req), stop)
            }
            Err(e) => (
                Response::Error {
                    message: format!("bad request: {e}"),
                },
                false,
            ),
        };
        let written = send(&stream, &resp);
        if stop {
            shutdown();
            return;
        }
        if !written {
            return;
        }
    }
}

/// One of `Limits::max_conns` connection slots; released on drop (however the handler ends).
struct ConnSlot(Arc<AtomicUsize>);

impl ConnSlot {
    fn try_take(active: &Arc<AtomicUsize>, max: usize) -> Option<Self> {
        active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < max).then_some(n + 1)
            })
            .ok()
            .map(|_| ConnSlot(Arc::clone(active)))
    }
}

impl Drop for ConnSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Refuse a connection without ever blocking the accept loop: one short error line into the
/// new socket's empty send buffer (non-blocking), then close.
fn refuse(s: UnixStream, message: &str) {
    if s.set_nonblocking(true).is_ok() {
        send(
            &s,
            &Response::Error {
                message: message.to_string(),
            },
        );
    }
}

/// Accept loop: one thread per connection, at most `limits.max_conns` at a time.
fn serve(
    listener: UnixListener,
    daemon: Arc<Daemon>,
    shutdown: Arc<dyn Fn() + Send + Sync>,
    limits: Limits,
) {
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let s = match stream {
            Ok(s) => s,
            Err(e) => {
                // Out of fds or similar: back off instead of spinning on the error.
                eprintln!("[laya-codex] accept error: {e}");
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        if !peer_allowed(&s) {
            eprintln!("[laya-codex] rejected a connection from another user");
            continue;
        }
        let Some(slot) = ConnSlot::try_take(&active, limits.max_conns) else {
            refuse(s, "daemon busy: too many connections");
            continue;
        };
        let d = Arc::clone(&daemon);
        let stop = Arc::clone(&shutdown);
        let spawned = std::thread::Builder::new()
            .name("laya-conn".into())
            .spawn(move || {
                let _slot = slot;
                serve_conn(d, s, &*stop, &limits);
            });
        if let Err(e) = spawned {
            eprintln!("[laya-codex] cannot start a connection thread: {e}");
        }
    }
}

/// Only processes of the daemon's own user may talk to it. The socket is already mode 0600 in a
/// 0700 directory; this also covers a socket reached through a bind mount or a lax umask.
fn peer_allowed(s: &UnixStream) -> bool {
    matches!(crate::sys::peer_uid(s), Ok(uid) if uid == crate::sys::euid())
}

/// Bind the socket (mode 0600), refusing to start a second daemon when one already answers
/// (an older laya-codex that does not take the daemon lock).
fn bind_single(socket: &Path) -> anyhow::Result<Option<UnixListener>> {
    use std::os::unix::fs::PermissionsExt;
    if UnixStream::connect(socket).is_ok() {
        return Ok(None);
    }
    if let Some(dir) = socket.parent() {
        laya_store::create_private_dir(dir)?;
    }
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket)?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    Ok(Some(listener))
}

/// Remove `path` if it still holds this process's pid (another daemon may have replaced it).
fn remove_own_pidfile(path: &Path) {
    let ours =
        std::fs::read_to_string(path).is_ok_and(|s| s.trim() == std::process::id().to_string());
    if ours {
        let _ = std::fs::remove_file(path);
    }
}

/// Run the daemon. The caller holds the daemon lock (see `laya-codex daemon` in main.rs).
pub fn run(cfg: &Config) -> anyhow::Result<()> {
    let Some(listener) = bind_single(&cfg.socket_path())? else {
        eprintln!(
            "[laya-codex] daemon already running at {}",
            cfg.socket_path().display()
        );
        return Ok(());
    };
    let (socket, pidfile) = (cfg.socket_path(), cfg.daemon_pidfile());
    let shutdown: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        eprintln!("[laya-codex] shutdown requested; exiting (moon keeps running)");
        let _ = std::fs::remove_file(&socket);
        remove_own_pidfile(&pidfile);
        std::process::exit(0);
    });
    if let Err(e) = crate::config::ensure_moon(cfg) {
        let _ = std::fs::remove_file(cfg.socket_path());
        remove_own_pidfile(&cfg.daemon_pidfile());
        return Err(e);
    }
    let store: Arc<dyn Store> = Arc::new(laya_store::MoonStore::new(crate::config::store_config(
        cfg,
    )?)?);
    // Defaults are the configuration that won the paired benchmark (bench/results/claude-v2):
    // weighted fusion w=0.5, no probability gate, 128 state tokens. Env vars override them
    // (read once at daemon start; see bench/sweep.py). LAYA_CODEX_WEIGHT=rrf selects rank fusion.
    let mut base = RetrieverConfig {
        laya_budget: Duration::from_millis(cfg.budget_ms),
        use_laya: cfg.use_model,
        laya_weight: match std::env::var("LAYA_CODEX_WEIGHT").as_deref() {
            Ok("rrf") => None,
            Ok(v) => v.parse().ok().or(Some(0.5)),
            Err(_) => Some(0.5),
        },
        p_threshold: env_num::<f32>("LAYA_CODEX_P_THRESHOLD").unwrap_or(0.0),
        ..RetrieverConfig::default()
    };
    if let Some(k) = env_num::<usize>("LAYA_CODEX_K") {
        base.k_candidates = k;
    }
    if let Some(m) = env_num::<usize>("LAYA_CODEX_MIN_KEEP") {
        base.min_keep = m;
    }
    let state_tokens = env_num::<usize>("LAYA_CODEX_STATE_TOKENS").unwrap_or(128);
    eprintln!("[laya-codex] retriever config {base:?} state_tokens={state_tokens}");
    // Rank-based by default (thresholds 0 = full code for the top spans by fused rank, capped by
    // scope). Laya's P scale shifts with prompt wording, so on agent-wrapped prompts every P
    // threshold lost gold coverage vs the fused rank at equal code volume (bench/size_sweep.py on
    // --template bench/alt dumps). Adaptive's gain is the session delta, not P thresholds.
    let sizing = SizingPolicy {
        tau_full: env_num::<f32>("LAYA_CODEX_TAU_FULL").unwrap_or(0.0),
        tau_map: env_num::<f32>("LAYA_CODEX_TAU_MAP").unwrap_or(0.0),
        ..SizingPolicy::default()
    };
    let scope_p = env_num::<f32>("LAYA_CODEX_SCOPE_P").unwrap_or(0.4);
    // Off by default: zero-shot scope is near-uniform (macro-F1 <= 0.28) and even oracle scope
    // barely changes what loads, so it would only cost a model call per prompt.
    let use_scope = std::env::var("LAYA_CODEX_SCOPE")
        .map(|v| v == "1")
        .unwrap_or(false);
    eprintln!("[laya-codex] sizing {sizing:?} scope={use_scope} scope_p={scope_p}");
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
                    // Warm up before publishing: the first Metal run compiles kernels (~10 s),
                    // which would push the first real prompt past its budget into lexical mode.
                    let t_warm = std::time::Instant::now();
                    let warm = Chunk {
                        path: "warmup.rs".into(),
                        start_line: 1,
                        end_line: 40,
                        lang: laya_core::Lang::Rust,
                        symbol: String::new(),
                        kind: "function_item".into(),
                        defines: vec![],
                        refs: vec![],
                        text: "fn warm_up(x: u32) -> u32 { x.wrapping_mul(31).rotate_left(7) }\n"
                            .repeat(20),
                    };
                    let batch: Vec<&Chunk> = std::iter::repeat_n(&warm, 24).collect();
                    let _ = scorer.score("warm up the relevance model", &batch);
                    eprintln!("[laya-codex] model warm-up in {:?}", t_warm.elapsed());
                    let scorer = Arc::new(scorer);
                    let inner: Arc<dyn Scorer> = scorer.clone();
                    let mut memo = MemoScorer::new(inner, Arc::clone(&d.store), &tag);
                    if std::env::var("LAYA_CODEX_MEMO").is_ok_and(|v| v == "0") {
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
                    eprintln!(
                        "[laya-codex] model {} ready in {:?}",
                        dir.display(),
                        t0.elapsed()
                    );
                }
                Err(e) => {
                    eprintln!("[laya-codex] model load failed ({e}); serving lexical ranking")
                }
            }
        });
    }
    eprintln!(
        "[laya-codex] daemon listening on {}",
        cfg.socket_path().display()
    );
    serve(listener, daemon, shutdown, Limits::default());
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

    #[test]
    fn socket_is_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile_dir("sock");
        let sock = d.join("laya.sock");
        let _l = bind_single(&sock).unwrap().expect("bound");
        let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // A live socket is not stolen by a second daemon.
        assert!(bind_single(&sock).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn shutdown_request_is_acknowledged_then_runs_the_shutdown_hook() {
        use std::sync::atomic::AtomicBool;
        let (d, _) = daemon_with_code();
        let (client, server) = UnixStream::pair().unwrap();
        let fired = Arc::new(AtomicBool::new(false));
        let f = Arc::clone(&fired);
        let t = std::thread::spawn(move || {
            serve_conn(
                d,
                server,
                &move || f.store(true, Ordering::SeqCst),
                &Limits::default(),
            );
        });
        (&client)
            .write_all(b"{\"op\":\"ping\"}\n{\"op\":\"shutdown\"}\n")
            .unwrap();
        let mut lines = BufReader::new(client.try_clone().unwrap()).lines();
        assert!(lines.next().unwrap().unwrap().contains("pong"));
        assert_eq!(lines.next().unwrap().unwrap(), r#"{"status":"ok"}"#);
        t.join().unwrap();
        assert!(fired.load(Ordering::SeqCst));
    }

    #[test]
    fn peers_of_the_same_user_are_allowed() {
        let (a, _b) = UnixStream::pair().unwrap();
        assert!(peer_allowed(&a));
    }

    /// A store that panics in `bm25` (every query) and `ensure_index` (every index job), as a
    /// parser or store bug on a strange input would.
    #[derive(Default)]
    struct PanicStore(MemStore);
    impl Store for PanicStore {
        fn ensure_index(&self, _: &str) -> laya_core::Result<()> {
            panic!("injected ensure_index panic")
        }
        fn put_file(&self, r: &str, p: &str, h: &str, c: &[Chunk]) -> laya_core::Result<()> {
            self.0.put_file(r, p, h, c)
        }
        fn delete_file(&self, r: &str, p: &str) -> laya_core::Result<()> {
            self.0.delete_file(r, p)
        }
        fn file_hash(&self, r: &str, p: &str) -> laya_core::Result<Option<String>> {
            self.0.file_hash(r, p)
        }
        fn list_files(&self, r: &str) -> laya_core::Result<Vec<String>> {
            self.0.list_files(r)
        }
        fn bm25(&self, _: &str, _: &[String], _: usize) -> laya_core::Result<Vec<(String, f32)>> {
            panic!("injected bm25 panic")
        }
        fn chunks_defining(
            &self,
            r: &str,
            i: &[String],
            l: usize,
        ) -> laya_core::Result<Vec<String>> {
            self.0.chunks_defining(r, i, l)
        }
        fn get_chunks(&self, r: &str, ids: &[String]) -> laya_core::Result<Vec<Chunk>> {
            self.0.get_chunks(r, ids)
        }
        fn memo_get(&self, k: &str) -> laya_core::Result<Option<String>> {
            self.0.memo_get(k)
        }
        fn memo_put(&self, k: &str, v: &str, t: u64) -> laya_core::Result<()> {
            self.0.memo_put(k, v, t)
        }
    }

    fn query_line(repo: &str) -> String {
        let q = Request::Query {
            repo: repo.into(),
            session: Some("s".into()),
            prompt: "replay wal".into(),
            budget_ms: Some(0),
            top_n: None,
            render: None,
        };
        format!("{}\n", serde_json::to_string(&q).unwrap())
    }

    #[test]
    fn a_panicking_request_gets_an_error_and_the_connection_keeps_serving() {
        let d = Daemon::new(Arc::new(PanicStore::default()), RetrieverConfig::default());
        let repo = std::env::temp_dir().to_string_lossy().into_owned();
        let (client, server) = UnixStream::pair().unwrap();
        let t = std::thread::spawn(move || serve_conn(d, server, &|| {}, &Limits::default()));
        (&client).write_all(query_line(&repo).as_bytes()).unwrap();
        (&client).write_all(b"{\"op\":\"ping\"}\n").unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let mut lines = BufReader::new(client.try_clone().unwrap()).lines();
        let first = lines
            .next()
            .expect("a reply to the panicking request")
            .unwrap();
        assert!(
            first.contains("\"error\"") && first.contains("internal error"),
            "{first}"
        );
        let second = lines.next().expect("the connection still serves").unwrap();
        assert!(second.contains("pong"), "{second}");
        t.join().expect("serve_conn does not propagate the panic");
    }

    #[test]
    fn a_panic_while_holding_the_session_lock_does_not_wedge_sessions() {
        let d = Daemon::new(Arc::new(MemoStore::default()), RetrieverConfig::default());
        let d2 = Arc::clone(&d);
        let _ = std::thread::spawn(move || {
            let _g = d2.sessions.lock().unwrap();
            panic!("injected panic under the session lock");
        })
        .join();
        assert_eq!(
            d.handle(Request::NoteRead {
                session: "s".into(),
                path: "a.rs".into(),
                full: false
            }),
            Response::Count { count: 1 }
        );
    }

    #[test]
    fn a_panicking_index_job_clears_the_indexing_flag() {
        let d = Daemon::new(Arc::new(PanicStore::default()), RetrieverConfig::default());
        let root = tempfile_dir("idxpanic");
        let repo = root.to_string_lossy().into_owned();
        assert_eq!(
            d.handle(Request::IndexRepo { repo: repo.clone() }),
            Response::Ok
        );
        let t0 = std::time::Instant::now();
        while !d
            .indexing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
        {
            assert!(
                t0.elapsed() < Duration::from_secs(10),
                "indexing flag never cleared after the job panicked"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn index_repo_of_a_missing_directory_is_an_error() {
        let d = Daemon::new(Arc::new(MemoStore::default()), RetrieverConfig::default());
        let missing = std::env::temp_dir().join(format!("laya-no-repo-{}", std::process::id()));
        let r = d.handle(Request::IndexRepo {
            repo: missing.to_string_lossy().into_owned(),
        });
        assert!(
            matches!(&r, Response::Error { message } if message.contains("not a directory")),
            "{r:?}"
        );
        assert!(d.indexing.lock().unwrap().is_empty());
    }

    /// Read one reply line (the test fails instead of hanging if none comes).
    fn reply(s: &UnixStream) -> String {
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).unwrap();
        line
    }

    fn ping(s: &UnixStream) -> String {
        let mut w = s;
        w.write_all(b"{\"op\":\"ping\"}\n").unwrap();
        reply(s)
    }

    #[test]
    fn an_oversized_request_line_is_refused_without_buffering_it() {
        let (d, _) = daemon_with_code();
        let (client, server) = UnixStream::pair().unwrap();
        let t = std::thread::spawn(move || serve_conn(d, server, &|| {}, &Limits::default()));
        // 8 MiB with no newline, from another thread: the daemon must answer after reading at
        // most MAX_REQUEST_BYTES + 1 of it and close, not wait for the end of the line.
        let w = client.try_clone().unwrap();
        w.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
        let writer = std::thread::spawn(move || {
            let chunk = vec![b'a'; 64 * 1024];
            for _ in 0..128 {
                if (&w).write_all(&chunk).is_err() {
                    return false; // the daemon closed the connection
                }
            }
            true
        });
        let r = reply(&client);
        assert!(r.contains("\"error\"") && r.contains("too long"), "{r}");
        t.join().unwrap();
        assert!(
            !writer.join().unwrap(),
            "the daemon read the whole oversized line"
        );
        assert_eq!(MAX_REQUEST_BYTES, 1 << 20);
    }

    #[test]
    fn a_request_just_under_the_cap_is_served() {
        let (d, _) = daemon_with_code();
        let (client, server) = UnixStream::pair().unwrap();
        let limits = Limits {
            max_request_bytes: 64,
            ..Limits::default()
        };
        let t = std::thread::spawn(move || serve_conn(d, server, &|| {}, &limits));
        let pad = " ".repeat(64 - "{\"op\":\"ping\"}".len());
        (&client)
            .write_all(format!("{{\"op\":\"ping\"}}{pad}\n").as_bytes())
            .unwrap();
        assert!(reply(&client).contains("pong"));
        client.shutdown(std::net::Shutdown::Both).unwrap();
        t.join().unwrap();
    }

    fn start_server(tag: &str, limits: Limits) -> (PathBuf, PathBuf) {
        let dir = tempfile_dir(tag);
        let sock = dir.join("laya.sock");
        let listener = bind_single(&sock).unwrap().unwrap();
        let (d, _) = daemon_with_code();
        std::thread::spawn(move || serve(listener, d, Arc::new(|| {}), limits));
        (dir, sock)
    }

    #[test]
    fn connections_over_the_cap_get_an_immediate_error() {
        let limits = Limits {
            max_conns: 2,
            ..Limits::default()
        };
        let (dir, sock) = start_server("cap", limits);
        let a = UnixStream::connect(&sock).unwrap();
        let b = UnixStream::connect(&sock).unwrap();
        assert!(ping(&a).contains("pong") && ping(&b).contains("pong"));
        let c = UnixStream::connect(&sock).unwrap();
        let r = reply(&c);
        assert!(r.contains("\"error\"") && r.contains("busy"), "{r}");
        drop(a);
        // The freed slot is reused once the daemon sees `a` close.
        let t0 = std::time::Instant::now();
        loop {
            let d = UnixStream::connect(&sock).unwrap();
            if ping(&d).contains("pong") {
                break;
            }
            assert!(t0.elapsed() < Duration::from_secs(5), "slot never freed");
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(b);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn idle_and_unread_connections_time_out_and_free_their_slot() {
        let limits = Limits {
            max_conns: 1,
            read_timeout: Duration::from_millis(200),
            write_timeout: Duration::from_millis(200),
            ..Limits::default()
        };
        let (dir, sock) = start_server("timeouts", limits);
        // Idle: the daemon closes it after the read timeout.
        let idle = UnixStream::connect(&sock).unwrap();
        assert_eq!(reply(&idle), "", "idle connection closed");
        // A client that sends requests but never reads replies: once the socket buffer is full
        // the daemon's write times out and the connection is dropped.
        let greedy = UnixStream::connect(&sock).unwrap();
        greedy
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let line = b"{\"op\":\"ping\"}\n".repeat(64);
        let t0 = std::time::Instant::now();
        while (&greedy).write_all(&line).is_ok() {
            assert!(
                t0.elapsed() < Duration::from_secs(20),
                "daemon never gave up"
            );
        }
        let t0 = std::time::Instant::now();
        loop {
            let d = UnixStream::connect(&sock).unwrap();
            if ping(&d).contains("pong") {
                break;
            }
            assert!(t0.elapsed() < Duration::from_secs(5), "slot never freed");
            std::thread::sleep(Duration::from_millis(50));
        }
        drop(greedy);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn query_sizes_are_clamped_to_the_mcp_bounds() {
        let cfg = RetrieverConfig {
            k_candidates: 100,
            max_total_lines: 100_000,
            ..RetrieverConfig::default()
        };
        let d = Daemon::new(Arc::new(MemoStore::default()), cfg);
        let repo = std::env::temp_dir().to_string_lossy().into_owned();
        let (_, id) = d.repo(&repo);
        for i in 0..40 {
            let c = Chunk {
                path: format!("src/f{i}.rs"),
                start_line: 1,
                end_line: 3,
                lang: Lang::Rust,
                symbol: format!("fn flush_page{i}"),
                kind: "function_item".into(),
                defines: vec![format!("flush_page{i}")],
                refs: vec![],
                text: format!("fn flush_page{i}() {{\n    flush page\n}}"),
            };
            d.store
                .put_file(&id, &c.path, "h", std::slice::from_ref(&c))
                .unwrap();
        }
        let spans = |top_n| match d.handle(Request::Query {
            repo: repo.clone(),
            session: None,
            prompt: "flush page".into(),
            budget_ms: Some(0),
            top_n: Some(top_n),
            render: None,
        }) {
            Response::Query { result, .. } => result.spans.len(),
            other => panic!("{other:?}"),
        };
        assert_eq!(spans(1_000_000), crate::protocol::MAX_TOP_N);
        assert_eq!(spans(0), 1);
        assert_eq!(spans(5), 5);
    }

    fn tempfile_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("laya-dmn-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
