//! Incremental repo indexing: walk → hash-skip unchanged → parse/chunk changed files in parallel
//! → write to the store → drop files that no longer exist.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use laya_core::Store;
use serde::Serialize;

#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct IndexStats {
    pub files_seen: usize,
    pub unchanged: usize,
    pub indexed: usize,
    pub chunks: usize,
    pub removed: usize,
    pub failed: usize,
    pub elapsed_ms: u64,
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

pub fn index_repo(root: &Path, store: &dyn Store, repo: &str) -> laya_core::Result<IndexStats> {
    let t0 = Instant::now();
    store.ensure_index(repo)?;
    let files: Vec<PathBuf> = laya_parse::walk_repo(root)
        .into_iter()
        .filter(|p| laya_parse::detect_lang(&rel(root, p)).is_some())
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

/// Re-index one file (after an edit). Deletes it from the store if it is gone or unsupported.
pub fn index_file(
    root: &Path,
    store: &dyn Store,
    repo: &str,
    rel_path: &str,
) -> laya_core::Result<usize> {
    let abs = root.join(rel_path);
    if !abs.is_file() || laya_parse::detect_lang(rel_path).is_none() {
        store.delete_file(repo, rel_path)?;
        return Ok(0);
    }
    match laya_parse::parse_file(root, &abs) {
        Ok(f) => {
            store.put_file(repo, &f.path, &f.hash, &f.chunks)?;
            Ok(f.chunks.len())
        }
        Err(_) => {
            store.delete_file(repo, rel_path)?;
            Ok(0)
        }
    }
}

#[cfg(test)]
pub mod mem {
    //! In-memory `Store` used by CLI tests.
    use std::collections::HashMap;
    use std::sync::Mutex;

    use laya_core::{Chunk, Result, Store};

    #[derive(Default)]
    pub struct MemStore {
        pub files: Mutex<HashMap<String, (String, Vec<Chunk>)>>,
        pub puts: Mutex<usize>,
    }

    impl Store for MemStore {
        fn ensure_index(&self, _: &str) -> Result<()> {
            Ok(())
        }
        fn put_file(&self, _: &str, path: &str, hash: &str, chunks: &[Chunk]) -> Result<()> {
            *self.puts.lock().unwrap() += 1;
            self.files
                .lock()
                .unwrap()
                .insert(path.into(), (hash.into(), chunks.to_vec()));
            Ok(())
        }
        fn delete_file(&self, _: &str, path: &str) -> Result<()> {
            self.files.lock().unwrap().remove(path);
            Ok(())
        }
        fn file_hash(&self, _: &str, path: &str) -> Result<Option<String>> {
            Ok(self.files.lock().unwrap().get(path).map(|(h, _)| h.clone()))
        }
        fn list_files(&self, _: &str) -> Result<Vec<String>> {
            Ok(self.files.lock().unwrap().keys().cloned().collect())
        }
        fn bm25(&self, _: &str, _: &[String], _: usize) -> Result<Vec<(String, f32)>> {
            Ok(vec![])
        }
        fn chunks_defining(&self, _: &str, _: &[String], _: usize) -> Result<Vec<String>> {
            Ok(vec![])
        }
        fn get_chunks(&self, _: &str, _: &[String]) -> Result<Vec<Chunk>> {
            Ok(vec![])
        }
        fn chunks_of_file(&self, _: &str, path: &str) -> Result<Vec<Chunk>> {
            let mut v = self
                .files
                .lock()
                .unwrap()
                .get(path)
                .map(|(_, c)| c.clone())
                .unwrap_or_default();
            v.sort_by_key(|c| (c.start_line, c.end_line));
            Ok(v)
        }
        fn memo_get(&self, _: &str) -> Result<Option<String>> {
            Ok(None)
        }
        fn memo_put(&self, _: &str, _: &str, _: u64) -> Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mem::MemStore;
    use super::*;

    fn tmp_repo(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("laya-idx-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("src/a.rs"), "fn alpha() -> u32 {\n    1\n}\n").unwrap();
        std::fs::write(d.join("src/b.py"), "def beta():\n    return 2\n").unwrap();
        std::fs::write(d.join("image.png"), [0u8, 1, 2, 3]).unwrap();
        d
    }

    #[test]
    fn indexes_then_skips_unchanged_then_handles_edit_and_delete() {
        let root = tmp_repo("inc");
        let s = MemStore::default();
        let first = index_repo(&root, &s, "r").unwrap();
        assert_eq!(
            (first.files_seen, first.indexed, first.unchanged),
            (2, 2, 0)
        );
        assert!(first.chunks >= 2);

        let second = index_repo(&root, &s, "r").unwrap();
        assert_eq!((second.indexed, second.unchanged), (0, 2));

        std::fs::write(root.join("src/a.rs"), "fn alpha() -> u32 {\n    42\n}\n").unwrap();
        std::fs::remove_file(root.join("src/b.py")).unwrap();
        let third = index_repo(&root, &s, "r").unwrap();
        assert_eq!((third.indexed, third.unchanged, third.removed), (1, 0, 1));
        assert!(
            s.files.lock().unwrap().get("src/a.rs").unwrap().1[0]
                .text
                .contains("42")
        );
    }

    #[test]
    fn mem_store_lists_a_files_chunks_in_line_order() {
        let root = tmp_repo("chunks");
        std::fs::write(
            root.join("src/a.rs"),
            "fn alpha() -> u32 {\n    1\n}\n\nfn beta() -> u32 {\n    2\n}\n",
        )
        .unwrap();
        let s = MemStore::default();
        index_repo(&root, &s, "r").unwrap();
        let chunks = s.chunks_of_file("r", "src/a.rs").unwrap();
        assert!(!chunks.is_empty());
        assert!(chunks.iter().all(|c| c.path == "src/a.rs"));
        assert!(
            chunks
                .windows(2)
                .all(|w| w[0].start_line <= w[1].start_line)
        );
        assert!(s.chunks_of_file("r", "src/none.rs").unwrap().is_empty());
    }

    #[test]
    fn index_file_updates_and_deletes() {
        let root = tmp_repo("one");
        let s = MemStore::default();
        assert!(index_file(&root, &s, "r", "src/a.rs").unwrap() >= 1);
        std::fs::remove_file(root.join("src/a.rs")).unwrap();
        assert_eq!(index_file(&root, &s, "r", "src/a.rs").unwrap(), 0);
        assert!(s.files.lock().unwrap().get("src/a.rs").is_none());
    }
}
