//! `laya daemon`: long-lived process that keeps Moon supervised, the Laya model warm and
//! per-session state in memory. Clients speak the JSON-lines protocol in `protocol.rs`.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use laya_core::{Chunk, Scorer, Store};
use laya_rank::{Retriever, RetrieverConfig};

use crate::config::{Config, rel_path, repo_root};
use crate::indexer;
use crate::protocol::{Request, Response};
use crate::session::Sessions;

/// Caches Laya probabilities in the store, keyed by (task, chunk content), so repeated or
/// follow-up prompts in a session skip inference for chunks already judged.
pub struct MemoScorer {
    inner: Arc<dyn Scorer>,
    store: Arc<dyn Store>,
    model_tag: String,
    /// Set while a model run is in flight. A query that finds the model busy degrades to
    /// lexical ranking instead of queueing behind abandoned (timed-out) runs.
    busy: std::sync::atomic::AtomicBool,
}

struct BusyGuard<'a>(&'a std::sync::atomic::AtomicBool);
impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

impl MemoScorer {
    pub fn new(inner: Arc<dyn Scorer>, store: Arc<dyn Store>, model_tag: &str) -> Self {
        MemoScorer { inner, store, model_tag: model_tag.to_string(), busy: Default::default() }
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
            match self.store.memo_get(k).ok().flatten().and_then(|v| v.parse::<f32>().ok()) {
                Some(p) => out[i] = p,
                None => miss.push(i),
            }
        }
        if !miss.is_empty() {
            use std::sync::atomic::Ordering;
            if self.busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
                return Err(laya_core::Error::Model("model busy".into()));
            }
            let _guard = BusyGuard(&self.busy);
            let todo: Vec<&Chunk> = miss.iter().map(|&i| chunks[i]).collect();
            let t0 = std::time::Instant::now();
            let ps = self.inner.score(task, &todo)?;
            eprintln!("[laya] scored {} chunks ({} cached) in {:?}", todo.len(), chunks.len() - todo.len(), t0.elapsed());
            for (&i, p) in miss.iter().zip(ps) {
                out[i] = p;
                let _ = self.store.memo_put(&keys[i], &format!("{p:.5}"), 86_400);
            }
        }
        Ok(out)
    }
}

pub struct Daemon {
    pub store: Arc<dyn Store>,
    pub scorer: RwLock<Option<Arc<dyn Scorer>>>,
    pub sessions: Mutex<Sessions>,
    pub indexing: Mutex<HashSet<String>>,
    pub base_cfg: RetrieverConfig,
}

impl Daemon {
    pub fn new(store: Arc<dyn Store>, base_cfg: RetrieverConfig) -> Arc<Self> {
        Arc::new(Daemon {
            store,
            scorer: RwLock::new(None),
            sessions: Mutex::new(Sessions::default()),
            indexing: Mutex::new(HashSet::new()),
            base_cfg,
        })
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
            Request::Query { repo, session, prompt, budget_ms, top_n } => {
                let (_, id) = self.repo(&repo);
                let mut cfg = self.base_cfg.clone();
                if let Some(b) = budget_ms {
                    cfg.laya_budget = Duration::from_millis(b);
                }
                if let Some(n) = top_n {
                    cfg.top_n = n;
                }
                let scorer = self.scorer.read().ok().and_then(|s| s.clone());
                let retriever = Retriever::new(self.store.as_ref(), scorer, cfg);
                match retriever.query(&id, &prompt) {
                    Ok(result) => {
                        if let Some(s) = session {
                            if let Ok(mut sessions) = self.sessions.lock() {
                                sessions.record_query(&s, &result);
                            }
                        }
                        Response::Query { result }
                    }
                    Err(e) => Response::Error { message: e.to_string() },
                }
            }
            Request::NoteRead { session, path } => match self.sessions.lock() {
                Ok(mut s) => Response::Count { count: s.note_read(&session, &path) },
                Err(_) => Response::Error { message: "session lock poisoned".into() },
            },
            Request::Session { session } => match self.sessions.lock() {
                Ok(mut s) => Response::Session { view: s.view(&session) },
                Err(_) => Response::Error { message: "session lock poisoned".into() },
            },
            Request::ReindexFile { repo, path } => {
                let (root, id) = self.repo(&repo);
                let Some(rel) = rel_path(&root, &path) else { return Response::Ok };
                match indexer::index_file(&root, self.store.as_ref(), &id, &rel) {
                    Ok(_) => Response::Ok,
                    Err(e) => Response::Error { message: e.to_string() },
                }
            }
            Request::IndexRepo { repo } => {
                let (root, id) = self.repo(&repo);
                let fresh = self.indexing.lock().map(|mut s| s.insert(id.clone())).unwrap_or(false);
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
        }
    }
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
            Err(e) => Response::Error { message: format!("bad request: {e}") },
        };
        let Ok(mut s) = serde_json::to_string(&resp) else { return };
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
        eprintln!("[laya] daemon already running at {}", cfg.socket_path().display());
        return Ok(());
    };
    let sup = laya_store::MoonSupervisor::new(&cfg.moon_bin, cfg.moon_port, cfg.moon_dir());
    sup.ensure_running().map_err(|e| anyhow::anyhow!("moon: {e}"))?;
    let store: Arc<dyn Store> = Arc::new(laya_store::MoonStore::new(laya_store::StoreConfig::local(cfg.moon_port))?);
    let mut base = RetrieverConfig::default();
    base.laya_budget = Duration::from_millis(cfg.budget_ms);
    base.use_laya = cfg.use_model;
    let daemon = Daemon::new(Arc::clone(&store), base);

    if cfg.use_model {
        if let Some(dir) = cfg.model_dir.clone() {
            let d = Arc::clone(&daemon);
            std::thread::spawn(move || {
                let t0 = std::time::Instant::now();
                match laya_model::LayaModel::load(&dir, laya_model::DeviceKind::Auto) {
                    Ok(model) => {
                        let tag = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        let inner: Arc<dyn Scorer> = Arc::new(laya_model::LayaScorer::new(model));
                        let memo: Arc<dyn Scorer> = Arc::new(MemoScorer::new(inner, Arc::clone(&d.store), &tag));
                        if let Ok(mut s) = d.scorer.write() {
                            *s = Some(memo);
                        }
                        eprintln!("[laya] model {} ready in {:?}", dir.display(), t0.elapsed());
                    }
                    Err(e) => eprintln!("[laya] model load failed ({e}); serving lexical ranking"),
                }
            });
        }
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
    }
    impl Store for MemoStore {
        fn ensure_index(&self, r: &str) -> laya_core::Result<()> { self.inner.ensure_index(r) }
        fn put_file(&self, r: &str, p: &str, h: &str, c: &[Chunk]) -> laya_core::Result<()> { self.inner.put_file(r, p, h, c) }
        fn delete_file(&self, r: &str, p: &str) -> laya_core::Result<()> { self.inner.delete_file(r, p) }
        fn file_hash(&self, r: &str, p: &str) -> laya_core::Result<Option<String>> { self.inner.file_hash(r, p) }
        fn list_files(&self, r: &str) -> laya_core::Result<Vec<String>> { self.inner.list_files(r) }
        fn bm25(&self, r: &str, t: &[String], l: usize) -> laya_core::Result<Vec<(String, f32)>> { self.inner.bm25(r, t, l) }
        fn chunks_defining(&self, r: &str, i: &[String], l: usize) -> laya_core::Result<Vec<String>> { self.inner.chunks_defining(r, i, l) }
        fn get_chunks(&self, r: &str, i: &[String]) -> laya_core::Result<Vec<Chunk>> { self.inner.get_chunks(r, i) }
        fn memo_get(&self, k: &str) -> laya_core::Result<Option<String>> { Ok(self.memo.lock().unwrap().get(k).cloned()) }
        fn memo_put(&self, k: &str, v: &str, _: u64) -> laya_core::Result<()> { self.memo.lock().unwrap().insert(k.into(), v.into()); Ok(()) }
    }

    fn chunk(start: u32) -> Chunk {
        Chunk { path: "a.rs".into(), start_line: start, end_line: start + 9, lang: Lang::Rust, symbol: String::new(),
            kind: "function_item".into(), defines: vec![], text: format!("fn f{start}() {{}}") }
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
    fn daemon_tracks_reads_and_sessions() {
        let d = Daemon::new(Arc::new(MemoStore::default()), RetrieverConfig::default());
        assert_eq!(d.handle(Request::NoteRead { session: "s".into(), path: "a.rs".into() }), Response::Count { count: 1 });
        assert_eq!(d.handle(Request::NoteRead { session: "s".into(), path: "a.rs".into() }), Response::Count { count: 2 });
        assert!(matches!(d.handle(Request::Ping), Response::Pong { model_ready: false, .. }));
        assert!(matches!(d.handle(Request::Session { session: "s".into() }), Response::Session { .. }));
    }
}
