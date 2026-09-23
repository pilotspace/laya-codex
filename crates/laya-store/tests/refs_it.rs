//! Reference index: `Chunk.refs` persistence, `chunks_referencing`, `definition_counts`.

mod common;

use common::{chunk_refs, terms};
use laya_core::Store;
use laya_store::{MoonStore, StoreConfig};

const R: &str = "ffffffffffff";

fn store(m: &common::TestMoon) -> MoonStore {
    let s = MoonStore::new(StoreConfig::local(m.port)).expect("store");
    s.ensure_index(R).expect("ensure_index");
    s
}

fn set_members(m: &common::TestMoon, key: &str) -> Vec<String> {
    let mut con = m.raw();
    let mut v: Vec<String> = redis::cmd("SMEMBERS")
        .arg(key)
        .query(&mut con)
        .expect("smembers");
    v.sort();
    v
}

#[test]
fn refs_round_trip_and_legacy_chunks_read_as_empty() {
    let m = require_moon!();
    let s = store(&m);
    let a = chunk_refs(
        "a.rs",
        1,
        &["run"],
        &["WalWriter", "flush", "operator+"],
        "fn run() {}",
    );
    s.put_file(R, "a.rs", "h", std::slice::from_ref(&a))
        .expect("put");
    assert_eq!(s.get_chunks(R, &[a.id()]).expect("get"), vec![a]);

    // A chunk hash written before refs existed has no `refs` field.
    let mut con = m.raw();
    let _: () = redis::cmd("HSET")
        .arg(format!("lc:{R}:c:legacy"))
        .arg(&["path", "old.rs", "start", "1", "end", "2", "lang", "rust"])
        .arg(&[
            "symbol", "", "kind", "k", "defines", "", "terms", "old", "text", "x",
        ])
        .query(&mut con)
        .expect("hset legacy");
    let got = s
        .get_chunks(R, &["legacy".to_string()])
        .expect("get legacy");
    assert_eq!(got.len(), 1);
    assert!(got[0].refs.is_empty());
}

#[test]
fn chunks_referencing_orders_by_match_count_then_id() {
    let m = require_moon!();
    let s = store(&m);
    let both = chunk_refs("both.rs", 1, &[], &["Alpha", "Beta"], "a b");
    let only_a = chunk_refs("a.rs", 1, &[], &["Alpha"], "a");
    let only_b = chunk_refs("b.rs", 1, &[], &["Beta", "Gamma"], "b");
    let none = chunk_refs("n.rs", 1, &["Alpha"], &["Other"], "n");
    for c in [&both, &only_a, &only_b, &none] {
        s.put_file(R, &c.path, "h", std::slice::from_ref(c))
            .expect("put");
    }
    let got = s
        .chunks_referencing(R, &terms(&["Alpha", "Beta", "Alpha"]), 10)
        .expect("refs");
    let mut singles = [only_a.id(), only_b.id()];
    singles.sort();
    assert_eq!(got, vec![both.id(), singles[0].clone(), singles[1].clone()]);

    assert_eq!(
        s.chunks_referencing(R, &terms(&["Alpha", "Beta"]), 1)
            .expect("limit"),
        vec![both.id()]
    );
    assert!(
        s.chunks_referencing(R, &terms(&["alpha"]), 10)
            .expect("case")
            .is_empty(),
        "case-sensitive"
    );
    assert!(s.chunks_referencing(R, &[], 10).expect("empty").is_empty());
    assert!(
        s.chunks_referencing(R, &terms(&["Alpha"]), 0)
            .expect("limit 0")
            .is_empty()
    );
    // Defining an ident is not referencing it.
    assert!(
        !s.chunks_referencing(R, &terms(&["Alpha"]), 10)
            .expect("refs")
            .contains(&none.id())
    );
}

#[test]
fn reference_sets_follow_reput_and_delete() {
    let m = require_moon!();
    let s = store(&m);
    let v1 = chunk_refs("x.rs", 1, &[], &["OldCallee", "Shared"], "fn v1() {}");
    let keep = chunk_refs("x.rs", 9, &[], &["Shared"], "fn keep() {}");
    s.put_file(R, "x.rs", "h1", &[v1.clone(), keep.clone()])
        .expect("put");
    assert_eq!(
        set_members(&m, &format!("lc:{R}:r:OldCallee")),
        vec![v1.id()]
    );

    let v2 = chunk_refs("x.rs", 1, &[], &["NewCallee"], "fn v2() {}");
    s.put_file(R, "x.rs", "h2", &[v2.clone(), keep.clone()])
        .expect("re-put");
    assert!(set_members(&m, &format!("lc:{R}:r:OldCallee")).is_empty());
    assert_eq!(
        set_members(&m, &format!("lc:{R}:r:Shared")),
        vec![keep.id()]
    );
    assert_eq!(
        s.chunks_referencing(R, &terms(&["NewCallee"]), 5)
            .expect("refs"),
        vec![v2.id()]
    );

    // Unchanged content re-put keeps memberships intact.
    s.put_file(R, "x.rs", "h2", &[v2.clone(), keep.clone()])
        .expect("same re-put");
    assert_eq!(
        set_members(&m, &format!("lc:{R}:r:Shared")),
        vec![keep.id()]
    );

    s.delete_file(R, "x.rs").expect("delete");
    assert!(set_members(&m, &format!("lc:{R}:r:Shared")).is_empty());
    assert!(set_members(&m, &format!("lc:{R}:r:NewCallee")).is_empty());
    assert!(
        s.chunks_referencing(R, &terms(&["Shared", "NewCallee"]), 5)
            .expect("refs")
            .is_empty()
    );
}

#[test]
fn definition_counts_flag_ambiguous_identifiers() {
    let m = require_moon!();
    let s = store(&m);
    let a = chunk_refs("a.rs", 1, &["new", "Parser"], &[], "a");
    let b = chunk_refs("b.rs", 1, &["new"], &[], "b");
    s.put_file(R, "a.rs", "h", std::slice::from_ref(&a))
        .expect("put");
    s.put_file(R, "b.rs", "h", std::slice::from_ref(&b))
        .expect("put");
    assert_eq!(
        s.definition_counts(R, &terms(&["new", "Parser", "Missing", "", "new"]))
            .expect("counts"),
        vec![2, 1, 0, 0, 2]
    );
    assert!(s.definition_counts(R, &[]).expect("empty").is_empty());
    // chunks_defining stays deterministic for ambiguous idents.
    let first = s.chunks_defining(R, &terms(&["new"]), 10).expect("defs");
    let mut sorted = first.clone();
    sorted.sort();
    assert_eq!(first, sorted);
}
