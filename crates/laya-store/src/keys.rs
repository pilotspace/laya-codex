//! Key schema. Everything a repo owns lives under `lc:{repo}:` so repos never collide and a
//! repo can be dropped with one prefix scan.
//!
//! | key                       | type | content                                             |
//! |---------------------------|------|-----------------------------------------------------|
//! | `lc:{repo}:c:{chunk_id}`  | HASH | path, start, end, lang, symbol, kind, defines, terms, text |
//! | `lc:{repo}:f:{path}`      | HASH | hash, chunks (comma ids), defines (newline idents)  |
//! | `lc:{repo}:files`         | SET  | indexed paths                                       |
//! | `lc:{repo}:d:{ident}`     | SET  | chunk ids defining `ident` (exact, case-sensitive)  |
//! | `lc:{repo}:df`            | HASH | term -> number of chunks containing it (query planning) |
//! | `lc:{repo}:idx`           | FT   | `ON HASH PREFIX lc:{repo}:c: SCHEMA terms TEXT`     |
//! | `lc:memo:{key}`           | STR  | memo cache value (optional TTL)                     |

use std::path::Path;

/// Stable repo namespace: first 12 hex chars of blake3 of the absolute repo path.
#[must_use]
pub fn repo_id(abs_repo_path: &Path) -> String {
    let h = blake3::hash(abs_repo_path.as_os_str().as_encoded_bytes());
    h.to_hex()[..12].to_string()
}

#[must_use]
pub fn chunk(repo: &str, id: &str) -> String {
    format!("lc:{repo}:c:{id}")
}

#[must_use]
pub fn chunk_prefix(repo: &str) -> String {
    format!("lc:{repo}:c:")
}

#[must_use]
pub fn file(repo: &str, path: &str) -> String {
    format!("lc:{repo}:f:{path}")
}

#[must_use]
pub fn files(repo: &str) -> String {
    format!("lc:{repo}:files")
}

#[must_use]
pub fn defines(repo: &str, ident: &str) -> String {
    format!("lc:{repo}:d:{ident}")
}

#[must_use]
pub fn index(repo: &str) -> String {
    format!("lc:{repo}:idx")
}

#[must_use]
pub fn df(repo: &str) -> String {
    format!("lc:{repo}:df")
}

#[must_use]
pub fn memo(key: &str) -> String {
    format!("lc:memo:{key}")
}

/// Split a comma-joined list stored in a hash field, skipping empties.
pub fn split_list(s: &str) -> impl Iterator<Item = &str> {
    s.split(',').filter(|p| !p.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_id_is_12_hex_and_stable() {
        let a = repo_id(Path::new("/tmp/repo"));
        assert_eq!(a.len(), 12);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(a, repo_id(Path::new("/tmp/repo")));
        assert_ne!(a, repo_id(Path::new("/tmp/repo2")));
    }

    #[test]
    fn keys_are_namespaced() {
        assert_eq!(chunk("r", "abc"), "lc:r:c:abc");
        assert!(chunk("r", "abc").starts_with(&chunk_prefix("r")));
        assert_eq!(file("r", "src/a b.rs"), "lc:r:f:src/a b.rs");
        assert_eq!(files("r"), "lc:r:files");
        assert_eq!(defines("r", "Foo"), "lc:r:d:Foo");
        assert_eq!(index("r"), "lc:r:idx");
        assert_eq!(df("r"), "lc:r:df");
        assert_eq!(memo("q:1"), "lc:memo:q:1");
    }

    #[test]
    fn split_list_skips_empty() {
        assert_eq!(split_list("a,,b,").collect::<Vec<_>>(), vec!["a", "b"]);
        assert_eq!(split_list("").count(), 0);
    }
}
