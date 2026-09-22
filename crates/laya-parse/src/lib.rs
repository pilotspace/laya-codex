//! # laya-parse
//!
//! Turns a repository into retrieval chunks for laya-codex:
//!
//! * [`walk_repo`]: gitignore-aware file discovery (hidden, generated and vendored paths skipped).
//! * [`detect_lang`]: path-based language detection; `None` means "do not index".
//! * [`file_hash`]: blake3 content hash used to skip unchanged files.

mod lang;
mod walk;

pub use lang::{MAX_FILE_BYTES, detect_lang, lang_for_path};
pub use walk::{file_hash, walk_repo};
