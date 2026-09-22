//! Gitignore-aware repository walking and content hashing.

use std::path::{Path, PathBuf};

use crate::lang::{MAX_FILE_BYTES, lang_for_path};

/// All indexable files under `root`, sorted. Respects `.gitignore`/`.ignore`/git excludes
/// (even outside a git checkout), skips hidden entries, generated/vendored directories,
/// symlinks, files whose language is unknown and files larger than 1 MiB.
/// Unreadable entries are skipped rather than aborting the walk (fail-open).
pub fn walk_repo(root: &Path) -> Vec<PathBuf> {
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .ignore(true)
        .parents(true)
        .require_git(false)
        .follow_links(false)
        .build();
    let mut out = Vec::new();
    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let rel = path.strip_prefix(root).unwrap_or(path);
        if lang_for_path(&rel.to_string_lossy()).is_none() {
            continue;
        }
        if entry
            .metadata()
            .map(|m| m.len() > MAX_FILE_BYTES)
            .unwrap_or(true)
        {
            continue;
        }
        out.push(entry.into_path());
    }
    out.sort();
    out
}

/// Content hash of a file: blake3, lowercase hex, first 32 chars (128 bits).
pub fn file_hash(bytes: &[u8]) -> String {
    let hex = blake3::hash(bytes).to_hex();
    hex[..32].to_string()
}
