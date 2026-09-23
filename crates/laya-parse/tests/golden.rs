//! Golden tests: one small fixture per language, rendered under the default config and a
//! small config that forces splitting. Regenerate with `UPDATE_GOLDEN=1 cargo test -p laya-parse
//! --test golden` and review the diff by hand before committing.

mod common;

use std::path::PathBuf;

use laya_parse::{ChunkConfig, chunk_source_with};

const FIXTURES: &[&str] = &[
    "sample.rs",
    "sample.py",
    "sample.ts",
    "sample.tsx",
    "sample.js",
    "sample.go",
    "Sample.java",
    "sample.c",
    "sample.cpp",
    "Sample.cs",
    "sample.rb",
    "sample.php",
    "Sample.kt",
    "sample.swift",
    "sample.md",
];

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn render(name: &str, src: &str) -> String {
    let small = ChunkConfig {
        min_lines: 4,
        max_lines: 12,
        ..ChunkConfig::default()
    };
    let mut out = String::new();
    for cfg in [ChunkConfig::default(), small] {
        out.push_str(&format!("== min={} max={}\n", cfg.min_lines, cfg.max_lines));
        let chunks = chunk_source_with(&cfg, name, src);
        common::assert_invariants(&cfg, name, src, &chunks);
        for c in &chunks {
            let line = format!(
                "L{}-{} | {} | {} | {}",
                c.start_line,
                c.end_line,
                c.kind,
                c.symbol,
                c.defines.join(",")
            );
            out.push_str(line.trim_end());
            out.push('\n');
        }
    }
    out
}

#[test]
fn golden_per_language() {
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    let mut failures = Vec::new();
    for name in FIXTURES {
        let path = fixture_dir().join(name);
        let src = std::fs::read_to_string(&path).expect("fixture readable");
        let got = render(name, &src);
        let golden = fixture_dir().join(format!("{name}.golden"));
        if update {
            std::fs::write(&golden, &got).expect("write golden");
            continue;
        }
        let want = std::fs::read_to_string(&golden).unwrap_or_default();
        if got != want {
            failures.push(format!("--- {name}\nwant:\n{want}\ngot:\n{got}"));
        }
    }
    assert!(
        failures.is_empty(),
        "golden mismatches:\n{}",
        failures.join("\n")
    );
}

#[test]
fn every_fixture_uses_a_grammar() {
    use laya_core::Lang;
    for name in FIXTURES {
        let lang = laya_parse::detect_lang(name).expect("fixture language detected");
        if name.ends_with(".md") {
            assert_eq!(lang, Lang::Text);
        } else {
            assert_ne!(lang, Lang::Text, "{name} fell back to text");
        }
    }
}

#[test]
fn chunk_lang_and_path_are_set() {
    let src = std::fs::read_to_string(fixture_dir().join("sample.rs")).unwrap();
    let chunks = laya_parse::chunk_source("src\\sample.rs", &src);
    assert!(!chunks.is_empty());
    for c in &chunks {
        assert_eq!(c.lang, laya_core::Lang::Rust);
        assert_eq!(c.path, "src/sample.rs");
    }
}
