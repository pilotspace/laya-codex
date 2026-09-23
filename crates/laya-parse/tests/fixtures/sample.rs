//! Module docs.
use std::collections::HashMap;

/// A key-value store.
pub struct MoonStore {
    map: HashMap<String, String>,
}

pub const MAX_KEYS: usize = 1024;

pub trait Store {
    fn get(&self, key: &str) -> Option<String>;
}

impl Store for MoonStore {
    /// Fetch a key.
    fn get(&self, key: &str) -> Option<String> {
        let v = self.map.get(key);
        let v = v.cloned();
        if v.is_none() {
            return None;
        }
        let out = v;
        let out = out.map(|s| s.to_string());
        out
    }
}

fn helper() -> u32 {
    42
}
