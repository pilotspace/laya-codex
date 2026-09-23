use std::collections::{BTreeSet, HashMap};
use crate::store::MoonStore;

pub struct Cache {
    inner: HashMap<String, Entry>,
}

pub fn build(cfg: &Config) -> Cache {
    let store = MoonStore::open(cfg);
    let v = store.get("k").unwrap();
    helper(v.len());
    println!("{v}");
    Cache { inner: HashMap::new() }
}

fn helper(n: usize) -> usize {
    compute_total(n)
}
