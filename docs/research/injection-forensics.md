# Injection forensics: why laya's context does not cut more reading, turns and time

Date: 2026-09-23. Data: the v1–v3 Claude Code benchmark transcripts (20 held-out moon tasks per arm,
single prompt; v6 partial, 6 tasks), the dev-set retrieval dumps in `bench/results/dev-dumps/`,
and the moon checkout. Everything is offline: no laya, daemon or Claude calls.
Tool: `bench/forensics.py` (reusable; commands in §9).

**Where we are (v3, laya-refs vs baseline, from `docs/RESULTS.md`):** −44.7% code-reading tokens and
−24.6% wall-clock. **Targets:** −50% and −30%. Measured from session durations
(`result.duration_ms`), which is what this study uses, the gaps per task are:
reading 6,684 → ≤6,045 tokens (−640 more) and wall 24.8 s → ≤23.4 s (−1.35 s more).

## 1. Summary

1. **Claude does not ignore the injection. It reads past it.** Re-reading inlined code is only 2.6–3.6% of
   reading tokens (0.05 calls per task). The rest goes to code the injection did not contain: gold files
   the retriever missed (27%), other regions of files the map listed (21%), ±100 lines around an inlined
   chunk (14%), and exploration (12%).
2. **Whole-file Reads are about half of what is left.** In laya-refs, 0.55 whole-file Reads per task carry
   49% of Read tokens (2,728 tokens per task). 8 of 11 are gold files. The Read-narrowing hook fired on
   0 of 11 (`not_narrowed`), because these files were not in the ranking at p ≥ 0.4.
3. **Retrieval misses are the largest token class and the cheapest for Claude to fix.** 44% of gold
   files were absent from the v3 injection, and 5 of 20 tasks had no gold file at all. Claude recovered
   every missed file with one Grep in turn 1, by turning phrases into identifiers
   (`mmap budget` → `MmapBudget|mmap_budget`). A miss costs tokens (a whole-file Read) but almost no
   turns (the oracle saves 0.25 calls per task).
4. **v3 retrieval was contaminated by the benchmark prompt's boilerplate.** `examples/rag-quickstart/rag_quickstart.py`
   was in 18 of 20 ranked lists and was inlined in 11 of 20. The stoplist fix (commit 1b0ba87) landed
   *after* v3. In v6 (partial) the RAG spans are gone, but gold coverage is almost unchanged on 6 tasks.
5. **Inlining more spans is not the lever.** Top-5/6/8/10 would save 126–312 reading tokens per task and
   0 turns, while adding 1.0–3.7k injected tokens. Inlining "Related by references" saves 178 and costs
   3,029. Both are net negative.
6. **Time depends on turns and output, not on reading.** A linear fit over 180 sessions is
   wall ≈ 1.1 + 0.28·calls + 8.9·(output ktok) + 1.6·(cache-read 100k), with R² = 0.90. The final
   answer turn alone takes 9.2–9.6 s in every arm (28–37% of wall), and laya cannot reduce it. To reach
   −30% overall, the rest of the session must drop by 42%.
7. **The largest time sink laya can remove is Claude locating identifiers it was already shown.**
   `grep-locate` takes 0.95 turns and 3.9 s per task in laya-refs. Examples: serial Greps for the call
   sites of `recover_shard_v3`, or for `maybe_force_checkpoint_on_wal_overflow`. 9 of 28 of these Greps
   name an identifier that appears verbatim in the task text.
8. **The cold Laya call adds 1.1 s, and the benchmark only partly counts it.** The UserPromptSubmit hook
   takes about 1,100 ms when Laya scores for the first time and about 10 ms when the store's probability
   cache hits. In v3, the first arm to run each task paid this in 20 of 20 tasks (refs 14 times, compact
   6 times). In v2, no arm paid it. In real use every new prompt is cold.
9. **The v1 "full injection" arm was not a size test.** 16 of 20 v1 injections were over 10,000
   characters. Claude Code stores hook output that large in a file and shows only a preview. Claude
   Read that file in 7 sessions, which cost 887 tokens per task (14% of v1 laya reading).
   `INJECT_TOKENS = 3500` still allows about 12,250 characters, and the Related section is appended
   outside the budget: one v3 refs injection reached 10,164 characters.
10. **Noise is large.** v2 and v3 laya-compact received identical ranked lists in 19 of 20 tasks, yet
    measured −47%/−23% and −44%/−15% (reading/wall). An 8-point wall swing between identical arms means
    the refs-vs-compact wall gap (−26% vs −15%) is inside replicate noise.

## 2. Method

**Parsing** (`bench/forensics.py`): each transcript is stream-json with `--include-hook-events`.
- The injection is the `additionalContext` of each UserPromptSubmit `hook_response`. From it the tool
  parses the inlined spans (`### path:a-b`), the "Ranked locations" map, "Related by references", and
  the identifier set (identifiers in inlined code plus symbols and file stems in the map and related
  lists).
- Tool calls are joined to their results by `tool_use_id`.
- A **turn** is one API call (a distinct `message.id`). Turn time is the gap between the end of one
  turn's tool results and the end of the next turn's, from event timestamps. Whatever
  `result.duration_ms` does not explain is assigned to turn 1.
- Token estimate is characters / 3.5, the same as `run_bench.py`. Per-class totals add up to
  `reading_tokens`.

**Read classes.** Classification is per returned line. Read results carry `startLine`/`numLines`, so
offset/limit semantics are exact. Absolute paths are made repo-relative. Priority order:
a redundant (inside an inlined span) > d expansion (same file, within ±100 lines of an inlined span) >
b map-hit (inside a listed map or related range) > b2 map-file (listed file, outside its ranges) >
c gold-miss (gold file the injection does not mention) > f other > h hook-file (the persisted hook
output).

**Search classes.** A Grep counts as e grep-locate if its pattern contains a "strong" identifier
(snake_case, CamelCase, or at least 8 characters) that also appears in the injection's identifier set.
Otherwise it is g grep-explore. Glob is always explore.

**Baseline** sessions are classified against the injection laya-refs received for the same task. For
example, their "a redundant" means baseline tokens spent on code laya would have inlined.

**Validation by eye.** The per-call dumps of these sessions were checked against `show_session.py`
timelines:
- 756db483ef refs: 3 gold Reads classified c; Greps classified g.
- 192d1c0cc7 refs: a Read split d/a, `maybe_force_checkpoint_on_wal_overflow` classified e, event_loop windows classified b2.
- 46a1e8b7bb refs: a/d/b split.
- a4964139c8 refs and baseline.

**Edge cases handled:**
- Errored or unparseable Reads (1) fall to f.
- Grep content-mode results with and without path prefixes are handled.
- Reads of `tool-results/hook-*` go to class h.
- Multi-prompt sessions (v6) merge injections; the first prompt is used for coverage.
- No transcript was compacted: every session is under 25 API calls.

**Limits:**
- n = 20 per arm.
- The map's rank order is file-grouped (first-seen), not strict score order.
- The enclosing-item boundary is a brace/indent heuristic.
- What Claude saw of an over-cap hook output (the preview) is not in the transcript.

## 3. Where the reading goes (v3, mean per task)

| class | baseline tok | laya-compact tok | laya-refs tok | refs % | refs calls | refs turns | refs s |
|---|---|---|---|---|---|---|---|
| a redundant (inlined code re-read) | 365 | 179 | 241 | 3.6% | 0.05 | 0.00 | 0.0 |
| d expansion (±100 lines of inlined chunk) | 1,296 | 785 | 959 | 14.4% | 0.60 | 0.45 | 2.3 |
| b map-hit (listed range, not inlined) | 276 | 396 | 387 | 5.8% | 0.30 | 0.20 | 0.9 |
| b2 map-file (listed file, other lines) | 3,194 | 1,818 | 1,373 | 20.5% | 0.65 | 0.45 | 2.2 |
| c gold-miss (gold file not in injection) | 2,114 | 1,574 | 1,790 | 26.8% | 0.60 | 0.50 | 2.5 |
| f other exploration | 3,131 | 765 | 819 | 12.3% | 0.50 | 0.30 | 0.9 |
| e grep-locate (identifier already shown) | 812 | 635 | 443 | 6.6% | 1.40 | 0.95 | 3.9 |
| g grep-explore | 902 | 637 | 671 | 10.0% | 1.50 | 0.65 | 3.0 |
| final answer turn | – | – | – | – | – | 1.00 | 9.2 |
| **total** | **12,090** | **6,791** | **6,684** | | | **4.50 calls** | **24.8 s** (baseline 33.5, 7.15 calls) |

The v2 replicate (same laya-compact injections) has the same shape. Totals are 5,525 tokens per task,
with c 1,773, b2 1,426, d 702, e 425 and g 375.

**What baseline spent, measured against laya-refs' injection:**
- 42% of its reading (5,131 tokens) went to files the injection named: a 365, d 1,296, b 276, b2 3,194.
- 17.5% went to gold files the injection missed.
- 26% went to other files.
- laya removes most of the "other" (3,131 → 819) and about half of the map-file reading. It does not
  remove the gold-miss reading.

**Whole-file Reads (no offset/limit):**

| arm | calls/task | tokens/task | share of Read tokens |
|---|---|---|---|
| baseline | 1.15 | 7,436 | 72% |
| laya-compact | 0.50 | 2,444 | 44% |
| laya-refs | 0.55 | 2,728 | 49% |

In refs, whole-file Reads split by class as gold-miss 26.9k, map-file 17.4k and other 10.3k tokens
(summed over the 20 tasks). The PreToolUse narrowing hook logged `not_narrowed` on all 11.

## 4. Gold coverage, answer containment, discovery timing

| arm | gold inlined | map only | related only | absent | tasks with 0 gold | FILES ⊆ injection | FILES ⊆ inlined files |
|---|---|---|---|---|---|---|---|
| v3 laya-compact | 14/34 (41%) | 3 | 0 | 17 (50%) | 5/20 | 5/20 | 2/20 |
| v3 laya-refs | 14/34 (41%) | 3 | 2 | 15 (44%) | 5/20 | 5/20 | 2/20 |
| v1 laya (full) | 16/34 (47%) | – | – | 18 (53%) | 6/20 | 3/20 | 3/20 |

**Dev-set holdout** (60 tasks, 113 gold files, shipped post-stoplist retriever, benchmark-wrapped
prompt):
- Files of the 3 inlined spans: 0.41 recall.
- Top-5 ranked files: 0.55.
- The whole map plus related: 0.64.

Retrieval often finds the gold file but does not inline it.

**Discovery timing.** "Last gold" is the API call at which the last new gold file was first Read
(0 = inlined).

| arm | first gold Read | last new gold | calls | calls after last gold (incl. answer) | seconds after | share of wall |
|---|---|---|---|---|---|---|
| baseline | 4.1 | 2.4 | 6.9 | 4.6 | 24.4 | 75% |
| laya-compact | 2.6 | 1.5 | 5.2 | 3.7 | 21.4 | 74% |
| laya-refs | 2.6 | 1.6 | 4.5 | 3.0 | 17.5 | 71% |

The tail after the last gold discovery, excluding the answer turn, is 1.95 calls and 8.4 s for refs.
It is not waste in any simple sense: 26 of 26 tail Reads in refs, and 34 of 35 in compact, are of
files Claude then names in FILES. These are secondary files and gold-region expansion; answer
precision is 0.42.

**Coverage strata** (pooled compact sessions, each paired with its same-run baseline):

| gold in injection | n | reading ratio vs baseline | calls (laya / base) | wall ratio |
|---|---|---|---|---|
| all gold inlined | 21 | 0.39 | 4.4 / 6.0 | 0.83 |
| some gold inlined | 18 | 0.48 | 5.1 / 7.5 | 0.89 |
| gold listed only | 6 | 0.54 | 4.0 / 7.7 | 0.64 |
| no gold | 15 | 0.88 | 5.7 / 7.7 | 0.68 |

Coverage drives reading tokens strongly. Its effect on turns and wall is weak and noisy.

## 5. Size ablation, de-confounded as far as the data allows

- **The v1 "full" arm mostly never delivered its full text.**
  - Injections averaged 10.8k characters; 16 of 20 were over Claude Code's 10,000-character limit for
    hook output.
  - Every one of the 7 Reads of `~/.claude/projects/.../tool-results/hook-*-additionalContext.txt` is
    in a session whose injection was over 10k. None happens below it.
  - So "full versus compact" in v1→v2 compares "preview plus a file pointer (plus an optional
    3.4k-token Read)" against a fully visible 1.6k compact injection. The −6% → −21% wall change in
    `RESULTS.md` §5 cannot be credited to size.
- **Controlling for coverage stratum:**
  - All gold inlined: full 0.44 vs compact 0.39 reading ratio, and 0.89 vs 0.83 wall ratio.
  - Some gold inlined: 0.54 vs 0.48.
  - Class by class, the full arm re-read less inlined code only because it inlined more (a 252 vs 179).
    Its hook-file reading (887 tokens per task) cancels any gain.
- **Visible full injections** (v1, ≤10k characters) number only 4 per arm. That is too few to estimate
  a pure size effect.
- **Conclusion:** the data cannot separate size from visibility. The only clean statement is that a
  hidden injection is worse than a compact visible one.
- **A clean size test** needs arms at 1.6k, 2.6k and 3.4k tokens, all under 10k characters, on a
  cold Laya cache.

## 6. Counterfactual estimates (per task; they reuse the classified calls, and are estimates, not measurements)

Rules:
- A turn counts as removed only if every call in it is removed.
- Removed seconds are the measured cycle times of those turns.
- Added injection tokens are priced as one cache write plus a cache read on each remaining turn.

| option | reading tokens saved (refs / v3 compact / v2 compact) | calls saved | seconds saved | injected tokens added |
|---|---|---|---|---|
| inline top-5 spans | 126 / 181 / 124 | 0.00–0.05 | 0.0–0.1 | +1,001 |
| inline top-8 spans | 194 / 277 / 209 | 0.00–0.05 | 0.0–0.1 | +2,701 |
| inline top-10 spans | 312 / 432 / 298 | 0.00–0.10 | 0.0–0.2 | +3,689 |
| inline "Related" ranges | 178 / – / – | 0 | 0 | +3,029 |
| enclosing fn/item instead of 10–50-line chunk | 393 / 326 / 285 | 0.00–0.05 | 0.0–0.3 | +384 |
| narrow whole-file Reads of files over 250 lines to about 150 lines (assumes half need one follow-up Read) | **1,598 / 1,323 / 1,216 net** | −0.17 to −0.23 (added) | −0.6 to −0.8 (added) | 0 |
| usage list (definition + call sites) for identifiers named in the task | 198 / 282 / 199 | **0.30–0.40** | **1.1–1.5** | +198 to +282 |
| upper bound: all grep-locate answered by the injection | 443 / 635 / 425 | 0.85–0.95 | 3.1–3.9 | about the same as saved |
| oracle retrieval (no gold-miss lines) | 1,790 / 1,574 / 1,773 | 0.15–0.30 | 0.5–0.8 | – |
| upper bound: stop at last gold discovery | 2,768 / 2,953 / 2,186 | 1.95–2.60 | 8.4–11.3 | – (quality risk, §4) |

Two further estimates.

**Realistic retrieval.**
- Offline, compound-identifier synthesis plus a path-word prior (method in §9) put 8 of the 15
  absent v3 gold files in its top-5. That result is in-sample and optimistic.
- On the dev holdout it found only 6 of 113 gold files that the shipped map lacked.
- Fusing laya's 3 inlined files with the synthesis top-2 raised inlined-file recall from 0.41 to 0.48.
- Inlining the top-3 *distinct files* instead of the top-3 spans raised it to 0.46–0.52 across the
  three prompt templates, at zero token cost. On the v3 test set it moved 14 to 15 of 34.
- Estimate: 15–25% of the gold-miss class is recoverable, about −250 to −450 reading tokens per task.
  Effect on turns and time is about 0.

**Cold Laya latency.** 1.1 s per uncached prompt; in v3 refs this averaged 0.77 s per task.
Removing it from the critical path is worth −1.1 s per prompt in real use, which is 3.3 points of
the wall target.

## 7. Annotated sessions (v3 unless noted)

**A. Boilerplate leak, then a one-Grep recovery (756db483ef, "P5 — three post-review issues in mmap budget").**
The injection (1.5k tokens) inlined `rag_quickstart.py:239-281` (`search_documents`, `format_context`),
`fwht.rs:1-50` and `rag_quickstart.py:1-27`. It contained no gold file.
```
t1 Grep(mmap.*budget|budget.*mmap|MmapBudget|mmap_budget)  -> 38 files incl. all 3 gold   [g explore]
t2 Read(src/vector/persistence/mmap_budget.rs)  whole file, 516 lines, 6,246 tok           [c gold-miss]
t5 Read(src/vector/store.rs 1040-1179)           1,802 tok                                  [c gold-miss]
```
10k reading tokens and 7 calls, against 10.6k for baseline. After the stoplist fix (v6),
mmap_budget.rs is listed at rank 4 but still not inlined.

**B. The injection works (46a1e8b7bb, "maybe_force_checkpoint_on_wal_overflow for P6 ceiling trigger").**
The gold `persistence_tick.rs` was inlined. Claude made one turn with two ranged Reads
(an a/d split around the chunk, plus event_loop 2085-2154), then answered: 2 calls, 15 s, 2.6k tokens.
Baseline took 5 calls, 29 s and 7.1k tokens, and spent its first 2 turns Grepping
`force_checkpoint|overflow`.

**C. Gold inlined, but a serial call-site chain (a4964139c8, "wire recover_shard_v3 to honor target_lsn").**
The injection inlined `recovery.rs`. Claude then made 5 grep-locate turns
(`recover_shard_v3\b|…_pitr`, `target_lsn|pitr`, `recovery_target_lsn|restore_from_persistence`)
interleaved with Reads of `shard/mod.rs` and `config.rs`: 10 calls and 55 s, against 8 calls and
35 s for baseline. Each hop depends on the previous result. A definition-plus-usages list for
`recover_shard_v3` and `target_lsn`, both named in the task, would have collapsed the chain.

**D. Over-cap injection, junk in the inlined slots, and Reads outside the listed ranges (192d1c0cc7).**
The refs injection was 10,164 characters, just over the cap. Two of the 3 inlined spans were RAG
examples. The gold `event_loop.rs` was listed at 460-549 and 2085-2154. Claude grep-located
`maybe_force_checkpoint_on_wal_overflow`, then Read event_loop 1340-1459 and 2020-2169, where the
two arms actually are (class b2).

**E. v1 persisted hook output (d757a7aa25, laya arm).** The injection was 12,145 characters, so
Claude Code stored it. Claude's first action was `Read(.../tool-results/hook-…-additionalContext.txt)`,
paying again, as a tool result, for context laya had already produced.

## 8. Ranked optimisations

Expected impact is measured against the targets: reading −640 tokens per task and wall −1.35 s per
task (v3 refs basis).

| # | optimisation | evidence | est. reading Δ/task | est. wall Δ/task | confidence | validate |
|---|---|---|---|---|---|---|
| 1 | **Hard cap at about 9,500 characters for all hook output**, with the Related section counted inside the budget (today `INJECT_TOKENS = 3500` allows about 12.2k characters and Related is appended after the budget check) | v1: 16/20 over the cap, 7 Reads of the persisted file, 887 tok/task; v3: 1/20 over | protects against a +0.9k regression | protects | high | unit test on `render_compact_opts` output length; run `forensics.py` and check for 0 `h_hook_file` |
| 2 | **Outline-narrow whole-file Reads** of files over 250 lines that the ranking cannot narrow: return an item outline (fn/struct and line ranges) plus the best region, and tell Claude to Read again for the full file | whole-file Reads are 49% of refs Read tokens, 11/11 `not_narrowed` | **−1,200 to −1,600 net** (replicated in 3 arm-runs) | +0.6 to +0.8 (penalty if half re-read) | medium: the follow-up rate is unknown | offline: outline size per file; small benchmark (10 tasks × 2 reps) measuring follow-up-Read rate and recall |
| 3 | **Usage list for task identifiers** (explicit identifiers in the prompt, plus symbols defined in the top spans): `path:line: text` for the definition and call sites, capped at about 300 tokens | grep-locate: 0.95 turns, 3.9 s per task; 21/28 results ≤400 tokens; 9/28 name a task identifier | +0.1 to +0.3k injected (about neutral) | **−1.1 to −1.5** (upper bound −3.9) | medium | offline: for each e-class Grep, check whether its result is a subset of the precomputed list; then a 20-task A/B |
| 4 | **Take cold Laya scoring off the critical path**: inject lexical results immediately, or cap Laya at a sub-200 ms budget. For benchmark hygiene, clear or pre-warm the probability cache for every arm | hook 1,104–1,217 ms cold vs 4–30 ms cached; v3 first arm cold in 20/20 tasks | 0 | **−1.1 per prompt in real use** (−0.77 in v3 refs) | high on cost; medium on the quality trade (Laya vs lexical is within noise in RESULTS §5) | re-run `eval_retrieval.py score` with a small budget; benchmark with the cache cleared per session |
| 5 | **Inline the top-3 distinct files, not the top-3 spans**, plus compound-identifier/path-word candidates fused into the top-2 | dev holdout: inlined-file recall 0.41 → 0.46–0.52; fusion 0.48 | −250 to −450 | ≈0 | medium-low on magnitude | `eval_retrieval.py` with a new `R@inlined-files` metric on the 60 dev tasks, then `forensics.py` c-class on a rerun |
| 6 | Enclosing fn/item spans instead of 10–50-line chunks | d-class 14%; saves 285–393 for +384 injected | −300 (reading-only metric) / ≈0 (reading+injected) | ≈0 | medium | offline via `forensics.py --counterfactual` |
| 7 | Do **not** inline more spans or the Related ranges | top-5..10 save ≤432 for +1.0–3.7k; Related saves 178 for +3k; 0 turns | net worse on reading+injected and cost | 0 | high | – |
| 8 | Nudge the stop condition (footer: "the listed spans are the implementation; read further only for callers or tests not listed") | tail is 1.95 calls / 8.4 s, but 26/26 tail Reads are of files later named in FILES | up to −2.8k | up to −8 | low (quality risk) | A/B with a recall/precision guard, n ≥ 40 |

**Putting it together (estimates; the v3 refs basis, with replicate noise of about ±8 wall points):**
- **Reading tokens:**
  - #2 + #5 (#6 optional) → about 6,684 − 1,400 − 350 ≈ **4,900–5,500 tokens per task (−55% to −60%)**.
  - The −50% target looks reachable. #2 carries most of the gain, and its uncertainty is the
    follow-up-Read rate.
- **Wall-clock:**
  - #3 (−1.3 s) + #4 (−0.8 s in benchmark terms, −1.1 s in production) − #2's penalty (+0.7 s) →
    about **−1.4 s**, i.e. −29% to −31%.
  - That is at the edge of the target, and inside the noise band.
  - Getting clearly past −30% needs the answer and thinking floor (9.2 s of output-bound generation per
    task, which applies to all arms) or the tail (#8), and both carry quality risk.

**Required before re-benchmarking:**
- Fix #1 and #4's cache confound.
- Use 2 repetitions per task (or 40+ tasks).
- Make every arm cold, or every arm warm.

Without these, a −30% versus −25% difference is not resolvable.

## 9. Reproduce

```
python3 bench/forensics.py <bench>/claude-v3 --counterfactual --json /tmp/fx_v3.json
python3 bench/forensics.py <bench>/claude-v2 --counterfactual
python3 bench/forensics.py <bench>/claude-v1          # h_hook_file class = persisted hook output
python3 bench/forensics.py <bench>/claude-v6          # partial, 2-prompt sessions
```
`--ref-arm` picks the laya arm that baseline calls are classified against (default: laya-refs,
then laya-compact, then laya). `--repo` points at the moon checkout used to size unlisted spans.

The following side analyses were one-off scripts and are not committed; each is described here so it
can be rebuilt:
- Hook latency: `hooklogs/*.jsonl` `elapsed_ms` of the UserPromptSubmit event, compared with which arm ran first.
- The wall regression.
- The dev-set synthesis check: candidates are `--flags`, hyphenated words, dotted calls, explicit
  snake_case, and adjacent non-stopword pairs as snake_case/CamelCase. Scoring is idf-weighted
  definition (×2), basename (×3) and mention (×0.3), plus 0.5 × a path-component idf prior.
