//! Integration tests for every `Store` method against a real, private moon.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{chunk, terms};
use laya_core::Store;
use laya_store::{MoonStore, StoreConfig};

const R: &str = "aaaaaaaaaaaa";

fn store(m: &common::TestMoon) -> MoonStore {
    let s = MoonStore::new(StoreConfig::local(m.port)).expect("store");
    s.ensure_index(R).expect("ensure_index");
    s
}

fn ids(v: &[(String, f32)]) -> Vec<String> {
    v.iter().map(|(i, _)| i.clone()).collect()
}

#[test]
fn ensure_index_is_idempotent() {
    let m = require_moon!();
    let s = store(&m);
    s.ensure_index(R).expect("second ensure_index");
}

#[test]
fn put_file_roundtrip_hash_list_and_get_chunks() {
    let m = require_moon!();
    let s = store(&m);
    let a = chunk(
        "src/lib.rs",
        1,
        "fn parse_config",
        &["parse_config"],
        "fn parse_config() -> Config {\n    load()\n}",
    );
    let b = chunk(
        "src/lib.rs",
        5,
        "struct Loader",
        &["Loader", "LoaderError"],
        "struct Loader { path: String }",
    );
    s.put_file(R, "src/lib.rs", "h1", &[a.clone(), b.clone()])
        .expect("put");

    assert_eq!(
        s.file_hash(R, "src/lib.rs").expect("hash"),
        Some("h1".to_string())
    );
    assert_eq!(s.file_hash(R, "src/none.rs").expect("hash"), None);
    assert_eq!(
        s.list_files(R).expect("list"),
        vec!["src/lib.rs".to_string()]
    );

    // Order follows the request; unknown ids are skipped.
    let got = s
        .get_chunks(R, &[b.id(), "missing".into(), a.id()])
        .expect("get");
    assert_eq!(got, vec![b, a]);
}

#[test]
fn bm25_has_or_semantics() {
    let m = require_moon!();
    let s = store(&m);
    let a = chunk("a.rs", 1, "", &[], "fn alpha_only() {}");
    let b = chunk("b.rs", 1, "", &[], "fn beta_only() {}");
    let c = chunk("c.rs", 1, "", &[], "fn unrelated() {}");
    s.put_file(R, "a.rs", "1", std::slice::from_ref(&a))
        .expect("put");
    s.put_file(R, "b.rs", "1", std::slice::from_ref(&b))
        .expect("put");
    s.put_file(R, "c.rs", "1", std::slice::from_ref(&c))
        .expect("put");

    let got = ids(&s.bm25(R, &terms(&["alpha", "beta"]), 10).expect("bm25"));
    assert!(
        got.contains(&a.id()),
        "doc matching only `alpha` must be returned: {got:?}"
    );
    assert!(
        got.contains(&b.id()),
        "doc matching only `beta` must be returned: {got:?}"
    );
    assert!(!got.contains(&c.id()));
}

#[test]
fn bm25_ranks_docs_matching_more_terms_first_and_sums_like_and() {
    let m = require_moon!();
    let s = store(&m);
    let both = chunk("both.rs", 1, "", &[], "fn gamma() { delta() }");
    let one = chunk("one.rs", 1, "", &[], "fn gamma() {}");
    let other = chunk("other.rs", 1, "", &[], "fn epsilon() {}");
    for c in [&both, &one, &other] {
        s.put_file(R, &c.path, "1", std::slice::from_ref(c))
            .expect("put");
    }
    let got = s.bm25(R, &terms(&["gamma", "delta"]), 10).expect("bm25");
    assert_eq!(got[0].0, both.id());

    // The per-term sum must equal Moon's own AND score for a doc containing all terms.
    let mut con = m.raw();
    let v: redis::Value = redis::cmd("FT.SEARCH")
        .arg(format!("lc:{R}:idx"))
        .arg("gamma delta")
        .arg("NOCONTENT")
        .query(&mut con)
        .expect("and query");
    let and = laya_store::query::parse_search_reply(&v).expect("parse");
    assert_eq!(and.len(), 1);
    assert!(
        (and[0].1 - got[0].1).abs() < 1e-3,
        "AND {} vs summed {}",
        and[0].1,
        got[0].1
    );
}

#[test]
fn bm25_ignores_stop_words_and_handles_empty_inputs() {
    let m = require_moon!();
    let s = store(&m);
    let a = chunk("a.rs", 1, "", &[], "fn zeta() {}");
    s.put_file(R, "a.rs", "1", std::slice::from_ref(&a))
        .expect("put");

    assert!(
        s.bm25(R, &terms(&["the", "and", "of"]), 10)
            .expect("stop-only")
            .is_empty()
    );
    assert_eq!(
        ids(&s.bm25(R, &terms(&["the", "zeta"]), 10).expect("mixed")),
        vec![a.id()]
    );
    assert!(s.bm25(R, &[], 10).expect("empty").is_empty());
    assert!(s.bm25(R, &terms(&["zeta"]), 0).expect("limit 0").is_empty());
    assert!(
        s.bm25(R, &terms(&["@x", "a|b", "-y"]), 10)
            .expect("syntax chars")
            .is_empty()
    );
}

#[test]
fn bm25_respects_limit_and_orders_by_score() {
    let m = require_moon!();
    let s = store(&m);
    for i in 0..30u32 {
        let reps = "token ".repeat(i as usize + 1);
        let c = chunk(
            &format!("f{i}.rs"),
            1,
            "",
            &[],
            &format!("{reps} padding words here"),
        );
        s.put_file(R, &c.path, "1", std::slice::from_ref(&c))
            .expect("put");
    }
    let got = s.bm25(R, &terms(&["token"]), 5).expect("bm25");
    assert_eq!(got.len(), 5);
    assert!(got.windows(2).all(|w| w[0].1 >= w[1].1));
}

#[test]
fn bm25_without_index_returns_empty() {
    let m = require_moon!();
    let s = MoonStore::new(StoreConfig::local(m.port)).expect("store");
    assert!(
        s.bm25("bbbbbbbbbbbb", &terms(&["anything"]), 10)
            .expect("no index")
            .is_empty()
    );
}

#[test]
fn reput_replaces_chunks() {
    let m = require_moon!();
    let s = store(&m);
    let v1 = chunk(
        "x.rs",
        1,
        "fn old_name",
        &["OldName"],
        "fn old_name() { vintage_marker() }",
    );
    s.put_file(R, "x.rs", "h1", std::slice::from_ref(&v1))
        .expect("put v1");
    assert_eq!(
        ids(&s.bm25(R, &terms(&["vintage_marker"]), 10).expect("bm25")),
        vec![v1.id()]
    );

    let v2 = chunk(
        "x.rs",
        1,
        "fn new_name",
        &["NewName"],
        "fn new_name() { modern_marker() }",
    );
    s.put_file(R, "x.rs", "h2", std::slice::from_ref(&v2))
        .expect("put v2");

    assert!(
        s.bm25(R, &terms(&["vintage_marker"]), 10)
            .expect("bm25")
            .is_empty(),
        "old chunk must be deindexed"
    );
    assert_eq!(
        ids(&s.bm25(R, &terms(&["modern_marker"]), 10).expect("bm25")),
        vec![v2.id()]
    );
    assert!(s.get_chunks(R, &[v1.id()]).expect("get").is_empty());
    assert!(
        s.chunks_defining(R, &terms(&["OldName"]), 10)
            .expect("defs")
            .is_empty()
    );
    assert_eq!(
        s.chunks_defining(R, &terms(&["NewName"]), 10)
            .expect("defs"),
        vec![v2.id()]
    );
    assert_eq!(s.file_hash(R, "x.rs").expect("hash").as_deref(), Some("h2"));

    // Re-putting identical content is a no-op in effect.
    s.put_file(R, "x.rs", "h2", std::slice::from_ref(&v2))
        .expect("put v2 again");
    assert_eq!(
        ids(&s.bm25(R, &terms(&["modern_marker"]), 10).expect("bm25")),
        vec![v2.id()]
    );
}

#[test]
fn chunks_of_file_returns_the_files_current_chunks_in_line_order() {
    let m = require_moon!();
    let s = store(&m);
    let late = chunk("big.rs", 40, "fn late", &["late"], "fn late() {}");
    let early = chunk("big.rs", 1, "fn early", &["early"], "fn early() {}");
    let other = chunk("other.rs", 1, "fn other", &["other"], "fn other() {}");
    s.put_file(R, "big.rs", "h1", &[late.clone(), early.clone()])
        .expect("put");
    s.put_file(R, "other.rs", "h", std::slice::from_ref(&other))
        .expect("put other");
    assert_eq!(
        s.chunks_of_file(R, "big.rs").expect("chunks"),
        vec![early.clone(), late]
    );
    // A re-put replaces the list; unknown and deleted files have no chunks.
    s.put_file(R, "big.rs", "h2", std::slice::from_ref(&early))
        .expect("reput");
    assert_eq!(s.chunks_of_file(R, "big.rs").expect("chunks"), vec![early]);
    assert!(s.chunks_of_file(R, "none.rs").expect("none").is_empty());
    s.delete_file(R, "big.rs").expect("delete");
    assert!(s.chunks_of_file(R, "big.rs").expect("deleted").is_empty());
}

#[test]
fn delete_file_removes_everything() {
    let m = require_moon!();
    let s = store(&m);
    let a = chunk("gone.rs", 1, "", &["Gone"], "fn ephemeral_thing() {}");
    let keep = chunk("keep.rs", 1, "", &["Keep"], "fn ephemeral_keep() {}");
    s.put_file(R, "gone.rs", "h", std::slice::from_ref(&a))
        .expect("put");
    s.put_file(R, "keep.rs", "h", std::slice::from_ref(&keep))
        .expect("put");
    s.delete_file(R, "gone.rs").expect("delete");
    s.delete_file(R, "never-indexed.rs")
        .expect("delete unknown is ok");

    assert_eq!(s.file_hash(R, "gone.rs").expect("hash"), None);
    assert_eq!(s.list_files(R).expect("list"), vec!["keep.rs".to_string()]);
    assert!(s.get_chunks(R, &[a.id()]).expect("get").is_empty());
    assert!(
        s.chunks_defining(R, &terms(&["Gone"]), 10)
            .expect("defs")
            .is_empty()
    );
    assert_eq!(
        ids(&s.bm25(R, &terms(&["ephemeral"]), 10).expect("bm25")),
        vec![keep.id()]
    );
}

#[test]
fn chunks_defining_is_exact_case_sensitive_union() {
    let m = require_moon!();
    let s = store(&m);
    let a = chunk(
        "a.rs",
        1,
        "",
        &["Foo", "foo_bar"],
        "struct Foo; fn foo_bar() {}",
    );
    let b = chunk("b.rs", 1, "", &["Bar"], "struct Bar;");
    let c = chunk("c.rs", 1, "", &["operator+", "a.b", "has space"], "weird");
    for x in [&a, &b, &c] {
        s.put_file(R, &x.path, "1", std::slice::from_ref(x))
            .expect("put");
    }
    assert_eq!(
        s.chunks_defining(R, &terms(&["Foo"]), 10).expect("defs"),
        vec![a.id()]
    );
    assert!(
        s.chunks_defining(R, &terms(&["foo"]), 10)
            .expect("defs")
            .is_empty(),
        "case-sensitive"
    );
    let mut both = s
        .chunks_defining(R, &terms(&["Foo", "Bar", "Foo"]), 10)
        .expect("defs");
    both.sort();
    let mut want = vec![a.id(), b.id()];
    want.sort();
    assert_eq!(both, want);
    assert_eq!(
        s.chunks_defining(R, &terms(&["Foo", "Bar"]), 1)
            .expect("defs")
            .len(),
        1
    );
    assert_eq!(
        s.chunks_defining(R, &terms(&["operator+"]), 10)
            .expect("defs"),
        vec![c.id()]
    );
    assert_eq!(
        s.chunks_defining(R, &terms(&["has space"]), 10)
            .expect("defs"),
        vec![c.id()]
    );
    assert!(s.chunks_defining(R, &[], 10).expect("defs").is_empty());
}

#[test]
fn chunks_defining_ranks_chunks_matching_more_idents_first() {
    let m = require_moon!();
    let s = store(&m);
    let one = chunk("one.rs", 1, "", &["Alpha"], "a");
    let two = chunk("two.rs", 1, "", &["Alpha", "Beta"], "b");
    s.put_file(R, "one.rs", "1", &[one]).expect("put");
    s.put_file(R, "two.rs", "1", std::slice::from_ref(&two))
        .expect("put");
    let got = s
        .chunks_defining(R, &terms(&["Alpha", "Beta"]), 10)
        .expect("defs");
    assert_eq!(got[0], two.id());
}

#[test]
fn special_characters_in_paths_roundtrip() {
    let m = require_moon!();
    let s = store(&m);
    let path = "src/we ird/a,b:{x}|y*.rs";
    let a = chunk(path, 3, "fn odd", &["odd"], "fn odd_path_fn() {}");
    s.put_file(R, path, "h", std::slice::from_ref(&a))
        .expect("put");
    assert_eq!(s.list_files(R).expect("list"), vec![path.to_string()]);
    assert_eq!(s.get_chunks(R, &[a.id()]).expect("get"), vec![a.clone()]);
    assert_eq!(
        ids(&s.bm25(R, &terms(&["odd_path_fn"]), 5).expect("bm25")),
        vec![a.id()]
    );
    s.delete_file(R, path).expect("delete");
    assert!(s.list_files(R).expect("list").is_empty());
}

#[test]
fn path_and_symbol_terms_are_searchable() {
    let m = require_moon!();
    let s = store(&m);
    let a = chunk(
        "src/storage/wal_writer.rs",
        1,
        "impl WalWriter::flush",
        &[],
        "x = 1",
    );
    s.put_file(R, &a.path, "h", std::slice::from_ref(&a))
        .expect("put");
    assert_eq!(
        ids(&s.bm25(R, &terms(&["wal_writer"]), 5).expect("path term")),
        vec![a.id()]
    );
    assert_eq!(
        ids(&s.bm25(R, &terms(&["walwriter"]), 5).expect("symbol term")),
        vec![a.id()]
    );
}

#[test]
fn repos_are_isolated() {
    let m = require_moon!();
    let s = store(&m);
    let other = "cccccccccccc";
    s.ensure_index(other).expect("ensure");
    let a = chunk("same.rs", 1, "", &["Same"], "fn isolation_probe() {}");
    s.put_file(R, "same.rs", "h", std::slice::from_ref(&a))
        .expect("put");
    assert!(
        s.bm25(other, &terms(&["isolation_probe"]), 5)
            .expect("bm25")
            .is_empty()
    );
    assert!(s.list_files(other).expect("list").is_empty());
    assert!(
        s.chunks_defining(other, &terms(&["Same"]), 5)
            .expect("defs")
            .is_empty()
    );
}

#[test]
fn empty_file_is_recorded_without_chunks() {
    let m = require_moon!();
    let s = store(&m);
    s.put_file(R, "empty.rs", "h0", &[]).expect("put");
    assert_eq!(
        s.file_hash(R, "empty.rs").expect("hash").as_deref(),
        Some("h0")
    );
    assert_eq!(s.list_files(R).expect("list"), vec!["empty.rs".to_string()]);
}

#[test]
fn huge_file_is_indexed_in_one_call() {
    let m = require_moon!();
    let s = store(&m);
    let chunks: Vec<_> = (0..2000u32)
        .map(|i| {
            chunk(
                "big.rs",
                i * 10 + 1,
                &format!("fn f{i}"),
                &[&format!("F{i}")],
                &format!("fn item_{i}() {{ common_body() }}"),
            )
        })
        .collect();
    s.put_file(R, "big.rs", "h", &chunks).expect("put");
    assert_eq!(
        s.bm25(R, &terms(&["common_body"]), 50).expect("bm25").len(),
        50
    );
    assert_eq!(
        s.chunks_defining(R, &terms(&["F1999"]), 5).expect("defs"),
        vec![chunks[1999].id()]
    );
    // Replacing a huge file with a small one removes all old chunks.
    let small = chunk("big.rs", 1, "", &[], "fn tiny() {}");
    s.put_file(R, "big.rs", "h2", &[small]).expect("put small");
    assert!(
        s.bm25(R, &terms(&["common_body"]), 50)
            .expect("bm25")
            .is_empty()
    );
}

#[test]
fn concurrent_indexing_from_many_threads() {
    let m = require_moon!();
    let s = Arc::new(store(&m));
    let handles: Vec<_> = (0..8)
        .map(|t| {
            let s = Arc::clone(&s);
            std::thread::spawn(move || {
                for f in 0..20 {
                    let path = format!("t{t}/f{f}.rs");
                    let c = chunk(&path, 1, "", &[], "fn concurrent_marker() {}");
                    s.put_file(R, &path, "h", &[c]).expect("put");
                    s.bm25(R, &terms(&["concurrent_marker"]), 5).expect("bm25");
                }
            })
        })
        .collect();
    for h in handles {
        h.join().expect("thread");
    }
    assert_eq!(s.list_files(R).expect("list").len(), 160);
    assert_eq!(
        s.bm25(R, &terms(&["concurrent_marker"]), 500)
            .expect("bm25")
            .len(),
        160
    );
}

#[test]
fn memo_put_get_and_ttl() {
    let m = require_moon!();
    let s = store(&m);
    assert_eq!(s.memo_get("k").expect("get"), None);
    s.memo_put("k", "v1", 0).expect("put");
    assert_eq!(s.memo_get("k").expect("get").as_deref(), Some("v1"));
    s.memo_put("short", "v2", 1).expect("put ttl");
    assert_eq!(s.memo_get("short").expect("get").as_deref(), Some("v2"));
    std::thread::sleep(Duration::from_millis(1300));
    assert_eq!(s.memo_get("short").expect("get"), None, "expired");
    assert_eq!(
        s.memo_get("k").expect("get").as_deref(),
        Some("v1"),
        "no-ttl survives"
    );
}

#[test]
fn index_and_data_survive_moon_restart() {
    let mut m = require_moon!();
    let s = store(&m);
    let a = chunk("p.rs", 1, "", &["Persist"], "fn persisted_marker() {}");
    s.put_file(R, "p.rs", "h", std::slice::from_ref(&a))
        .expect("put");
    // appendfsync everysec: give the AOF a moment before the crash.
    std::thread::sleep(Duration::from_millis(1200));
    m.restart();
    s.ensure_index(R).expect("ensure after restart");
    assert_eq!(
        ids(&s.bm25(R, &terms(&["persisted_marker"]), 5).expect("bm25")),
        vec![a.id()]
    );
    assert_eq!(
        s.chunks_defining(R, &terms(&["Persist"]), 5).expect("defs"),
        vec![a.id()]
    );
    assert_eq!(s.get_chunks(R, &[a.id()]).expect("get"), vec![a]);
}

fn df(m: &common::TestMoon, term: &str) -> i64 {
    let mut con = m.raw();
    let v: Option<i64> = redis::cmd("HGET")
        .arg(format!("lc:{R}:df"))
        .arg(term)
        .query(&mut con)
        .expect("hget df");
    v.unwrap_or(0)
}

#[test]
fn document_frequencies_track_puts_replacements_and_deletes() {
    let m = require_moon!();
    let s = store(&m);
    let a = chunk(
        "d.rs",
        1,
        "",
        &[],
        "fn shared_tok() { only_a_tok(); only_a_tok() }",
    );
    let b = chunk("d.rs", 5, "", &[], "fn shared_tok() {}");
    s.put_file(R, "d.rs", "h1", &[a.clone(), b.clone()])
        .expect("put");
    assert_eq!(df(&m, "shared_tok"), 2, "df counts chunks, not occurrences");
    assert_eq!(df(&m, "only_a_tok"), 1);

    // Same content again: unchanged chunks must not be double counted.
    s.put_file(R, "d.rs", "h1", &[a.clone(), b.clone()])
        .expect("re-put");
    assert_eq!(df(&m, "shared_tok"), 2);

    let a2 = chunk("d.rs", 1, "", &[], "fn replaced_tok() {}");
    s.put_file(R, "d.rs", "h2", &[a2, b]).expect("replace a");
    assert_eq!(df(&m, "shared_tok"), 1);
    assert_eq!(df(&m, "only_a_tok"), 0);
    assert_eq!(df(&m, "replaced_tok"), 1);

    s.delete_file(R, "d.rs").expect("delete");
    assert_eq!(df(&m, "shared_tok"), 0);
    assert_eq!(df(&m, "replaced_tok"), 0);
}

#[test]
fn bm25_skips_frequent_terms_beyond_the_cost_budget_and_prefers_rarest() {
    let m = require_moon!();
    let cfg = StoreConfig {
        df_sq_budget: 10,
        ..StoreConfig::local(m.port)
    };
    let s = MoonStore::new(cfg).expect("store");
    s.ensure_index(R).expect("index");
    for i in 0..5 {
        let c = chunk(&format!("c{i}.rs"), 1, "", &[], "fn everywhere_tok() {}");
        s.put_file(R, &c.path, "h", std::slice::from_ref(&c))
            .expect("put");
    }
    let needle = chunk("n.rs", 1, "", &[], "fn needle_tok() {}");
    s.put_file(R, "n.rs", "h", std::slice::from_ref(&needle))
        .expect("put");
    let q = terms(&["everywhere_tok", "needle_tok"]);

    // needle: df 1 (cost 1) fits; everywhere: df 5 (cost 25) would exceed the budget of 10.
    assert_eq!(ids(&s.bm25(R, &q, 10).expect("bm25")), vec![needle.id()]);

    // With the default budget both terms are searched.
    assert_eq!(store(&m).bm25(R, &q, 10).expect("bm25").len(), 6);

    // max_terms = 1 keeps the rarest term.
    let one = MoonStore::new(StoreConfig {
        max_terms: 1,
        ..StoreConfig::local(m.port)
    })
    .expect("store");
    assert_eq!(ids(&one.bm25(R, &q, 10).expect("bm25")), vec![needle.id()]);

    // Terms never indexed cost no search and do not break the query.
    assert!(
        s.bm25(R, &terms(&["never_seen_tok"]), 10)
            .expect("bm25")
            .is_empty()
    );
}
