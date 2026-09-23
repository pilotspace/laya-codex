//! Which line ranges of one file to inline: the ranked span, optionally widened into its
//! surroundings, plus a few more listed spans of the same file, merged where they touch.
//!
//! Benchmark v2: 43% of Claude's Reads after an injection went to a file whose code was already
//! inlined, about half of them next to the inlined block and a sixth to another span the injection
//! had only listed. Sending those lines with the first block saves the Read turns.

/// How [`plan_file_blocks`] sizes one file's inlined code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockPolicy {
    /// Lines added on each side of the ranked span when widening (then snapped outwards to the
    /// indexed chunks the edges fall in).
    pub margin: u32,
    /// Upper bound on the inlined lines of one file, all blocks together.
    pub max_lines_per_file: u32,
    /// Other listed spans of the same file that may be inlined too.
    pub extra_spans_per_file: usize,
    /// Blocks this close (in lines) are merged into one.
    pub merge_gap: u32,
}

impl Default for BlockPolicy {
    fn default() -> Self {
        BlockPolicy {
            margin: 30,
            max_lines_per_file: 150,
            extra_spans_per_file: 1,
            merge_gap: 3,
        }
    }
}

/// Line ranges (1-based, inclusive, sorted, non-overlapping) to inline for a file of `file_len`
/// lines whose ranked span is `primary`. `widen` grows `primary` by `policy.margin` lines each
/// side and snaps each edge outwards to the chunk (from `chunks`) it falls inside. `extras` are
/// other listed spans of the file, best first; up to `policy.extra_spans_per_file` are added.
/// Ranges within `policy.merge_gap` lines are merged. The block holding `primary` is always kept
/// (falling back to the unsnapped, then the unwidened range if needed to fit
/// `policy.max_lines_per_file`); extras are added only while the file stays within it.
pub fn plan_file_blocks(
    primary: (u32, u32),
    extras: &[(u32, u32)],
    widen: bool,
    chunks: &[(u32, u32)],
    file_len: u32,
    policy: &BlockPolicy,
) -> Vec<(u32, u32)> {
    if file_len == 0 {
        return vec![primary];
    }
    let clip = |(s, e): (u32, u32)| {
        let s = s.clamp(1, file_len);
        (s, e.clamp(s, file_len))
    };
    let fits = |r: &(u32, u32)| lines(*r) <= policy.max_lines_per_file;
    let ranked = clip(primary);
    let mut main = ranked;
    if widen {
        let plain = (
            ranked.0.saturating_sub(policy.margin).max(1),
            ranked.1.saturating_add(policy.margin).min(file_len),
        );
        let start = chunks
            .iter()
            .filter(|c| c.0 <= plain.0 && plain.0 <= c.1)
            .map(|c| c.0)
            .min()
            .unwrap_or(plain.0);
        let end = chunks
            .iter()
            .filter(|c| c.0 <= plain.1 && plain.1 <= c.1)
            .map(|c| c.1)
            .max()
            .unwrap_or(plain.1);
        let snapped = clip((start, end));
        main = [snapped, plain].into_iter().find(fits).unwrap_or(ranked);
    }
    let mut blocks = vec![main];
    for &extra in extras.iter().take(policy.extra_spans_per_file) {
        let mut with = blocks.clone();
        with.push(clip(extra));
        let merged = merge(with, policy.merge_gap);
        if merged == blocks
            || merged.iter().map(|&r| lines(r)).sum::<u32>() <= policy.max_lines_per_file
        {
            blocks = merged;
        }
    }
    merge(blocks, policy.merge_gap)
}

fn lines((s, e): (u32, u32)) -> u32 {
    e - s + 1
}

/// Sort and merge ranges that overlap or lie within `gap` lines of each other.
fn merge(mut ranges: Vec<(u32, u32)>, gap: u32) -> Vec<(u32, u32)> {
    ranges.sort_unstable();
    let mut out: Vec<(u32, u32)> = Vec::with_capacity(ranges.len());
    for r in ranges {
        match out.last_mut() {
            Some(last) if r.0 <= last.1.saturating_add(gap).saturating_add(1) => {
                last.1 = last.1.max(r.1);
            }
            _ => out.push(r),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> BlockPolicy {
        BlockPolicy::default()
    }

    #[test]
    fn without_widening_or_extras_the_ranked_span_is_the_block() {
        assert_eq!(
            plan_file_blocks((100, 140), &[], false, &[], 500, &p()),
            vec![(100, 140)]
        );
    }

    #[test]
    fn widening_adds_the_margin_and_snaps_to_the_chunks_at_the_edges() {
        // 100-140 +/- 30 = 70-170; line 70 is inside chunk 60-80 and line 170 inside 165-190.
        let chunks = [(60, 80), (81, 99), (100, 140), (141, 164), (165, 190)];
        assert_eq!(
            plan_file_blocks((100, 140), &[], true, &chunks, 500, &p()),
            vec![(60, 190)]
        );
    }

    #[test]
    fn widening_stays_inside_the_file() {
        assert_eq!(
            plan_file_blocks((5, 20), &[], true, &[], 40, &p()),
            vec![(1, 40)]
        );
    }

    #[test]
    fn a_snap_that_would_exceed_the_cap_falls_back_to_the_plain_margin() {
        // Snapping to 1-300 would be 300 lines; the plain margin 70-170 (101 lines) fits.
        let chunks = [(1, 99), (141, 300)];
        assert_eq!(
            plan_file_blocks((100, 140), &[], true, &chunks, 500, &p()),
            vec![(70, 170)]
        );
        // A span already at the cap is not widened at all.
        let tight = BlockPolicy {
            max_lines_per_file: 41,
            ..p()
        };
        assert_eq!(
            plan_file_blocks((100, 140), &[], true, &[], 500, &tight),
            vec![(100, 140)]
        );
    }

    #[test]
    fn an_extra_listed_span_is_added_and_merged_when_it_touches() {
        // Separate: 300-320 is far from 100-140.
        assert_eq!(
            plan_file_blocks((100, 140), &[(300, 320)], false, &[], 500, &p()),
            vec![(100, 140), (300, 320)]
        );
        // Touching (gap 2 <= 3): one block.
        assert_eq!(
            plan_file_blocks((100, 140), &[(143, 160)], false, &[], 500, &p()),
            vec![(100, 160)]
        );
        // Only `extra_spans_per_file` extras, best first.
        assert_eq!(
            plan_file_blocks((100, 140), &[(300, 320), (400, 410)], false, &[], 500, &p()),
            vec![(100, 140), (300, 320)]
        );
    }

    #[test]
    fn extras_never_push_the_file_over_the_cap() {
        let small = BlockPolicy {
            max_lines_per_file: 60,
            ..p()
        };
        // Primary 41 lines + extra 21 lines = 62 > 60: the extra is left out.
        assert_eq!(
            plan_file_blocks((100, 140), &[(300, 320)], false, &[], 500, &small),
            vec![(100, 140)]
        );
    }

    #[test]
    fn an_extra_inside_the_widened_block_adds_nothing() {
        assert_eq!(
            plan_file_blocks((100, 140), &[(150, 160)], true, &[], 500, &p()),
            vec![(70, 170)]
        );
    }
}
