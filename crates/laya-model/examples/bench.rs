//! Latency of `LayaModel::noul_ids` for batch × state-token grids.
//!
//! ```text
//! cargo run --release -p laya-model --features metal --example bench -- --device metal
//! cargo run --release -p laya-model --example bench -- --device cpu --batches 16 --tokens 256
//! ```

use std::path::PathBuf;
use std::time::Instant;

use laya_model::{DeviceKind, LayaModel};

const SAMPLE: &str = r#"
impl Store for MoonStore {
    fn put_file(&self, repo_id: &str, path: &str, file_hash: &str, chunks: &[Chunk]) -> Result<()> {
        let mut conn = self.pool.get().map_err(|e| Error::StoreUnavailable(e.to_string()))?;
        let key = format!("{}:file:{}", repo_id, path);
        let mut pipe = redis::pipe();
        for c in chunks {
            pipe.hset_multiple(format!("{}:chunk:{}", repo_id, c.id()), &[("path", &c.path), ("text", &c.text)]);
        }
        pipe.set(key, file_hash).query::<()>(&mut *conn).map_err(|e| Error::Store(e.to_string()))
    }
}
"#;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let dir = arg(&args, "--model")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("LAYA_MODEL_DIR").map(PathBuf::from))
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join(".cache/laya-codex/models/laya-base")
        });
    let kind = match arg(&args, "--device").as_deref() {
        Some("cpu") => DeviceKind::Cpu,
        Some("metal") => DeviceKind::Metal,
        _ => DeviceKind::Auto,
    };
    let list = |name: &str, default: &str| -> Vec<usize> {
        arg(&args, name)
            .unwrap_or_else(|| default.to_string())
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect()
    };
    let batches = list("--batches", "16,32");
    let tokens = list("--tokens", "256,448");
    let iters: usize = arg(&args, "--iters")
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);

    let t0 = Instant::now();
    let model = LayaModel::load(&dir, kind)?;
    println!(
        "loaded {} on {:?} ({:?}) in {:.1}s",
        dir.display(),
        model.device_kind(),
        model.dtype(),
        t0.elapsed().as_secs_f64()
    );
    let question =
        "Is this source code relevant to the software change: \"O(1) fast path for HGET\"?";
    let long_text = SAMPLE.repeat(12);

    println!(
        "{:>6} {:>6} {:>8} {:>10} {:>10} {:>10}",
        "batch", "state", "seq_len", "mean_ms", "min_ms", "tok/s"
    );
    for &n_tok in &tokens {
        let ids = model.sequences().encode_state(&long_text, Some(n_tok))?;
        assert_eq!(ids.len(), n_tok, "sample too short for {n_tok} tokens");
        for &b in &batches {
            let states: Vec<Vec<u32>> = (0..b)
                .map(|i| {
                    // Vary rows slightly so nothing is cached by accident, keep the length fixed.
                    let mut v = ids.clone();
                    v[0] = ids[i % ids.len()];
                    v
                })
                .collect();
            // Warm-up (kernel compilation, allocator).
            let _ = model.noul_ids(question, &states, None)?;
            let mut times = Vec::with_capacity(iters);
            for _ in 0..iters {
                let t = Instant::now();
                let p = model.noul_ids(question, &states, None)?;
                times.push(t.elapsed().as_secs_f64() * 1e3);
                assert_eq!(p.len(), b);
            }
            let mean = times.iter().sum::<f64>() / times.len() as f64;
            let min = times.iter().cloned().fold(f64::INFINITY, f64::min);
            let seq_len = model
                .sequences()
                .build_from_state_ids(&ids, &laya_model::Question::noul(question))?
                .ids
                .len();
            println!(
                "{:>6} {:>6} {:>8} {:>10.1} {:>10.1} {:>10.0}",
                b,
                n_tok,
                seq_len,
                mean,
                min,
                (b * seq_len) as f64 / (mean / 1e3)
            );
        }
    }
    Ok(())
}
