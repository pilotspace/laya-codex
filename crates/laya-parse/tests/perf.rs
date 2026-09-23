//! Throughput check on the moon repo. Run with:
//! `cargo test -p laya-parse --release --test perf -- --ignored --nocapture`

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use laya_parse::{ChunkConfig, chunk_files, chunk_source_with, walk_repo};

const MOON: &str = "/Users/tindang/workspaces/tind-repo/moon";

#[test]
#[ignore = "perf: needs the moon repo and a release build"]
fn moon_parse_and_chunk_timing() {
    let root = Path::new(MOON);
    assert!(root.exists(), "moon repo missing at {MOON}");

    let t = Instant::now();
    let files = walk_repo(root);
    let walk = t.elapsed();

    let sources: Vec<(String, String)> = files
        .iter()
        .filter_map(|p| {
            let src = std::fs::read_to_string(p).ok()?;
            Some((
                p.strip_prefix(root).ok()?.to_string_lossy().into_owned(),
                src,
            ))
        })
        .collect();
    let bytes: usize = sources.iter().map(|(_, s)| s.len()).sum();
    let lines: usize = sources.iter().map(|(_, s)| s.lines().count()).sum();

    let cfg = ChunkConfig::default();
    let t = Instant::now();
    let mut chunks = Vec::new();
    for (path, src) in &sources {
        chunks.extend(chunk_source_with(&cfg, path, src));
    }
    let single = t.elapsed();

    let t = Instant::now();
    let par = chunk_files(root, &files);
    let parallel = t.elapsed();
    let par_chunks: usize = par
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .map(|f| f.chunks.len())
        .sum();

    let mut sizes: Vec<u32> = chunks.iter().map(|c| c.line_count()).collect();
    sizes.sort_unstable();
    let mean = sizes.iter().map(|&s| s as f64).sum::<f64>() / sizes.len().max(1) as f64;
    let median = sizes.get(sizes.len() / 2).copied().unwrap_or(0);
    let under_min = sizes
        .iter()
        .filter(|&&s| (s as usize) < cfg.min_lines)
        .count();
    let mut by_lang: BTreeMap<&str, usize> = BTreeMap::new();
    for c in &chunks {
        *by_lang.entry(c.lang.as_str()).or_default() += 1;
    }
    let with_symbol = chunks.iter().filter(|c| !c.symbol.is_empty()).count();

    println!(
        "files: {} ({} MiB, {} lines), walk {:?}",
        sources.len(),
        bytes / (1 << 20),
        lines,
        walk
    );
    println!("single-threaded parse+chunk: {:?}", single);
    println!(
        "rayon chunk_files (read+parse+chunk): {:?} ({} chunks)",
        parallel, par_chunks
    );
    println!(
        "chunks: {} | mean {:.1} lines | median {} | p10 {} | p90 {} | max {} | <min {} ({:.1}%) | with symbol {:.1}%",
        sizes.len(),
        mean,
        median,
        sizes[sizes.len() / 10],
        sizes[sizes.len() * 9 / 10],
        sizes.last().unwrap(),
        under_min,
        100.0 * under_min as f64 / sizes.len() as f64,
        100.0 * with_symbol as f64 / sizes.len() as f64
    );
    println!("by lang: {by_lang:?}");

    // Refs: density, cap pressure and how many resolve to a definition somewhere in the repo.
    let defined: std::collections::HashSet<&str> = chunks
        .iter()
        .flat_map(|c| c.defines.iter().map(String::as_str))
        .collect();
    let total_refs: usize = chunks.iter().map(|c| c.refs.len()).sum();
    let with_refs = chunks.iter().filter(|c| !c.refs.is_empty()).count();
    let capped = chunks
        .iter()
        .filter(|c| c.refs.len() == laya_parse::MAX_REFS)
        .count();
    let resolvable = chunks
        .iter()
        .flat_map(|c| &c.refs)
        .filter(|r| defined.contains(r.as_str()))
        .count();
    println!(
        "refs: {:.1}/chunk | chunks with refs {:.1}% | at cap {:.1}% | resolvable to a repo define {:.1}%",
        total_refs as f64 / chunks.len().max(1) as f64,
        100.0 * with_refs as f64 / chunks.len().max(1) as f64,
        100.0 * capped as f64 / chunks.len().max(1) as f64,
        100.0 * resolvable as f64 / total_refs.max(1) as f64
    );
    assert!(
        single.as_secs_f64() < 3.0,
        "single-threaded chunking took {single:?}"
    );
}
