//! `Chunk.refs`: callees, used types and imported names, minus own defines, stoplist and
//! short names; first-occurrence order, deduplicated, capped at `MAX_REFS`.

use std::fmt::Write as _;
use std::path::PathBuf;

use laya_parse::{MAX_REFS, REF_STOPLIST, chunk_source};

fn fixture(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(p).expect("fixture")
}

/// Refs of a fixture that fits in one chunk under the default config.
fn single_chunk_refs(name: &str) -> (Vec<String>, Vec<String>) {
    let chunks = chunk_source(name, &fixture(name));
    assert_eq!(chunks.len(), 1, "{name} should be a single chunk");
    (chunks[0].refs.clone(), chunks[0].defines.clone())
}

#[test]
fn rust_refs() {
    let (refs, defines) = single_chunk_refs("refs_sample.rs");
    assert_eq!(defines, vec!["Cache", "build", "helper"]);
    // Imports (use-list leaves), used types, callees; own defines (Cache, helper), stoplist
    // (String, get, unwrap, len, println, new) and paths (std, collections) are excluded.
    assert_eq!(
        refs,
        vec![
            "BTreeSet",
            "HashMap",
            "MoonStore",
            "Entry",
            "Config",
            "open",
            "compute_total"
        ]
    );
}

#[test]
fn python_refs() {
    let (refs, defines) = single_chunk_refs("refs_sample.py");
    assert_eq!(defines, vec!["Cache", "get", "helper"]);
    // `open_store as opener` references open_store; str/print/len are stoplisted.
    assert_eq!(
        refs,
        vec![
            "path",
            "Optional",
            "MoonStore",
            "open_store",
            "BaseCache",
            "Entry",
            "fetch_value",
            "transform"
        ]
    );
}

#[test]
fn typescript_refs() {
    let (refs, defines) = single_chunk_refs("refs_sample.ts");
    assert_eq!(defines, vec!["Cache", "lookup", "build"]);
    // `Entry as E` references Entry; console/log are stoplisted; `new Cache` is own define.
    assert_eq!(
        refs,
        vec![
            "readFile",
            "Store",
            "Entry",
            "openStore",
            "BaseCache",
            "Lookup",
            "fetchRaw",
            "decode",
            "createStore"
        ]
    );
}

#[test]
fn go_refs() {
    let (refs, defines) = single_chunk_refs("refs_sample.go");
    assert_eq!(defines, vec!["Cache", "Lookup"]);
    // Import paths contribute their last segment; string/error/Errorf are stoplisted.
    assert_eq!(
        refs,
        vec![
            "fmt", "store", "Store", "Metadata", "Entry", "FetchRaw", "decode"
        ]
    );
}

#[test]
fn java_refs() {
    let (refs, defines) = single_chunk_refs("RefsSample.java");
    assert_eq!(defines, vec!["Cache", "lookup"]);
    // `new MoonStore()` dedups with the import; String/println/get are stoplisted.
    assert_eq!(
        refs,
        vec![
            "List",
            "MoonStore",
            "BaseCache",
            "Entry",
            "fetchAll",
            "decode"
        ]
    );
}

#[test]
fn php_refs() {
    let src = "<?php\nuse App\\Store\\KeyStore;\n$x = new Widget();\nbar_baz();\n$y->doThing();\nUtil::runAll();\n";
    let chunks = chunk_source("x.php", src);
    assert_eq!(
        chunks[0].refs,
        vec!["KeyStore", "Widget", "bar_baz", "doThing", "runAll"]
    );
}

#[test]
fn other_languages_extract_callees_and_types() {
    let cases = [
        ("sample.js", "readFileSync"),
        ("sample.tsx", "setState"),
        ("sample.c", "calloc"),
        ("Sample.cs", "TryGetValue"),
        ("sample.rb", "File"),
        ("Sample.kt", "sqrt"),
        ("sample.swift", "squareRoot"),
        ("sample.cpp", "static_cast"),
    ];
    for (name, want) in cases {
        let chunks = chunk_source(name, &fixture(name));
        let all: Vec<&String> = chunks.iter().flat_map(|c| &c.refs).collect();
        assert!(
            all.iter().any(|r| *r == want),
            "{name}: {want} missing from {all:?}"
        );
    }
}

#[test]
fn refs_are_capped_and_scoped_to_the_chunk() {
    let mut src = String::from("fn caller() {\n");
    for i in 0..45 {
        writeln!(src, "    callee_{i:03}(); other_{i:03}();").unwrap();
    }
    src.push_str("}\n");
    let chunks = chunk_source("cap.rs", &src);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].refs.len(), MAX_REFS);
    assert_eq!(chunks[0].refs[0], "callee_000");
    assert_eq!(chunks[0].refs[1], "other_000");

    // Two chunks: each only lists what its own lines reference.
    let mut two = String::new();
    for f in 0..2 {
        writeln!(two, "fn func_{f}() {{").unwrap();
        for i in 0..40 {
            writeln!(two, "    target_{f}_{i}();").unwrap();
        }
        two.push_str("}\n\n");
    }
    let chunks = chunk_source("two.rs", &two);
    assert_eq!(chunks.len(), 2);
    assert!(chunks[0].refs.iter().all(|r| r.starts_with("target_0_")));
    assert!(chunks[1].refs.iter().all(|r| r.starts_with("target_1_")));
}

#[test]
fn tsx_heritage_and_jsx_components() {
    let src = "class Panel extends React.Component {\n  render() {\n    return <div><UserCard id={1} /><Layout.Header /></div>;\n  }\n}\n";
    let chunks = chunk_source("panel.tsx", src);
    // `div` is an HTML tag, not a component reference.
    assert_eq!(chunks[0].refs, vec!["Component", "UserCard", "Header"]);
}

#[test]
fn text_chunks_have_no_refs() {
    let chunks = chunk_source("notes.md", "# Title\n\ncall_something() here\n");
    assert!(chunks[0].refs.is_empty());
}

#[test]
fn stoplist_is_a_single_tunable_const() {
    for w in [
        "new", "unwrap", "len", "console", "useState", "self", "this",
    ] {
        assert!(REF_STOPLIST.contains(&w), "{w}");
    }
}
