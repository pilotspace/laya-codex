//! Property-style tests: structural invariants over real files and adversarial inputs.

mod common;

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use laya_core::Lang;
use laya_parse::{
    ChunkConfig, ParseError, chunk_files, chunk_source, chunk_source_with, parse_file, walk_repo,
};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Env var naming an extra real-world repository (e.g. a moon checkout) to check.
const MOON_REPO_VAR: &str = "LAYA_CODEX_TEST_MOON_REPO";

fn check_tree(root: &Path, limit: usize) -> usize {
    let cfg = ChunkConfig::default();
    let mut n = 0;
    for path in walk_repo(root).into_iter().take(limit) {
        let Ok(src) = fs::read_to_string(&path) else {
            continue;
        };
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let chunks = chunk_source_with(&cfg, &rel, &src);
        common::assert_invariants(&cfg, &rel, &src, &chunks);
        n += 1;
    }
    n
}

#[test]
fn invariants_hold_on_every_file_of_this_repo() {
    let n = check_tree(&workspace_root(), usize::MAX);
    assert!(n > 10, "expected to check the workspace files, checked {n}");
}

#[test]
fn invariants_hold_on_moon_sample() {
    let Some(repo) = std::env::var_os(MOON_REPO_VAR) else {
        eprintln!("SKIP: set {MOON_REPO_VAR} to a large repository (e.g. a moon checkout)");
        return;
    };
    let root = Path::new(&repo);
    if !root.is_dir() {
        eprintln!(
            "SKIP: {MOON_REPO_VAR}={} is not a directory",
            root.display()
        );
        return;
    }
    let n = check_tree(root, 400);
    assert!(n > 50);
}

#[test]
fn invariants_hold_under_a_tight_config() {
    let cfg = ChunkConfig {
        min_lines: 3,
        max_lines: 7,
        ..ChunkConfig::default()
    };
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if name.ends_with(".golden") {
            continue;
        }
        let src = fs::read_to_string(&path).unwrap();
        let chunks = chunk_source_with(&cfg, &name, &src);
        common::assert_invariants(&cfg, &name, &src, &chunks);
    }
}

#[test]
fn empty_and_blank_sources_yield_no_chunks() {
    assert!(chunk_source("a.rs", "").is_empty());
    assert!(chunk_source("a.rs", "\n\n   \n\t\n").is_empty());
    assert!(chunk_source("a.md", "").is_empty());
    assert!(chunk_source("a.unknownext", "\n").is_empty());
}

#[test]
fn unknown_extension_falls_back_to_text_windows() {
    let src: String = (1..=65).map(|i| format!("line {i}\n")).collect();
    let chunks = chunk_source("data.weird", &src);
    assert!(
        chunks
            .iter()
            .all(|c| c.lang == Lang::Text && c.kind == "window")
    );
    let cfg = ChunkConfig::default();
    common::assert_invariants(&cfg, "data.weird", &src, &chunks);
    // No blank lines: hard 30-line windows; the 5-line tail merges into its predecessor.
    let spans: Vec<(u32, u32)> = chunks.iter().map(|c| (c.start_line, c.end_line)).collect();
    assert_eq!(spans, vec![(1, 30), (31, 65)]);
}

#[test]
fn text_windows_prefer_blank_lines() {
    let mut src = String::new();
    for block in 0..4 {
        for i in 0..12 {
            writeln!(src, "block {block} line {i}").unwrap();
        }
        src.push('\n');
    }
    let chunks = chunk_source("notes.txt", &src);
    let spans: Vec<(u32, u32)> = chunks.iter().map(|c| (c.start_line, c.end_line)).collect();
    // Blocks are 12 lines + 1 blank: two blocks (25 lines) fit a 30-line window.
    assert_eq!(spans, vec![(1, 25), (27, 51)]);
}

#[test]
fn crlf_sources_are_handled() {
    let src = "fn a() {\r\n    1\r\n}\r\n\r\nfn b() {\r\n    2\r\n}\r\n";
    let chunks = chunk_source("crlf.rs", src);
    common::assert_invariants(&ChunkConfig::default(), "crlf.rs", src, &chunks);
    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].text.contains("\r\n"));
    assert!(!chunks[0].text.ends_with('\r'));
    assert_eq!(chunks[0].defines, vec!["a", "b"]);
}

#[test]
fn giant_function_is_split_and_keeps_its_symbol() {
    let mut src =
        String::from("impl Big {\n    fn giant(&self) -> u32 {\n        let mut x = 0;\n");
    for i in 0..200 {
        writeln!(src, "        x += {i};").unwrap();
    }
    src.push_str("        x\n    }\n}\n");
    let cfg = ChunkConfig::default();
    let chunks = chunk_source_with(&cfg, "big.rs", &src);
    common::assert_invariants(&cfg, "big.rs", &src, &chunks);
    assert!(chunks.len() >= 5);
    for c in &chunks {
        assert_eq!(
            c.symbol, "impl Big > fn giant",
            "chunk {}-{}",
            c.start_line, c.end_line
        );
    }
    assert_eq!(
        chunks[0].start_line, 1,
        "the header stays with the first body chunk"
    );
    assert_eq!(chunks[0].defines, vec!["giant"]);
    assert!(chunks[1..].iter().all(|c| c.defines.is_empty()));
}

#[test]
fn indivisible_leaf_is_cut_at_max_lines() {
    let mut src = String::from("const S: &str = \"\n");
    for i in 0..118 {
        writeln!(src, "text {i}").unwrap();
    }
    src.push_str("\";\n");
    let cfg = ChunkConfig::default();
    let chunks = chunk_source_with(&cfg, "s.rs", &src);
    common::assert_invariants(&cfg, "s.rs", &src, &chunks);
    let spans: Vec<(u32, u32)> = chunks.iter().map(|c| (c.start_line, c.end_line)).collect();
    assert_eq!(spans, vec![(1, 50), (51, 100), (101, 120)]);
}

#[test]
fn minified_single_line_is_one_chunk_but_skipped_on_disk() {
    let mut src = String::new();
    for i in 0..5000 {
        write!(src, "var a{i}=function(){{return {i}}};").unwrap();
    }
    let chunks = chunk_source("bundle.js", &src);
    common::assert_invariants(&ChunkConfig::default(), "bundle.js", &src, &chunks);
    assert_eq!(chunks.len(), 1);

    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("bundle.js"), &src).unwrap();
    let err = parse_file(dir.path(), &dir.path().join("bundle.js")).unwrap_err();
    assert!(matches!(err, ParseError::Minified { .. }), "{err:?}");
}

#[test]
fn deeply_nested_code_does_not_overflow() {
    let depth = 3000;
    let mut src = String::from("function f() {\n");
    for _ in 0..depth {
        src.push_str("if (x) {\n");
    }
    src.push_str("y();\n");
    for _ in 0..depth {
        src.push_str("}\n");
    }
    src.push_str("}\n");
    let cfg = ChunkConfig::default();
    let chunks = chunk_source_with(&cfg, "deep.js", &src);
    common::assert_invariants(&cfg, "deep.js", &src, &chunks);
}

#[test]
fn syntax_errors_still_cover_everything() {
    let src = "fn broken( {\n  let x = ;\n}\n}}}\nstruct Ok;\n";
    let cfg = ChunkConfig::default();
    let chunks = chunk_source_with(&cfg, "broken.rs", src);
    common::assert_invariants(&cfg, "broken.rs", src, &chunks);
}

#[test]
fn parse_file_rejects_non_utf8_and_binary() {
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("latin1.rs");
    fs::write(&bad, b"fn caf\xe9() {}\n").unwrap();
    assert!(matches!(
        parse_file(dir.path(), &bad),
        Err(ParseError::NotUtf8 { .. })
    ));

    let bin = dir.path().join("blob.txt");
    fs::write(&bin, b"abc\0\0\0def").unwrap();
    assert!(matches!(
        parse_file(dir.path(), &bin),
        Err(ParseError::Binary { .. })
    ));

    let missing = dir.path().join("missing.rs");
    assert!(matches!(
        parse_file(dir.path(), &missing),
        Err(ParseError::Io { .. })
    ));

    let png = dir.path().join("x.png");
    fs::write(&png, b"\x89PNG").unwrap();
    assert!(matches!(
        parse_file(dir.path(), &png),
        Err(ParseError::Unsupported { .. })
    ));
}

#[test]
fn parse_file_handles_empty_files() {
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("empty.py");
    fs::write(&empty, b"").unwrap();
    let parsed = parse_file(dir.path(), &empty).unwrap();
    assert_eq!(parsed.path, "empty.py");
    assert_eq!(parsed.lang, Lang::Python);
    assert!(parsed.chunks.is_empty());
    assert_eq!(parsed.hash, laya_parse::file_hash(b""));
}

#[test]
fn chunk_files_is_parallel_and_order_preserving() {
    let dir = tempfile::tempdir().unwrap();
    let mut files = Vec::new();
    for i in 0..40 {
        let p = dir.path().join(format!("m{i}.py"));
        fs::write(&p, format!("def f{i}():\n    return {i}\n")).unwrap();
        files.push(p);
    }
    let out = chunk_files(dir.path(), &files);
    assert_eq!(out.len(), files.len());
    for (i, r) in out.iter().enumerate() {
        let f = r.as_ref().unwrap();
        assert_eq!(f.path, format!("m{i}.py"));
        assert_eq!(f.chunks[0].defines, vec![format!("f{i}")]);
        assert_eq!(f.chunks[0].path, f.path);
    }
}
