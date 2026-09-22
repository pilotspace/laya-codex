//! Language detection, repo walking and hashing.

use std::fs;
use std::path::Path;

use laya_core::Lang;
use laya_parse::{detect_lang, file_hash, walk_repo};

#[test]
fn code_extensions_map_to_grammars() {
    let cases = [
        ("src/main.rs", Lang::Rust),
        ("a/b.py", Lang::Python),
        ("types.pyi", Lang::Python),
        ("x.ts", Lang::TypeScript),
        ("x.mts", Lang::TypeScript),
        ("x.tsx", Lang::Tsx),
        ("x.js", Lang::JavaScript),
        ("x.jsx", Lang::JavaScript),
        ("x.mjs", Lang::JavaScript),
        ("x.cjs", Lang::JavaScript),
        ("main.go", Lang::Go),
        ("Main.java", Lang::Java),
        ("x.c", Lang::C),
        ("x.h", Lang::C),
        ("x.cc", Lang::Cpp),
        ("x.cpp", Lang::Cpp),
        ("x.hpp", Lang::Cpp),
        ("x.cs", Lang::CSharp),
        ("x.rb", Lang::Ruby),
        ("Rakefile", Lang::Ruby),
        ("x.php", Lang::Php),
        ("x.kt", Lang::Kotlin),
        ("build.gradle.kts", Lang::Kotlin),
        ("x.swift", Lang::Swift),
        ("SRC/MAIN.RS", Lang::Rust),
    ];
    for (p, want) in cases {
        assert_eq!(detect_lang(p), Some(want), "{p}");
    }
}

#[test]
fn text_extensions_use_line_windows() {
    for p in [
        "README.md",
        "Cargo.toml",
        "ci.yaml",
        "ci.yml",
        "pkg.json",
        "run.sh",
        "q.sql",
        "Makefile",
        "Dockerfile",
        "notes.txt",
        "a.proto",
        "index.html",
        "style.css",
    ] {
        assert_eq!(detect_lang(p), Some(Lang::Text), "{p}");
    }
}

#[test]
fn skipped_paths() {
    for p in [
        "logo.png",
        "blob.bin",
        "font.woff2",
        "Cargo.lock",
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "go.sum",
        "poetry.lock",
        "app.min.js",
        "styles.min.css",
        "target/debug/build.rs",
        "node_modules/x/index.js",
        "web/dist/app.js",
        "build/gen.py",
        ".git/config",
        "vendor/github.com/x/y.go",
        "a/__pycache__/m.py",
        "no_extension_blob",
        ".env",
        "prod.env",
        "node_modules\\x\\index.js",
    ] {
        assert_eq!(detect_lang(p), None, "{p}");
    }
}

#[test]
fn backslash_paths_are_normalized() {
    assert_eq!(detect_lang("src\\lib.rs"), Some(Lang::Rust));
}

#[test]
fn files_over_one_mib_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("big.rs");
    fs::write(&big, vec![b'a'; 1024 * 1024 + 1]).unwrap();
    let small = dir.path().join("small.rs");
    fs::write(&small, "fn main() {}\n").unwrap();
    assert_eq!(detect_lang(big.to_str().unwrap()), None);
    assert_eq!(detect_lang(small.to_str().unwrap()), Some(Lang::Rust));
}

fn write(root: &Path, rel: &str, body: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

#[test]
fn walk_respects_gitignore_hidden_and_generated_dirs() {
    // tempfile's default `.tmpXXXX` name is itself hidden; use a visible prefix for the root.
    let dir = tempfile::Builder::new()
        .prefix("laya-walk")
        .tempdir()
        .unwrap();
    let root = dir.path();
    write(root, ".gitignore", b"ignored/\n*.log\n");
    write(root, "src/a.rs", b"fn a() {}\n");
    write(root, "src/nested/b.py", b"def b(): pass\n");
    write(root, "README.md", b"# hi\n");
    write(root, "ignored/c.rs", b"fn c() {}\n");
    write(root, "debug.log", b"log\n");
    write(root, ".hidden/d.rs", b"fn d() {}\n");
    write(root, "node_modules/e/index.js", b"x\n");
    write(root, "target/f.rs", b"fn f() {}\n");
    write(root, "dist/g.js", b"x\n");
    write(root, "img.png", b"\x89PNG");
    write(root, "Cargo.lock", b"x\n");

    let got: Vec<String> = walk_repo(root)
        .into_iter()
        .map(|p| {
            p.strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    assert_eq!(got, vec!["README.md", "src/a.rs", "src/nested/b.py"]);
}

#[test]
fn walk_of_missing_root_is_empty() {
    assert!(walk_repo(Path::new("/definitely/not/a/real/dir")).is_empty());
}

#[test]
fn file_hash_is_32_hex_and_content_addressed() {
    let a = file_hash(b"hello");
    assert_eq!(a.len(), 32);
    assert!(
        a.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
    assert_eq!(a, file_hash(b"hello"));
    assert_ne!(a, file_hash(b"hello!"));
    assert_eq!(a, blake3::hash(b"hello").to_hex()[..32].to_string());
}
