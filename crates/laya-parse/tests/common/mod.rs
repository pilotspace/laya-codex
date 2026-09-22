//! Shared invariant checks for chunker tests.
#![allow(dead_code)]

use laya_core::Chunk;
use laya_parse::ChunkConfig;

/// Lines of `src` split on `\n` (a trailing newline does not open a new line).
pub fn lines(src: &str) -> Vec<&str> {
    let mut v: Vec<&str> = src.split('\n').collect();
    if src.ends_with('\n') {
        v.pop();
    }
    v
}

pub fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// Coverage of every non-blank line, no overlap, `max_lines` respected, exact text, trimmed
/// boundaries, and "no mergeable undersized chunk left".
pub fn assert_invariants(cfg: &ChunkConfig, path: &str, src: &str, chunks: &[Chunk]) {
    let ls = lines(src);
    let mut covered = vec![false; ls.len()];
    let mut prev_end = 0u32;
    for (i, c) in chunks.iter().enumerate() {
        assert!(
            c.start_line >= 1 && c.start_line <= c.end_line,
            "{path}: bad range {c:?}"
        );
        assert!(
            c.start_line > prev_end,
            "{path}: overlap/unsorted at chunk {i} ({}-{})",
            c.start_line,
            c.end_line
        );
        assert!(
            c.line_count() as usize <= cfg.max_lines,
            "{path}: chunk {}-{} exceeds max_lines {}",
            c.start_line,
            c.end_line,
            cfg.max_lines
        );
        let s = c.start_line as usize - 1;
        let e = c.end_line as usize - 1;
        assert!(e < ls.len(), "{path}: chunk past EOF");
        assert!(
            !is_blank(ls[s]) && !is_blank(ls[e]),
            "{path}: chunk {}-{} not trimmed",
            c.start_line,
            c.end_line
        );
        // Exact source bytes of the span; a CRLF's `\r` on the final line is not part of the text.
        let want = ls[s..=e].join("\n");
        let want = want.strip_suffix('\r').unwrap_or(&want);
        assert_eq!(
            c.text, want,
            "{path}: text mismatch at {}-{}",
            c.start_line, c.end_line
        );
        for flag in &mut covered[s..=e] {
            *flag = true;
        }
        prev_end = c.end_line;
    }
    for (i, l) in ls.iter().enumerate() {
        assert!(
            is_blank(l) || covered[i],
            "{path}: non-blank line {} not covered",
            i + 1
        );
    }
    // Undersized chunks exist only when merging with either neighbour would exceed max_lines.
    for (i, c) in chunks.iter().enumerate() {
        if (c.line_count() as usize) >= cfg.min_lines {
            continue;
        }
        if i > 0 {
            let merged = c.end_line - chunks[i - 1].start_line + 1;
            assert!(
                merged as usize > cfg.max_lines,
                "{path}: chunk {}-{} could merge back",
                c.start_line,
                c.end_line
            );
        }
        if i + 1 < chunks.len() {
            let merged = chunks[i + 1].end_line - c.start_line + 1;
            assert!(
                merged as usize > cfg.max_lines,
                "{path}: chunk {}-{} could merge forward",
                c.start_line,
                c.end_line
            );
        }
    }
}
