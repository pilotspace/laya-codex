//! Line-window fallback for text files and unparseable sources: windows of
//! `text_window_lines` (capped at `max_lines`), cut at the last blank line that keeps the
//! window at least `min_lines` long, no overlap; a short tail merges into its predecessor.

use laya_core::{Chunk, Lang};

use crate::ChunkConfig;
use crate::lines::Lines;

fn heading(line: &str) -> Option<&str> {
    let t = line.trim_start();
    let rest = t.trim_start_matches('#');
    let level = t.len() - rest.len();
    (1..=6)
        .contains(&level)
        .then_some(())
        .filter(|_| rest.starts_with(' '))
        .map(|_| rest.trim())
}

pub(crate) fn chunk_text(cfg: &ChunkConfig, path: &str, lines: &Lines<'_>) -> Vec<Chunk> {
    let n = lines.len();
    let window = cfg.text_window_lines.min(cfg.max_lines).max(1);
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut s = 0;
    loop {
        while s < n && lines.is_blank(s) {
            s += 1;
        }
        if s >= n {
            break;
        }
        let hard_end = s + window - 1;
        let mut end = if hard_end >= n - 1 {
            n - 1
        } else {
            // Latest blank row b in [s + min, hard_end + 1] -> the window ends at b - 1.
            let lo = s + cfg.min_lines.max(1);
            (lo..=hard_end + 1)
                .rev()
                .find(|&b| lines.is_blank(b))
                .map_or(hard_end, |b| b - 1)
        };
        while end > s && lines.is_blank(end) {
            end -= 1;
        }
        spans.push((s, end));
        s = end + 1;
    }
    // Undersized windows merge into their predecessor (else successor) when the result fits.
    let mut i = 0;
    while i < spans.len() {
        let (s, e) = spans[i];
        if e - s + 1 >= cfg.min_lines {
            i += 1;
        } else if i > 0 && e - spans[i - 1].0 < cfg.max_lines {
            spans[i - 1].1 = e;
            spans.remove(i);
            i -= 1;
        } else if i + 1 < spans.len() && spans[i + 1].1 - s < cfg.max_lines {
            spans[i + 1].0 = s;
            spans.remove(i);
        } else {
            i += 1;
        }
    }

    let markdown = {
        let p = path.to_ascii_lowercase();
        p.ends_with(".md") || p.ends_with(".markdown") || p.ends_with(".mdx")
    };
    let mut current_heading = String::new();
    let mut scanned = 0;
    spans
        .into_iter()
        .map(|(s, e)| {
            if markdown {
                while scanned <= s {
                    if let Some(h) = heading(lines.row(scanned)) {
                        current_heading = h.to_string();
                    }
                    scanned += 1;
                }
            }
            Chunk {
                path: path.to_string(),
                start_line: (s + 1) as u32,
                end_line: (e + 1) as u32,
                lang: Lang::Text,
                symbol: current_heading.clone(),
                kind: "window".to_string(),
                defines: Vec::new(),
                text: lines.text(s, e).to_string(),
            }
        })
        .collect()
}
