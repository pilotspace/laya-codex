//! The candidate lists laya-code is asked to rerank in production, for training data.
//!
//! Train/serve parity is the point: instead of re-implementing candidate generation in Python,
//! this runs the production [`Retriever`] (task focus, then BM25 ⊕ defining ⊕ path, RRF, top 24,
//! non-code demotion) over the production [`laya_store::MoonStore`], and records what the
//! retriever hands to the scorer: the focus text and the candidate chunks in lexical order.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use laya_core::{Chunk, Scorer, Store};
use laya_rank::{Retriever, RetrieverConfig};

/// What the retriever asked the scorer: the task focus and the candidates, best lexical first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Recorded {
    pub focus: String,
    pub chunks: Vec<Chunk>,
}

/// A `Scorer` that scores nothing and remembers its last call.
#[derive(Default)]
pub struct Recorder {
    last: Mutex<Option<Recorded>>,
}

impl Recorder {
    pub fn take(&self) -> Option<Recorded> {
        self.last.lock().ok()?.take()
    }
}

impl Scorer for Recorder {
    fn score(&self, task: &str, chunks: &[&Chunk]) -> laya_core::Result<Vec<f32>> {
        if let Ok(mut last) = self.last.lock() {
            *last = Some(Recorded {
                focus: task.to_string(),
                chunks: chunks.iter().map(|c| (*c).clone()).collect(),
            });
        }
        Ok(vec![0.0; chunks.len()])
    }
}

/// The production retriever configuration, with the model asked about every candidate (not only
/// the first `score_top`) and no related-code expansion (not part of the candidates).
pub fn recording_config() -> RetrieverConfig {
    RetrieverConfig {
        laya_budget: Duration::from_secs(60),
        use_laya: true,
        laya_weight: Some(0.5),
        p_threshold: 0.0,
        score_top: 0,
        max_related: 0,
        ..RetrieverConfig::default()
    }
}

/// Run the production candidate generation for `prompt` and return what the scorer would see.
/// `None` when the prompt yields no candidates (the scorer is never called).
pub fn candidates(
    store: &dyn Store,
    repo: &str,
    prompt: &str,
) -> laya_core::Result<Option<Recorded>> {
    let recorder = Arc::new(Recorder::default());
    let scorer: Arc<dyn Scorer> = recorder.clone();
    Retriever::new(store, Some(scorer), recording_config()).query(repo, prompt)?;
    Ok(recorder.take())
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Counts from one [`index_repo`] run (the fields of `IndexStats` in `laya-cli/src/indexer.rs`).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct IndexStats {
    pub files_seen: usize,
    pub unchanged: usize,
    pub indexed: usize,
    pub chunks: usize,
    pub removed: usize,
    pub failed: usize,
    pub elapsed_ms: u64,
}

/// Incremental index of `root`: a verbatim copy of `index_repo` in `crates/laya-cli/src/indexer.rs`
/// (`laya-codex index`), which lives in a binary crate and cannot be called from here. Walk, skip
/// unchanged hashes, chunk changed files, put them, delete files that are gone. The test
/// `index_repo_is_the_production_indexer_verbatim` fails when the two drift apart.
pub fn index_repo(root: &Path, store: &dyn Store, repo: &str) -> laya_core::Result<IndexStats> {
    let t0 = Instant::now();
    store.ensure_index(repo)?;
    let files: Vec<PathBuf> = laya_parse::walk_repo(root)
        .into_iter()
        .filter(|p| laya_parse::lang_for_path(&rel(root, p)).is_some())
        .collect();
    let mut stats = IndexStats {
        files_seen: files.len(),
        ..Default::default()
    };
    let mut changed = Vec::new();
    for p in &files {
        let r = rel(root, p);
        let current = std::fs::read(p).ok().map(|b| laya_parse::file_hash(&b));
        match (current, store.file_hash(repo, &r)?) {
            (Some(c), Some(s)) if c == s => stats.unchanged += 1,
            _ => changed.push(p.clone()),
        }
    }
    for parsed in laya_parse::chunk_files(root, &changed) {
        match parsed {
            Ok(f) => {
                store.put_file(repo, &f.path, &f.hash, &f.chunks)?;
                stats.indexed += 1;
                stats.chunks += f.chunks.len();
            }
            Err(_) => stats.failed += 1,
        }
    }
    let live: HashSet<String> = files.iter().map(|p| rel(root, p)).collect();
    for old in store.list_files(repo)? {
        if !live.contains(&old) {
            store.delete_file(repo, &old)?;
            stats.removed += 1;
        }
    }
    stats.elapsed_ms = t0.elapsed().as_millis() as u64;
    Ok(stats)
}

/// JSON form of one candidate (the fields the state and the labels need).
pub fn chunk_json(rank: usize, c: &Chunk) -> serde_json::Value {
    serde_json::json!({
        "rank": rank, "path": c.path, "start": c.start_line, "end": c.end_line,
        "lang": format!("{:?}", c.lang), "kind": c.kind, "symbol": c.symbol, "text": c.text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use laya_core::Lang;
    use std::collections::HashMap;

    /// In-memory store: BM25 = number of query terms in the chunk text (ranked, deterministic).
    #[derive(Default)]
    struct Mem {
        files: Mutex<HashMap<String, (String, Vec<Chunk>)>>,
    }

    impl Store for Mem {
        fn ensure_index(&self, _: &str) -> laya_core::Result<()> {
            Ok(())
        }
        fn put_file(&self, _: &str, p: &str, h: &str, c: &[Chunk]) -> laya_core::Result<()> {
            self.files
                .lock()
                .unwrap()
                .insert(p.into(), (h.into(), c.to_vec()));
            Ok(())
        }
        fn delete_file(&self, _: &str, p: &str) -> laya_core::Result<()> {
            self.files.lock().unwrap().remove(p);
            Ok(())
        }
        fn file_hash(&self, _: &str, p: &str) -> laya_core::Result<Option<String>> {
            Ok(self.files.lock().unwrap().get(p).map(|(h, _)| h.clone()))
        }
        fn list_files(&self, _: &str) -> laya_core::Result<Vec<String>> {
            Ok(self.files.lock().unwrap().keys().cloned().collect())
        }
        fn bm25(
            &self,
            _: &str,
            terms: &[String],
            limit: usize,
        ) -> laya_core::Result<Vec<(String, f32)>> {
            let files = self.files.lock().unwrap();
            let mut hits: Vec<(String, f32)> = files
                .values()
                .flat_map(|(_, cs)| cs.iter())
                .map(|c| {
                    let t = c.text.to_lowercase();
                    (
                        c.id(),
                        terms.iter().filter(|q| t.contains(q.as_str())).count() as f32,
                    )
                })
                .filter(|(_, s)| *s > 0.0)
                .collect();
            hits.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap().then(a.0.cmp(&b.0)));
            hits.truncate(limit);
            Ok(hits)
        }
        fn chunks_defining(
            &self,
            _: &str,
            _: &[String],
            _: usize,
        ) -> laya_core::Result<Vec<String>> {
            Ok(vec![])
        }
        fn get_chunks(&self, _: &str, ids: &[String]) -> laya_core::Result<Vec<Chunk>> {
            let files = self.files.lock().unwrap();
            let all: Vec<&Chunk> = files.values().flat_map(|(_, cs)| cs.iter()).collect();
            Ok(ids
                .iter()
                .filter_map(|id| all.iter().find(|c| &c.id() == id).map(|c| (*c).clone()))
                .collect())
        }
        fn chunks_of_file(&self, _: &str, p: &str) -> laya_core::Result<Vec<Chunk>> {
            Ok(self
                .files
                .lock()
                .unwrap()
                .get(p)
                .map(|(_, c)| c.clone())
                .unwrap_or_default())
        }
        fn memo_get(&self, _: &str) -> laya_core::Result<Option<String>> {
            Ok(None)
        }
        fn memo_put(&self, _: &str, _: &str, _: u64) -> laya_core::Result<()> {
            Ok(())
        }
    }

    fn chunk(path: &str, start: u32, text: &str) -> Chunk {
        Chunk {
            path: path.into(),
            start_line: start,
            end_line: start + 9,
            lang: Lang::Rust,
            symbol: String::new(),
            kind: "function_item".into(),
            defines: vec![],
            refs: vec![],
            text: text.into(),
        }
    }

    fn store_with(n: usize) -> Mem {
        let s = Mem::default();
        for i in 0..n {
            // chunk i matches (i % 3) + 1 of the query terms: a ranked, tied list
            let words = ["eviction", "cache", "capacity"][..(i % 3) + 1].join(" ");
            let c = chunk(
                &format!("src/f{i}.rs"),
                1,
                &format!("fn f{i}() {{ {words} }}"),
            );
            s.put_file("r", &c.path.clone(), "h", &[c]).unwrap();
        }
        s
    }

    #[test]
    fn records_every_candidate_up_to_k_in_lexical_order() {
        let s = store_with(40);
        let got = candidates(&s, "r", "fix cache eviction when capacity is exceeded")
            .unwrap()
            .expect("candidates");
        assert_eq!(
            got.chunks.len(),
            24,
            "all k_candidates reach the scorer, not only score_top"
        );
        // the three-term matches come first (lexical order preserved)
        assert!(got.chunks[..10].iter().all(|c| c.text.contains("capacity")));
        assert_eq!(got.focus, "fix cache eviction when capacity is exceeded");
    }

    #[test]
    fn focus_is_the_quoted_task_of_a_wrapped_prompt() {
        let s = store_with(5);
        let prompt = "In this repository, find the source code for the following change:\n\n\"fix cache eviction when capacity is exceeded\"\n\nBe efficient.";
        let got = candidates(&s, "r", prompt).unwrap().expect("candidates");
        assert_eq!(got.focus, "fix cache eviction when capacity is exceeded");
    }

    #[test]
    fn no_candidates_is_none() {
        let s = store_with(3);
        assert_eq!(
            candidates(&s, "r", "unrelated words entirely").unwrap(),
            None
        );
    }

    #[test]
    fn config_matches_production_except_scoring_all_and_no_expansion() {
        let c = recording_config();
        let prod = RetrieverConfig::default();
        assert_eq!(c.k_candidates, prod.k_candidates);
        assert_eq!(c.rrf_k, prod.rrf_k);
        assert_eq!(c.score_top, 0);
        assert_eq!(c.max_related, 0);
        assert!(c.use_laya);
    }

    /// `fn <name>(` ... the closing `}` at column 0, from a source file.
    fn function_text(src: &str, signature: &str) -> String {
        let start = src
            .find(signature)
            .unwrap_or_else(|| panic!("{signature} not found"));
        let end = src[start..].find("\n}\n").expect("function end") + start + 3;
        src[start..end].to_string()
    }

    /// Drift guard: `index_repo` (and its `rel` helper) must stay the production indexer's code,
    /// byte for byte. laya-cli is a binary crate, so its function cannot be called from here.
    #[test]
    fn index_repo_is_the_production_indexer_verbatim() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let prod = std::fs::read_to_string(root.join("../../crates/laya-cli/src/indexer.rs"))
            .expect("crates/laya-cli/src/indexer.rs");
        let ours = std::fs::read_to_string(root.join("src/lib.rs")).expect("src/lib.rs");
        for sig in [
            "pub fn index_repo(root: &Path, store: &dyn Store, repo: &str)",
            "fn rel(root: &Path, p: &Path) -> String",
        ] {
            assert_eq!(
                function_text(&ours, sig),
                function_text(&prod, sig),
                "{sig} drifted from crates/laya-cli/src/indexer.rs"
            );
        }
    }

    /// Behaviour on a fixture repo: the store holds exactly the files the production walk admits,
    /// with exactly the chunks `laya_parse::parse_file` makes for them.
    #[test]
    fn index_repo_stores_the_parse_file_chunks_of_admitted_files() {
        let dir = std::env::temp_dir().join(format!("candgen-fx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (p, text) in [
            (
                "src/a.rs",
                "pub fn alpha(x: u32) -> u32 {\n    x + 1\n}\n\nfn beta() {}\n",
            ),
            (
                "pkg/b.py",
                "class B:\n    def run(self):\n        return 1\n",
            ),
            (".hidden/c.rs", "fn hidden() {}\n"),
            ("ignored.rs", "fn ignored() {}\n"),
            ("notes.bin", "\u{0}\u{1}"),
            (".gitignore", "ignored.rs\n"),
        ] {
            std::fs::create_dir_all(dir.join(p).parent().unwrap()).unwrap();
            std::fs::write(dir.join(p), text).unwrap();
        }
        let s = Mem::default();
        index_repo(&dir, &s, "r").unwrap();
        let mut files = s.list_files("r").unwrap();
        files.sort();
        assert_eq!(files, ["pkg/b.py", "src/a.rs"]);
        for f in &files {
            let want = laya_parse::parse_file(&dir, &dir.join(f)).unwrap().chunks;
            assert_eq!(s.chunks_of_file("r", f).unwrap(), want, "{f}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn index_repo_is_incremental_and_drops_deleted_files() {
        let dir = std::env::temp_dir().join(format!("candgen-idx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn a() {\n    let x = 1;\n}\n").unwrap();
        std::fs::write(dir.join("src/b.rs"), "fn b() {\n    let y = 2;\n}\n").unwrap();
        let s = Mem::default();
        let first = index_repo(&dir, &s, "r").unwrap();
        assert_eq!((first.files_seen, first.indexed), (2, 2));
        std::fs::remove_file(dir.join("src/b.rs")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn a() {\n    let x = 3;\n}\n").unwrap();
        let second = index_repo(&dir, &s, "r").unwrap();
        assert_eq!(
            (second.files_seen, second.indexed, second.removed),
            (1, 1, 1)
        );
        let third = index_repo(&dir, &s, "r").unwrap();
        assert_eq!(third.indexed, 0, "unchanged files are skipped");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
