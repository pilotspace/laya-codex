//! # laya-parse
//!
//! Turns a repository into retrieval chunks for laya-codex:
//!
//! * [`walk_repo`]: gitignore-aware file discovery (hidden, generated and vendored paths skipped).
//! * [`detect_lang`]: path-based language detection; `None` means "do not index".
//! * [`chunk_source`] / [`chunk_source_with`]: cAST-style AST-aligned chunking into 10–50-line
//!   spans with a symbol path, dominant node kind and defined identifiers. Unknown text files
//!   and unparseable sources fall back to line windows.
//! * [`parse_file`] / [`chunk_files`]: read + validate + chunk files from disk (rayon-parallel).
//! * [`file_hash`]: blake3 content hash used to skip unchanged files.
//!
//! ```
//! let src = "fn add(a: u32, b: u32) -> u32 {\n    a + b\n}\n";
//! let chunks = laya_parse::chunk_source("src/math.rs", src);
//! assert_eq!(chunks.len(), 1);
//! assert_eq!(chunks[0].symbol, "fn add");
//! assert_eq!(chunks[0].defines, vec!["add"]);
//! ```

mod cast;
mod defs;
mod lang;
mod lines;
mod text;
mod walk;

use std::cell::RefCell;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use laya_core::{Chunk, Lang};
use rayon::prelude::*;
use tree_sitter::{ParseOptions, ParseState, Parser, Tree};

pub use lang::{MAX_FILE_BYTES, detect_lang, lang_for_path};
pub use walk::{file_hash, walk_repo};

use lang::{LANG_COUNT, grammar, lang_index};
use lines::Lines;

/// Wall-clock budget for one tree-sitter parse; on expiry the file falls back to line windows.
const PARSE_DEADLINE: Duration = Duration::from_secs(2);

/// Chunk size policy. Line counts are inclusive spans (blank lines inside a chunk count).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkConfig {
    /// Chunks smaller than this merge into a neighbour when the result fits `max_lines`.
    pub min_lines: usize,
    /// Hard upper bound on a chunk's span.
    pub max_lines: usize,
    /// Window length for the text fallback (capped at `max_lines`).
    pub text_window_lines: usize,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            min_lines: 10,
            max_lines: 50,
            text_window_lines: 30,
        }
    }
}

/// Why a file on disk was not chunked.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("unsupported or excluded file: {path}")]
    Unsupported { path: String },
    #[error("file too large ({bytes} bytes): {path}")]
    TooLarge { path: String, bytes: u64 },
    #[error("cannot read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("binary content: {path}")]
    Binary { path: String },
    #[error("not valid UTF-8: {path}")]
    NotUtf8 { path: String },
    #[error("minified or single-line generated content: {path}")]
    Minified { path: String },
}

/// One chunked file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFile {
    /// Repo-relative path with `/` separators.
    pub path: String,
    pub lang: Lang,
    /// [`file_hash`] of the raw bytes.
    pub hash: String,
    pub chunks: Vec<Chunk>,
}

thread_local! {
    static PARSERS: RefCell<[Option<Parser>; LANG_COUNT]> = RefCell::new(std::array::from_fn(|_| None));
}

/// Parse with this thread's cached parser for `lang`. `None` on grammar/ABI failure or deadline.
fn parse(lang: Lang, source: &str) -> Option<Tree> {
    let language = grammar(lang)?;
    PARSERS.with(|cell| {
        let mut parsers = cell.borrow_mut();
        let slot = &mut parsers[lang_index(lang)];
        if slot.is_none() {
            let mut p = Parser::new();
            p.set_language(&language).ok()?;
            *slot = Some(p);
        }
        let parser = slot.as_mut()?;
        let bytes = source.as_bytes();
        let started = Instant::now();
        let mut on_progress = |_: &ParseState| {
            if started.elapsed() > PARSE_DEADLINE {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let options = ParseOptions::new().progress_callback(&mut on_progress);
        let tree = parser.parse_with_options(
            &mut |i, _| bytes.get(i..).unwrap_or(&[]),
            None,
            Some(options),
        );
        if tree.is_none() {
            parser.reset();
        }
        tree
    })
}

/// Chunk `source` with the default [`ChunkConfig`] (10–50 lines).
pub fn chunk_source(path: &str, source: &str) -> Vec<Chunk> {
    chunk_source_with(&ChunkConfig::default(), path, source)
}

/// Chunk `source`, choosing the grammar from `path` (unknown extensions use line windows).
///
/// Guarantees: chunks are sorted, never overlap, cover every non-blank line, start and end
/// on non-blank lines, span at most `max_lines`, and `text` is the exact source of the span.
pub fn chunk_source_with(cfg: &ChunkConfig, path: &str, source: &str) -> Vec<Chunk> {
    let cfg = ChunkConfig {
        min_lines: cfg.min_lines.max(1),
        max_lines: cfg.max_lines.max(1),
        text_window_lines: cfg.text_window_lines.max(1),
    };
    let path = path.replace('\\', "/");
    let lines = Lines::new(source);
    if lines.len() == 0 {
        return Vec::new();
    }
    let lang = lang_for_path(&path).unwrap_or(Lang::Text);
    if let Some(language) = grammar(lang)
        && let Some(tree) = parse(lang, source)
    {
        let table = defs::kind_table(lang, &language);
        let defs = defs::collect_defs(table, tree.root_node(), source);
        return cast::chunk_tree(&cfg, lang, &path, &lines, &tree, table, &defs);
    }
    text::chunk_text(&cfg, &path, &lines)
}

/// Heuristic for bundles/minified output: very long lines or a very high mean line length.
fn looks_minified(src: &str) -> bool {
    let longest = src.split('\n').map(str::len).max().unwrap_or(0);
    let rows = src.bytes().filter(|&b| b == b'\n').count() + 1;
    longest > 5_000 || (src.len() > 20_000 && src.len() / rows > 300)
}

/// Read, validate and chunk one file. `root` is used to derive the repo-relative path.
pub fn parse_file(root: &Path, path: &Path) -> Result<ParsedFile, ParseError> {
    parse_file_with(&ChunkConfig::default(), root, path)
}

/// [`parse_file`] with an explicit config.
pub fn parse_file_with(
    cfg: &ChunkConfig,
    root: &Path,
    path: &Path,
) -> Result<ParsedFile, ParseError> {
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let lang = lang_for_path(&rel).ok_or_else(|| ParseError::Unsupported { path: rel.clone() })?;
    let io = |source| ParseError::Io {
        path: rel.clone(),
        source,
    };
    let len = std::fs::metadata(path).map_err(io)?.len();
    if len > MAX_FILE_BYTES {
        return Err(ParseError::TooLarge {
            path: rel,
            bytes: len,
        });
    }
    let bytes = std::fs::read(path).map_err(io)?;
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return Err(ParseError::Binary { path: rel });
    }
    let hash = file_hash(&bytes);
    let source = String::from_utf8(bytes).map_err(|_| ParseError::NotUtf8 { path: rel.clone() })?;
    if looks_minified(&source) {
        return Err(ParseError::Minified { path: rel });
    }
    let chunks = chunk_source_with(cfg, &rel, &source);
    Ok(ParsedFile {
        path: rel,
        lang,
        hash,
        chunks,
    })
}

/// Parse and chunk `files` in parallel (rayon; one cached parser per thread). Results are in
/// input order; per-file failures are reported, never fatal.
pub fn chunk_files(root: &Path, files: &[PathBuf]) -> Vec<Result<ParsedFile, ParseError>> {
    chunk_files_with(&ChunkConfig::default(), root, files)
}

/// [`chunk_files`] with an explicit config.
pub fn chunk_files_with(
    cfg: &ChunkConfig,
    root: &Path,
    files: &[PathBuf],
) -> Vec<Result<ParsedFile, ParseError>> {
    files
        .par_iter()
        .map(|p| parse_file_with(cfg, root, p))
        .collect()
}
