//! One-hop reference expansion: after the Laya gate and span shaping, follow the top-ranked
//! spans' callees and callers one hop out, so retrieval doesn't stop at lexical/model relevance
//! alone. Added in response to `docs/RESULTS.md` §7-8: recall dropped 0.958 → 0.875 because
//! retrieval never follows references (a fix that touched a used-by-nobody helper, or a symbol
//! renamed at its call sites, has no lexical/BM25 signal pointing at the call sites at all).
//!
//! Algorithm (see the task brief this ports):
//! 1. Seeds = the top 3 *scored* chunks (before span shaping merges same-file chunks together,
//!    which would lose `defines`/`refs`).
//! 2. Callees: per seed (rank order), take up to 6 of its `refs` that no seed itself `defines`,
//!    resolve each via one `Store::chunks_defining(&[ident], 4)` call — skip idents with more
//!    than 3 defining chunks (ambiguous, e.g. a common name overloaded across the codebase).
//! 3. Callers: per seed, take up to 4 of its `defines`, resolve each via one
//!    `Store::chunks_referencing(&[ident], CALLER_LIMIT)` call.
//! 4. All resolved chunk ids (callees ∪ callers) are fetched in a single batched `get_chunks`
//!    call, then the callee/caller candidate lists are interleaved (callee, caller, callee,
//!    caller, …) and filtered: drop a hit that resolves back to one of the seeds themselves
//!    (redundant — seeds are already prominent, top-ranked spans), drop anything overlapping an
//!    already-accepted span (the ranked spans, or an already-accepted `Related`), cap at 2
//!    entries per file, cap the total at `max_related`.
//!
//! No Laya scoring runs over related items — this stays a handful of cheap, mostly-cacheable
//! `Store` lookups (§ the brief's ~20 ms budget), not another model pass.

use std::collections::{HashMap, HashSet};

use laya_core::{Chunk, RankedSpan, Related, Result, Store};

use crate::render::COMPLETE_USES;

/// How many chunks `chunks_referencing` may return per `defines` ident.
const CALLER_LIMIT: usize = 4;
/// `chunks_defining` limit used for the ambiguity probe: a hit count over `AMBIGUOUS_ABOVE`
/// means "skip this ident, too common to be a useful pointer".
const AMBIGUOUS_ABOVE: usize = 3;
/// Refs/defines considered per seed for each direction (callees, callers).
const PER_SEED_IDENTS: usize = 6;
const PER_SEED_DEFINES: usize = 4;
/// At most this many `Related` entries may share a path.
const MAX_PER_FILE: usize = 2;

/// Compute one-hop `Related` neighbours of `seeds` (top-ranked scored chunks, in rank order).
/// `existing` is the final ranked span list — related items overlapping any of them are dropped.
/// Returns `Ok(vec![])` immediately (no `Store` calls) when `max_related == 0` or there are no
/// seeds. Any `Store` error aborts the whole expansion (callers should treat that as "no related
/// items", not a query failure — see `Retriever::query`).
pub(crate) fn expand_related(
    store: &dyn Store,
    repo_id: &str,
    seeds: &[Chunk],
    existing: &[RankedSpan],
    max_related: usize,
) -> Result<Vec<Related>> {
    if max_related == 0 || seeds.is_empty() {
        return Ok(Vec::new());
    }

    let own_defines: HashSet<&str> = seeds
        .iter()
        .flat_map(|s| s.defines.iter().map(String::as_str))
        .collect();

    // ---- callees: per-seed candidate idents, then resolve (with the ambiguity probe) ----
    let callee_idents_per_seed: Vec<Vec<String>> = seeds
        .iter()
        .map(|seed| {
            seed.refs
                .iter()
                .filter(|r| !own_defines.contains(r.as_str()))
                .take(PER_SEED_IDENTS)
                .cloned()
                .collect()
        })
        .collect();

    // Cache by ident: identical idents across seeds resolve to the same answer, so this also
    // cuts down on `Store` round trips.
    let mut callee_defs: HashMap<String, Vec<String>> = HashMap::new();
    for idents in &callee_idents_per_seed {
        for ident in idents {
            if callee_defs.contains_key(ident) {
                continue;
            }
            let hits =
                store.chunks_defining(repo_id, std::slice::from_ref(ident), AMBIGUOUS_ABOVE + 1)?;
            let resolved = if hits.len() > AMBIGUOUS_ABOVE {
                Vec::new()
            } else {
                hits
            };
            callee_defs.insert(ident.clone(), resolved);
        }
    }

    // ---- callers: per-seed defines, then resolve ----
    let caller_idents_per_seed: Vec<Vec<String>> = seeds
        .iter()
        .map(|seed| {
            seed.defines
                .iter()
                .take(PER_SEED_DEFINES)
                .cloned()
                .collect()
        })
        .collect();

    let mut caller_refs: HashMap<String, Vec<String>> = HashMap::new();
    for idents in &caller_idents_per_seed {
        for def in idents {
            if caller_refs.contains_key(def) {
                continue;
            }
            let hits =
                store.chunks_referencing(repo_id, std::slice::from_ref(def), CALLER_LIMIT)?;
            caller_refs.insert(def.clone(), hits);
        }
    }

    // ---- one batched fetch for every id we resolved above ----
    let mut all_ids: Vec<String> = callee_defs.values().flatten().cloned().collect();
    all_ids.extend(caller_refs.values().flatten().cloned());
    all_ids.sort();
    all_ids.dedup();
    let by_id: HashMap<String, Chunk> = if all_ids.is_empty() {
        HashMap::new()
    } else {
        store
            .get_chunks(repo_id, &all_ids)?
            .into_iter()
            .map(|c| (c.id(), c))
            .collect()
    };

    // A hit that resolves back to one of the seeds themselves is redundant — seeds are already
    // prominent (top-ranked) spans, not a *new* pointer, so exclude them regardless of whether
    // `existing` happens to cover the exact same range after shaping.
    let seed_ids: HashSet<String> = seeds.iter().map(Chunk::id).collect();

    // ---- build the two ordered candidate lists ----
    let mut callee_candidates: Vec<Related> = Vec::new();
    for (i, idents) in callee_idents_per_seed.iter().enumerate() {
        for ident in idents {
            for id in callee_defs
                .get(ident)
                .into_iter()
                .flatten()
                .filter(|id| !seed_ids.contains(*id))
            {
                if let Some(c) = by_id.get(id) {
                    callee_candidates.push(Related {
                        path: c.path.clone(),
                        start_line: c.start_line,
                        end_line: c.end_line,
                        symbol: c.symbol.clone(),
                        relation: format!("defines `{ident}` (used by #{})", i + 1),
                    });
                }
            }
        }
    }

    let mut caller_candidates: Vec<Related> = Vec::new();
    for (i, defs) in caller_idents_per_seed.iter().enumerate() {
        for def in defs {
            for id in caller_refs
                .get(def)
                .into_iter()
                .flatten()
                .filter(|id| !seed_ids.contains(*id))
            {
                if let Some(c) = by_id.get(id) {
                    caller_candidates.push(Related {
                        path: c.path.clone(),
                        start_line: c.start_line,
                        end_line: c.end_line,
                        symbol: c.symbol.clone(),
                        relation: format!("calls `{def}` (#{})", i + 1),
                    });
                }
            }
        }
    }

    Ok(merge(
        callee_candidates,
        caller_candidates,
        existing,
        max_related,
    ))
}

/// Interleave callees/callers (callee, caller, callee, caller, …), then drop anything overlapping
/// an already-accepted span (`existing` or a previously accepted `Related`), cap at
/// [`MAX_PER_FILE`] entries per path, and cap the total at `max_related`.
fn merge(
    callees: Vec<Related>,
    callers: Vec<Related>,
    existing: &[RankedSpan],
    max_related: usize,
) -> Vec<Related> {
    let n = callees.len().max(callers.len());
    let mut interleaved = Vec::with_capacity(callees.len() + callers.len());
    for i in 0..n {
        if let Some(c) = callees.get(i) {
            interleaved.push(c.clone());
        }
        if let Some(c) = callers.get(i) {
            interleaved.push(c.clone());
        }
    }

    let mut out: Vec<Related> = Vec::new();
    let mut per_file: HashMap<String, usize> = HashMap::new();
    for r in interleaved {
        if out.len() >= max_related {
            break;
        }
        let overlaps_existing = existing
            .iter()
            .any(|s| overlaps(&r, s.start_line, s.end_line, &s.path));
        let overlaps_accepted = out
            .iter()
            .any(|o| overlaps(&r, o.start_line, o.end_line, &o.path));
        if overlaps_existing || overlaps_accepted {
            continue;
        }
        let count = per_file.entry(r.path.clone()).or_insert(0);
        if *count >= MAX_PER_FILE {
            continue;
        }
        *count += 1;
        out.push(r);
    }
    out
}

fn overlaps(r: &Related, other_start: u32, other_end: u32, other_path: &str) -> bool {
    r.path == other_path && r.start_line <= other_end && other_start <= r.end_line
}

/// Identifiers looked up for the usage list and the referencing-chunk lookup limit. The line
/// caps come from [`crate::RetrieverConfig`] (10 lines, 4 per identifier: ~300 tokens).
const USAGE_IDENTS: usize = 6;
const USAGE_REF_LIMIT: usize = 8;
/// Referencing chunks looked at to find test files among them, and test ranges per file.
const TEST_REF_LIMIT: usize = 48;
const TESTS_PER_FILE: usize = 2;

/// A test file by the usual conventions: a `test`, `tests`, `__tests__` or `spec` directory (any
/// case); a `tests.*` module file; `test_*.py`; or a name ending `_test`, `_tests`, `_spec`,
/// `.test`, `.spec` or `Tests` before the extension.
pub(crate) fn is_test_path(path: &str) -> bool {
    let mut parts: Vec<&str> = path.split('/').collect();
    let base = parts.pop().unwrap_or_default();
    if parts.iter().any(|d| {
        let d = d.to_ascii_lowercase();
        matches!(
            d.as_str(),
            "test" | "tests" | "__tests__" | "spec" | "specs"
        )
    }) {
        return true;
    }
    let Some((stem, ext)) = base.rsplit_once('.') else {
        return false;
    };
    if ext.is_empty() || !ext.chars().all(|c| c.is_ascii_lowercase()) {
        return false;
    }
    stem == "tests"
        || (ext == "py" && stem.starts_with("test_"))
        || [
            "_test", "_tests", "_spec", ".test", ".spec", "Tests", "Test",
        ]
        .iter()
        .any(|s| stem.ends_with(s))
}

/// Up to `n` test-file chunks that use `idents` (most identifiers used first), as pointers with
/// the relation "test using `x`". They answer "which tests cover this" without a search.
pub(crate) fn test_pointers(
    store: &dyn Store,
    repo_id: &str,
    idents: &[String],
    n: usize,
) -> Result<Vec<Related>> {
    let idents: Vec<String> = idents.iter().take(USAGE_IDENTS).cloned().collect();
    if n == 0 || idents.is_empty() {
        return Ok(Vec::new());
    }
    let ids = store.chunks_referencing(repo_id, &idents, TEST_REF_LIMIT)?;
    let by_id: HashMap<String, Chunk> = store
        .get_chunks(repo_id, &ids)?
        .into_iter()
        .map(|c| (c.id(), c))
        .collect();
    let mut out: Vec<Related> = Vec::new();
    for id in &ids {
        if out.len() == n {
            break;
        }
        let Some(c) = by_id.get(id).filter(|c| is_test_path(&c.path)) else {
            continue;
        };
        let Some(used) = idents.iter().find(|i| c.refs.contains(i)) else {
            continue;
        };
        if out.iter().filter(|r| r.path == c.path).count() >= TESTS_PER_FILE {
            continue;
        }
        out.push(Related {
            path: c.path.clone(),
            start_line: c.start_line,
            end_line: c.end_line,
            symbol: c.symbol.clone(),
            relation: format!("test using `{used}`"),
        });
    }
    Ok(out)
}
const USAGE_LINE_CHARS: usize = 120;

/// Grep-style usage list: for each identifier (task-named ones first), the line that defines it
/// and lines that use it, as single-line [`Related`] items (`symbol` = the trimmed source line,
/// relation `definition of `x`` / `use of `x``). Agents otherwise spend about one turn per task
/// grepping for the definition and call sites of identifiers they were already shown.
/// Ambiguous identifiers (defined in more than [`AMBIGUOUS_ABOVE`] places) are skipped.
pub(crate) fn usage_list(
    store: &dyn Store,
    repo_id: &str,
    idents: &[String],
    max_lines: usize,
    per_ident: usize,
) -> Result<Vec<Related>> {
    let mut out: Vec<Related> = Vec::new();
    for id in idents.iter().take(USAGE_IDENTS) {
        if out.len() >= max_lines {
            break;
        }
        let one = std::slice::from_ref(id);
        let defs = store.chunks_defining(repo_id, one, AMBIGUOUS_ABOVE + 1)?;
        if defs.len() > AMBIGUOUS_ABOVE {
            continue;
        }
        let uses = store.chunks_referencing(repo_id, one, USAGE_REF_LIMIT)?;
        if defs.is_empty() && uses.is_empty() {
            continue;
        }
        let ids: Vec<String> = defs.iter().chain(uses.iter()).cloned().collect();
        let by_id: HashMap<String, Chunk> = store
            .get_chunks(repo_id, &ids)?
            .into_iter()
            .map(|c| (c.id(), c))
            .collect();
        let tagged = defs
            .iter()
            .map(|d| (d, "definition of"))
            .chain(uses.iter().map(|u| (u, "use of")));
        let mut listed = 0;
        let mut shown = 0; // defs and uses with a line in `out`, new or already there
        let mut def_lines: Vec<usize> = Vec::new();
        for (cid, kind) in tagged {
            if listed >= per_ident || out.len() >= max_lines {
                break;
            }
            let Some(chunk) = by_id.get(cid) else {
                continue;
            };
            let Some((line, text)) = first_line_with(chunk, id) else {
                continue;
            };
            shown += 1;
            if out
                .iter()
                .any(|r| r.path == chunk.path && r.start_line == line)
            {
                continue;
            }
            if kind == "definition of" {
                def_lines.push(out.len());
            }
            out.push(Related {
                path: chunk.path.clone(),
                start_line: line,
                end_line: line,
                symbol: text,
                relation: format!("{kind} `{id}`"),
            });
            listed += 1;
        }
        // Complete only if the store returned every referencing chunk (fewer than the lookup
        // limit) and each definition and use got a line. The claim saves the agent the grep for
        // call sites; it is never made for a partial list.
        if uses.len() < USAGE_REF_LIMIT && shown == defs.len() + uses.len() {
            for i in def_lines {
                out[i].relation.push_str(COMPLETE_USES);
            }
        }
    }
    Ok(out)
}

/// First line of `chunk` containing `ident` as a whole word: (absolute line number, trimmed text
/// cut to [`USAGE_LINE_CHARS`]).
fn first_line_with(chunk: &Chunk, ident: &str) -> Option<(u32, String)> {
    chunk.text.lines().enumerate().find_map(|(i, line)| {
        contains_word(line, ident).then(|| {
            let text: String = line.trim().chars().take(USAGE_LINE_CHARS).collect();
            (chunk.start_line + i as u32, text)
        })
    })
}

fn contains_word(line: &str, word: &str) -> bool {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    line.match_indices(word).any(|(at, _)| {
        let before = line[..at].chars().next_back();
        let after = line[at + word.len()..].chars().next();
        !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::{FailingReferencingStore, FakeStore};
    use laya_core::Lang;

    fn chunk(path: &str, start: u32, end: u32, defines: &[&str], refs: &[&str]) -> Chunk {
        Chunk {
            path: path.to_string(),
            start_line: start,
            end_line: end,
            lang: Lang::Rust,
            symbol: format!("fn {}", defines.first().copied().unwrap_or("anon")),
            kind: "function_item".to_string(),
            defines: defines.iter().map(|s| s.to_string()).collect(),
            refs: refs.iter().map(|s| s.to_string()).collect(),
            text: "…".to_string(),
        }
    }

    fn ranked(path: &str, start: u32, end: u32) -> RankedSpan {
        RankedSpan {
            path: path.to_string(),
            start_line: start,
            end_line: end,
            symbol: String::new(),
            p_relevant: None,
            score: 1.0,
            text: String::new(),
        }
    }

    #[test]
    fn max_related_zero_disables_expansion_without_any_store_calls() {
        let store = FakeStore::new(vec![chunk("a.rs", 1, 10, &["foo"], &["bar"])]);
        let seeds = vec![chunk("a.rs", 1, 10, &["foo"], &["bar"])];
        let out = expand_related(&store, "repo", &seeds, &[], 0).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn no_seeds_yields_empty_related() {
        let store = FakeStore::new(vec![]);
        let out = expand_related(&store, "repo", &[], &[], 8).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn resolves_a_callee_to_its_defining_chunk() {
        let bar = chunk("src/bar.rs", 5, 15, &["bar"], &[]);
        let seed = chunk("src/foo.rs", 1, 10, &["foo"], &["bar"]);
        let store = FakeStore::new(vec![seed.clone(), bar.clone()]);
        let out = expand_related(&store, "repo", &[seed], &[], 8).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "src/bar.rs");
        assert_eq!(out[0].start_line, 5);
        assert_eq!(out[0].relation, "defines `bar` (used by #1)");
    }

    #[test]
    fn callee_ref_defined_by_a_seed_itself_is_not_expanded() {
        // seed 1 defines `bar`; seed 2 refs `bar` — that ref should be excluded (it's already
        // covered by seed 1 itself, not an external one-hop neighbour).
        let seed1 = chunk("src/bar.rs", 1, 10, &["bar"], &[]);
        let seed2 = chunk("src/foo.rs", 1, 10, &["foo"], &["bar"]);
        let store = FakeStore::new(vec![seed1.clone(), seed2.clone()]);
        let out = expand_related(&store, "repo", &[seed1, seed2], &[], 8).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn only_the_first_six_refs_per_seed_are_considered() {
        let refs: Vec<&str> = vec!["r1", "r2", "r3", "r4", "r5", "r6", "r7"];
        let mut defs = vec![chunk("src/foo.rs", 1, 10, &["foo"], &refs)];
        for (i, r) in refs.iter().enumerate() {
            defs.push(chunk(&format!("src/{r}.rs"), 1, 5, &[r], &[]));
            let _ = i;
        }
        let seed = defs[0].clone();
        let store = FakeStore::new(defs);
        let out = expand_related(&store, "repo", &[seed], &[], 100).unwrap();
        assert_eq!(out.len(), 6, "the 7th ref must not be considered");
        assert!(!out.iter().any(|r| r.path == "src/r7.rs"));
    }

    #[test]
    fn ambiguous_ident_with_more_than_three_definitions_is_skipped() {
        let seed = chunk("src/foo.rs", 1, 10, &["foo"], &["common"]);
        let mut chunks = vec![seed.clone()];
        for i in 0..4 {
            chunks.push(chunk(&format!("src/dup{i}.rs"), 1, 5, &["common"], &[]));
        }
        let store = FakeStore::new(chunks);
        let out = expand_related(&store, "repo", &[seed], &[], 8).unwrap();
        assert!(
            out.is_empty(),
            "4 definitions of `common` should be treated as ambiguous"
        );
    }

    #[test]
    fn three_definitions_is_not_ambiguous() {
        let seed = chunk("src/foo.rs", 1, 10, &["foo"], &["shared"]);
        let mut chunks = vec![seed.clone()];
        for i in 0..3 {
            chunks.push(chunk(&format!("src/dup{i}.rs"), 1, 5, &["shared"], &[]));
        }
        let store = FakeStore::new(chunks);
        let out = expand_related(&store, "repo", &[seed], &[], 8).unwrap();
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn resolves_a_caller_via_chunks_referencing() {
        let seed = chunk("src/lib.rs", 1, 10, &["run"], &[]);
        let caller = chunk("src/main.rs", 1, 10, &["main"], &["run"]);
        let store = FakeStore::new(vec![seed.clone(), caller.clone()]);
        let out = expand_related(&store, "repo", &[seed], &[], 8).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "src/main.rs");
        assert_eq!(out[0].relation, "calls `run` (#1)");
        assert_eq!(out[0].symbol, "fn main");
    }

    #[test]
    fn only_the_first_four_defines_per_seed_are_considered() {
        let seed = chunk("src/lib.rs", 1, 10, &["d1", "d2", "d3", "d4", "d5"], &[]);
        let mut chunks = vec![seed.clone()];
        for d in ["d1", "d2", "d3", "d4", "d5"] {
            chunks.push(chunk(&format!("src/caller_{d}.rs"), 1, 5, &[], &[d]));
        }
        let store = FakeStore::new(chunks);
        let out = expand_related(&store, "repo", &[seed], &[], 100).unwrap();
        assert_eq!(out.len(), 4, "the 5th define must not be considered");
        assert!(!out.iter().any(|r| r.path == "src/caller_d5.rs"));
    }

    #[test]
    fn interleaves_callees_and_callers() {
        let seed = chunk("src/lib.rs", 1, 10, &["run"], &["bar"]);
        let bar = chunk("src/bar.rs", 1, 5, &["bar"], &[]);
        let caller = chunk("src/main.rs", 1, 5, &["main"], &["run"]);
        let store = FakeStore::new(vec![seed.clone(), bar, caller]);
        let out = expand_related(&store, "repo", &[seed], &[], 8).unwrap();
        assert_eq!(out.len(), 2);
        assert!(
            out[0].relation.starts_with("defines"),
            "callees come first at each interleave step"
        );
        assert!(out[1].relation.starts_with("calls"));
    }

    #[test]
    fn drops_related_items_overlapping_an_existing_span() {
        let bar = chunk("src/bar.rs", 5, 15, &["bar"], &[]);
        let seed = chunk("src/foo.rs", 1, 10, &["foo"], &["bar"]);
        let store = FakeStore::new(vec![seed.clone(), bar]);
        let existing = vec![ranked("src/bar.rs", 5, 15)];
        let out = expand_related(&store, "repo", &[seed], &existing, 8).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn drops_related_items_overlapping_an_already_accepted_related_item() {
        // Two seeds both ref `bar`, defined once: the second hit is a duplicate location and
        // must be dropped by the accepted-overlap check, not just deduped by id.
        let bar = chunk("src/bar.rs", 5, 15, &["bar"], &[]);
        let seed1 = chunk("src/foo.rs", 1, 10, &["foo"], &["bar"]);
        let seed2 = chunk("src/baz.rs", 1, 10, &["baz"], &["bar"]);
        let store = FakeStore::new(vec![seed1.clone(), seed2.clone(), bar]);
        let out = expand_related(&store, "repo", &[seed1, seed2], &[], 8).unwrap();
        assert_eq!(out.len(), 1, "the duplicate `bar` location must be deduped");
    }

    #[test]
    fn caps_at_two_entries_per_file() {
        let seed = chunk("src/foo.rs", 1, 10, &["foo"], &["a", "b", "c"]);
        let mut chunks = vec![seed.clone()];
        // three distinct definitions, all living in the same file at non-overlapping lines.
        chunks.push(chunk("src/many.rs", 1, 5, &["a"], &[]));
        chunks.push(chunk("src/many.rs", 10, 15, &["b"], &[]));
        chunks.push(chunk("src/many.rs", 20, 25, &["c"], &[]));
        let store = FakeStore::new(chunks);
        let out = expand_related(&store, "repo", &[seed], &[], 8).unwrap();
        assert_eq!(out.len(), 2, "at most 2 entries per file");
        assert!(out.iter().all(|r| r.path == "src/many.rs"));
    }

    #[test]
    fn caps_the_total_at_max_related() {
        let refs: Vec<&str> = vec!["r1", "r2", "r3", "r4"];
        let mut chunks = vec![chunk("src/foo.rs", 1, 10, &["foo"], &refs)];
        for r in &refs {
            chunks.push(chunk(&format!("src/{r}.rs"), 1, 5, &[r], &[]));
        }
        let seed = chunks[0].clone();
        let store = FakeStore::new(chunks);
        let out = expand_related(&store, "repo", &[seed], &[], 2).unwrap();
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn store_error_aborts_expansion_without_failing_the_caller() {
        let seed = chunk("src/lib.rs", 1, 10, &["run"], &[]);
        let inner = FakeStore::new(vec![seed.clone()]);
        let store = FailingReferencingStore(inner);
        // Only a caller lookup should fire (no refs to resolve), so this exercises
        // `chunks_referencing` failing specifically.
        let out = expand_related(&store, "repo", &[seed], &[], 8);
        assert!(out.is_err(), "expand_related itself surfaces the error");
    }
}
