//! Latency microbench (ignored by default):
//!
//! ```text
//! cargo test --release -p laya-store --test bench_it -- --ignored --nocapture
//! ```
//!
//! Indexes 10k synthetic code chunks (Zipf-distributed identifier vocabulary, ~120 tokens per
//! chunk) into a private moon, then measures `bm25` with 10 terms, plus `get_chunks(10)` and
//! `chunks_defining`, and prints p50/p95/p99.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use laya_core::{Chunk, Store};
use laya_store::{MoonStore, StoreConfig};

const REPO: &str = "bbbbbbbbbbbb";
const FILES: usize = 1000;
const CHUNKS_PER_FILE: usize = 10;
const VOCAB: usize = 5000;
const QUERIES: usize = 300;

fn vocab() -> Vec<String> {
    let parts = [
        "parse", "config", "load", "store", "index", "query", "chunk", "file", "path", "cache",
        "shard", "event", "loop", "wal", "flush", "commit", "read", "write", "hash", "table",
        "node", "graph", "vector", "score", "rank", "token", "search", "text", "field", "schema",
        "server", "client", "conn", "pool", "retry", "timeout", "error", "result", "state", "lock",
    ];
    (0..VOCAB)
        .map(|i| {
            let a = parts[i % parts.len()];
            let b = parts[(i / parts.len()) % parts.len()];
            format!("{a}_{b}_{i}")
        })
        .collect()
}

/// Zipf(s=1.1) sample via inverse CDF on precomputed weights.
struct Zipf {
    cdf: Vec<f64>,
}

impl Zipf {
    fn new(n: usize) -> Self {
        let mut acc = 0.0;
        let cdf = (1..=n)
            .map(|k| {
                acc += 1.0 / (k as f64).powf(1.1);
                acc
            })
            .collect::<Vec<_>>();
        let total = acc;
        Zipf {
            cdf: cdf.into_iter().map(|c| c / total).collect(),
        }
    }

    fn sample(&self, rng: &mut fastrand::Rng) -> usize {
        let u = rng.f64();
        self.cdf.partition_point(|&c| c < u).min(self.cdf.len() - 1)
    }
}

fn pct(v: &mut [Duration], p: f64) -> Duration {
    v.sort_unstable();
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}

fn report(name: &str, mut v: Vec<Duration>) {
    let (p50, p95, p99) = (pct(&mut v, 0.50), pct(&mut v, 0.95), pct(&mut v, 0.99));
    println!(
        "{name:<44} n={:<4} p50={p50:>9.3?} p95={p95:>9.3?} p99={p99:>9.3?}",
        v.len()
    );
}

fn build_corpus() -> Vec<(String, Vec<Chunk>)> {
    let vocab = vocab();
    let zipf = Zipf::new(VOCAB);
    let mut rng = fastrand::Rng::with_seed(7);
    (0..FILES)
        .map(|f| {
            let path = format!("src/mod_{}/file_{f}.rs", f % 37);
            let chunks = (0..CHUNKS_PER_FILE)
                .map(|c| {
                    let def = format!("Item{f}x{c}");
                    let body: Vec<&str> = (0..120)
                        .map(|_| vocab[zipf.sample(&mut rng)].as_str())
                        .collect();
                    common::chunk(
                        &path,
                        (c * 20 + 1) as u32,
                        &format!("fn {def}"),
                        &[&def],
                        &format!("fn {def}() {{ {} }}", body.join("(); ")),
                    )
                })
                .collect();
            (path, chunks)
        })
        .collect()
}

#[test]
#[ignore = "latency microbench; run with --ignored --nocapture"]
fn bm25_latency_10_terms_over_10k_chunks() {
    // LAYA_BENCH_PORT=<p> benchmarks an already-running moon (data is left in place).
    let external = std::env::var("LAYA_BENCH_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok());
    let _own;
    let port = match external {
        Some(p) => p,
        None => {
            _own = require_moon!();
            _own.port
        }
    };
    let store = Arc::new(MoonStore::new(StoreConfig::local(port)).expect("store"));
    store.ensure_index(REPO).expect("index");

    let corpus = build_corpus();
    let t = Instant::now();
    for (path, chunks) in &corpus {
        store.put_file(REPO, path, "h", chunks).expect("put");
    }
    let idx = t.elapsed();
    println!(
        "indexed {} chunks in {idx:.2?} ({:.0} chunks/s, one put_file per 10-chunk file)",
        FILES * CHUNKS_PER_FILE,
        (FILES * CHUNKS_PER_FILE) as f64 / idx.as_secs_f64()
    );

    let vocab = vocab();
    let zipf = Zipf::new(VOCAB);
    let mut rng = fastrand::Rng::with_seed(11);
    // Realistic mixes: prompt terms are a blend of frequent and rare identifiers.
    let queries: Vec<Vec<String>> = (0..QUERIES)
        .map(|_| {
            (0..10)
                .map(|i| {
                    if i % 2 == 0 {
                        vocab[zipf.sample(&mut rng)].clone()
                    } else {
                        vocab[rng.usize(..VOCAB)].clone()
                    }
                })
                .collect()
        })
        .collect();

    // Warm-up (connections, server caches).
    for q in queries.iter().take(20) {
        store.bm25(REPO, q, 50).expect("warm");
    }

    for per_term in [50usize, 200, 1000] {
        let cfg = StoreConfig {
            per_term_limit: per_term,
            ..StoreConfig::local(port)
        };
        let s = MoonStore::new(cfg).expect("store");
        s.bm25(REPO, &queries[0], 50).expect("warm");
        let lat: Vec<Duration> = queries
            .iter()
            .map(|q| {
                let t = Instant::now();
                let hits = s.bm25(REPO, q, 50).expect("bm25");
                assert!(!hits.is_empty());
                t.elapsed()
            })
            .collect();
        report(
            &format!("bm25 10 terms, limit 50, per_term {per_term}"),
            lat,
        );
    }

    let lat: Vec<Duration> = queries
        .iter()
        .map(|q| {
            let t = Instant::now();
            store.bm25(REPO, &q[..1], 50).expect("bm25");
            t.elapsed()
        })
        .collect();
    report("bm25 1 term (baseline round trip)", lat);

    let ids: Vec<String> = corpus.iter().take(10).map(|(_, c)| c[0].id()).collect();
    let lat: Vec<Duration> = (0..QUERIES)
        .map(|_| {
            let t = Instant::now();
            assert_eq!(store.get_chunks(REPO, &ids).expect("get").len(), 10);
            t.elapsed()
        })
        .collect();
    report("get_chunks 10", lat);

    let idents: Vec<String> = (0..5)
        .map(|i| format!("Item{}x{}", i * 97, i % 10))
        .collect();
    let lat: Vec<Duration> = (0..QUERIES)
        .map(|_| {
            let t = Instant::now();
            store.chunks_defining(REPO, &idents, 20).expect("defs");
            t.elapsed()
        })
        .collect();
    report("chunks_defining 5 idents", lat);

    // 4 concurrent query threads (pool of 4) — hook + MCP + reindex overlap.
    let handles: Vec<_> = (0..4)
        .map(|k| {
            let s = Arc::clone(&store);
            let qs: Vec<Vec<String>> = queries.iter().skip(k).step_by(4).cloned().collect();
            std::thread::spawn(move || {
                qs.iter()
                    .map(|q| {
                        let t = Instant::now();
                        s.bm25(REPO, q, 50).expect("bm25");
                        t.elapsed()
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let lat: Vec<Duration> = handles
        .into_iter()
        .flat_map(|h| h.join().expect("join"))
        .collect();
    report("bm25 10 terms, 4 concurrent threads", lat);
}
