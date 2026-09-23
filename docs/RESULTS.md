# laya-codex — results (2026-09-23)

All numbers are reproducible from this repo; raw per-run rows are in `bench/results/`.
Hardware: Apple M4 Pro, 24 GB. Agent: Claude Code 2.1.280, model `sonnet`, isolated from user
settings/plugins/MCP (`--setting-sources project --strict-mcp-config`), tools Read/Grep/Glob.

## 1. The question

Does giving Claude Code pre-ranked code spans (tree-sitter chunks → Moon BM25 → Laya re-rank,
injected by hooks) cut codebase-reading tokens by 50% and task time by 30% without hurting
answer quality?

## 2. Benchmark design

- **Repo**: a clean clone of `pilotspace/moon` (Rust, 783 source files, 8,842 chunks).
- **Tasks**: 20 held-out commits (`bench/tasks.jsonl`), each "find and explain the code for
  <commit subject>, end with `FILES: …`". Gold = files the commit touched. Not used for any
  tuning: tuning used a disjoint 60-commit dev set (`bench/dev_tasks.jsonl`); moon was
  excluded from Laya fine-tuning data.
- **Arms** (same tasks, interleaved, random order): `baseline` (stock Claude Code) vs laya-codex
  variants. Every run records tokens, wall-clock, turns, cost, and graded FILES recall/precision.
- **Stats**: paired bootstrap (10k resamples) 95% CI of the ratio of sums (`bench/stats.py`).

## 3a. Headline result: v7, the v0.1.0 defaults (two prompts per session)

These are the v0.1.0 defaults: adaptive injection (`laya-adaptive`). Tasks are the same 20; each
session has two prompts (localisation, then tests and call sites). Laya is scored **cold** in
every arm (`LAYA_MEMO=0`) and the model is warmed up at daemon start. Raw rows are in
`bench/results/claude-v7/`.

| metric (vs stock Claude Code) | v7 adaptive (default) | v7 laya-refs | 95% CI (adaptive) | tasks improved |
|---|---|---|---|---|
| code-reading tokens | **−50.1%** | −48.5% | [−61.6%, −33.0%] | 17/20 |
| reading + injected tokens | −27.9% | −25.8% | [−43.8%, −4.2%] | 13/20 |
| total input tokens | −26.8% | −28.5% | [−42.2%, −6.4%] | 14/20 |
| wall-clock | **−17.4%** | −16.6% (n.s.) | [−31.5%, −1.0%] | 13/20 |
| turns | −23.4% | −24.9% | [−34.1%, −10.5%] | 15/20 |
| cost | −27.2% | **−34.7%** | [−39.2%, −11.1%] | 13/20 |
| answer recall (turn 1) | 0.933 vs 0.975 | 0.933 | diff −0.042 [−0.108, 0.000] | |
| answer recall (both turns) | 0.933 vs 0.975 | 0.958 | diff −0.017 [−0.092, +0.058] | |

Read accuracy (`bench/read_accuracy.py`):

| arm | read precision | gold code seen | wasted read tokens | first gold Read at turn |
|---|---|---|---|---|
| baseline | 0.373 | 0.871 | 4,993 | 8.2 |
| **adaptive** | **0.546** | **0.933** | **2,118** | 4.0 |
| refs | 0.482 | 0.871 | 2,569 | 3.2 |

Mechanics (`bench/check_v7.py`):
- 80 prompt injections, max 8,303 characters, none over the 9,500 cap.
- 70 of 80 carried the "Definitions and uses" list.
- 6 whole-file Reads were narrowed to a region. Claude later read other ranges of those files,
  and never the whole file.

What moved from v6 to v7:
- Wall-clock went from −9% to −17%, code reading from −39% to −50%, and cost from −19% to
  −27%, even though v7 scores every prompt cold, which v6 did not.
- The forensics-driven changes were:
  - the character cap;
  - distinct-file inlining;
  - the definitions and uses list;
  - the Read plan;
  - the model warm-up.

Recall dipped by 2 tasks (−0.042, CI touching 0). In one of them (756db483ef) both laya arms
missed the third gold file, `warm_search.rs`. Treat that one as a possible systematic miss, not
proven noise.

## 3. Earlier headline: v3 (single prompt, compact injection + one-hop reference expansion)

`bench/results/claude-v3/stats-laya-refs.txt` — laya-refs vs baseline, paired n=20:

| metric | change | 95% CI | lower in |
|---|---|---|---|
| code-reading tokens (Read/Grep/Glob output) | **−44.7%** | [−59.4%, −26.0%] | 17/20 |
| reading + injected tokens | **−29.6%** | [−47.3%, −7.0%] | 14/20 |
| total input tokens (all turns) | **−42.0%** | [−55.0%, −24.4%] | 18/20 |
| wall-clock | **−24.6%** | [−41.8%, −4.0%] | 13/20 |
| turns | **−36.5%** | [−48.5%, −23.7%] | 17/20 |
| cost (USD) | **−28.6%** | [−38.9%, −16.5%] | 17/20 |
| answer recall / precision | **0.950 / 0.420** vs 0.938 / 0.337 | | |

Every change is significant (CI excludes 0), and answer recall is no longer below baseline.

### Read accuracy vs the stock Read tool (`bench/read_accuracy.py`, v3)

| arm | Read calls | read precision | read recall (gold code seen) | wasted read tokens | first gold Read (turn) | runs reading a gold file |
|---|---|---|---|---|---|---|
| baseline | 3.15 | 0.550 | 0.767 | 3,775 | 7.7 | 18/20 |
| laya-compact (no refs) | 3.15 | 0.556 | 0.771 | 1,891 | 4.4 | 17/20 |
| **laya-refs** | **2.70** | **0.657** | **0.871** | **1,740** | **4.5** | **20/20** |

laya-codex gets the agent to relevant code ~3 turns earlier and halves tokens wasted on
irrelevant files; the reference edges are what raise how much of the relevant code it sees.

## 3b. Multi-turn sessions — v6 (two prompts per session)

Each session asks the v3 localisation prompt, then a follow-up in the same session ("Now, for the
same change, identify the tests that cover this code and the main call sites that invoke it …").
The tasks are the same 20 and the runs are paired. Arms: `laya-refs` (v3 injection with the
retrieval fixes below) and `laya-adaptive` (the same, plus session delta: code already sent or
read is not re-sent, and the next-ranked spans take its place).

| metric (vs baseline) | laya-refs | laya-adaptive |
|---|---|---|
| code-reading tokens | **−33.7%** [−50.6, −15.7] | **−39.4%** [−52.6, −25.8] |
| reading + injected | −10.4% [−28.8, +9.4] | −17.4% [−33.3, +0.3] |
| total input tokens | −4.7% [−28.4, +25.3] | −17.1% [−34.5, +4.5] |
| wall-clock | −9.2% [−24.2, +9.8] | −9.6% [−23.5, +9.5] |
| turns | −12.2% [−25.9, +4.7] | **−19.7%** [−31.5, −6.1] |
| cost | **−21.5%** [−36.8, −3.8] | **−18.8%** [−32.4, −4.1] |
| answer recall (base 0.925) | 0.933 | 0.821 |
| read precision / gold seen (base 0.42 / 0.81) | 0.48 / 0.91 | 0.52 / 0.85 |
| first gold Read at turn (base 8.6) | 5.4 | 4.7 |

- **Reading, turns and cost fall significantly; wall-clock does not.** Across two prompts, time
  is dominated by turns, output and the final answer turn (9–10 s each), not by reading.
- **Adaptive's lower answer recall is run noise.** In v6 both laya arms got identical turn-1
  injections, yet they disagree on 5 tasks in both directions. The misses are wiring files
  (`mod.rs`, `main.rs`) that Claude sometimes leaves out of its FILES line.
- **Laya's scoring cache inflated some arms' speed in v3 and v6.** Whichever arm ran a task first
  paid the ~1.1 s model run, and later arms got cache hits. From v7 every arm scores cold
  (`LAYA_MEMO=0`), which matches real use.

### What changed between v3 and v6 (and why v4/v5 were stopped)

- **Retrieval defect (fixed, 1b0ba87, 2fbdd74).**
  - The benchmark's instruction wrapper ("find the source code…", "comma-separated repo-relative
    paths") beat the task words in the rarest-first BM25 term selection. Dev-set MRR fell from
    0.724 (bare task) to 0.394 (wrapped).
  - A prompt stoplist restores 0.541 on wrapped prompts with no loss on bare ones. A broader list
    scored 0.618 but dropped real code words ("fast-path").
  - Follow-ups are detected by explicit back-references ("same", "above", …). They are retrieved
    as the session topic plus any identifiers or paths they name.
  - v4 and v5 ran with the defect in place and were stopped (19/60 and 10/60 runs).
- **Confidence-sized context lost to rank.** Laya's P scale shifts with prompt wording. On
  wrapped prompts every P threshold covered fewer gold files with full code than the fused rank
  at equal code volume, so adaptive sizing is rank-based by default.
- **Scope classifier is effectively a no-op.** Zero-shot Laya can't separate function / file /
  module / cross-cutting scope (best macro-F1 0.28, bar 0.45; a bag-of-words baseline gets
  0.29–0.35). Even oracle scope barely changes what is loaded. It stays wired and gated but
  does nothing; the fine-tune was not run.
- **Fusion weight.** P-weight 0.5 beats 0.7 and Laya-only on the full pipeline (MRR 0.724 vs
  0.678 / 0.602), because the lexical rank is what demotes prose.

### Why Claude still reads after the injection (`docs/research/injection-forensics.md`)

- Claude re-reads only 3.6% of what laya inlined. Its remaining reading goes to:
  - gold files retrieval missed (27%);
  - other ranges of files in the map (21%);
  - within ±100 lines of an inlined chunk (14%).
- Whole-file Reads are 49% of the remaining Read tokens.
- Inlining more spans is net negative: it saves 126–312 tokens and 0 turns, and costs 1–3.7k.
- The largest removable time sink is Grepping for identifiers Claude was already shown: about
  one turn and 3.9 s per task.
- v1's "full injection" arm exceeded Claude Code's 10,000-character hook limit in 16 of 20
  injections, so Claude saw a preview and read the overflow file back.

**v7** (results in §3a) acted on those findings:
- hard 9,500-character cap on all hook output;
- first whole-file Read of a large file → best region plus an outline;
- a "Definitions and uses" list for task identifiers;
- inline the top 3 *distinct files*;
- cold scoring in every arm, with the model warmed up at load (the first Metal run took 10 s).

## 4. Against the goals

| goal | result (v7, v0.1.0 defaults; v3 single-prompt in brackets) | verdict |
|---|---|---|
| −50% codebase-reading tokens | −50.1% code-reading (CI −62%…−33%); −27.9% counting laya's injected context; −26.8% total input [v3: −44.7% / −29.6% / −42%] | **met on code reading (point estimate)**; not when counting the injection |
| −30% task time | −17.4% (CI −32%…−1%) over two prompts [v3: −24.6%, single prompt] | **not met** (significant improvement) |
| no quality drop | answer recall 0.933 vs 0.975, diff −0.042 (CI −0.108…0.000); read precision 0.55 vs 0.37 | **no significant loss**; point estimate −4 points |

## 5. What each component contributed (ablations)

| run | variant | wall | total input | answer recall |
|---|---|---|---|---|
| v1 | full injection (~3.1k tok) + Laya | −6% | −23% | 0.86 vs 0.91 |
| v1 | full injection, lexical only | −17% | −33% | 0.95 vs 0.91 |
| v2 | compact injection (~1.6k tok) + Laya | −21% | −32% | 0.875 vs 0.958 |
| v2 | compact, lexical only | −3.5% | −21% | 0.87 vs 0.96 |
| v3 | compact + Laya (same run as refs) | −14% | −30% | 0.933 vs 0.938 |
| v3 | **compact + Laya + reference expansion** | **−24.6%** | **−42%** | **0.950 vs 0.938** |

- **Injection size** was the first lever (−6% → −21% wall); **reference expansion** the second: in
  the same v3 run it moved wall −14% → −25% and total input −30% → −42% while fixing recall.
- **Laya vs lexical-only flipped between runs** (v1 favoured lexical, v2 Laya): at n=20 the
  model's marginal contribution is within run-to-run noise; its cost is ~0.8 s per prompt.
- **Read narrowing** fired once in 60 treated runs: Sonnet already reads with offset/limit.
- Offline retrieval (dev set, `bench/results/retrieval_dev.jsonl`): lexical MRR 0.644 → 0.700 with
  the prose-demotion prior; Laya re-rank 0.708–0.735; reference expansion raises recall of the
  injected map 0.70 → 0.74.

## 6. Engineering facts

- Index: 783 files → 8,842 chunks in 2.8 s; no-op re-index 75 ms (content-hash skip).
- Query (warm): lexical 13 ms p50; with Laya (24 candidates × 128 tokens, F16 Metal) ~0.77 s.
- Laya Rust port parity with the Python reference: CPU f32 logits |Δ| ≤ 7.3e-6; Metal f16 ≤ 9e-3.
- Hooks: UserPromptSubmit injection 16 ms client-side; every hook fails open.
- **Soak** (`bench/soak.py`, release binary, 15 min, 4 concurrent Claude-like sessions over 2
  repos, faults injected):
  - 151,326 hook calls with 0 failures (no non-zero exits, no invalid output).
  - A file changed under the index at 270 s and was restored at 540 s: handled.
  - The daemon was killed at 450 s: hooks failed open, and it autostarted within 30 s.
  - Latency p50 / p95 / max: prompt 22 / 86 / 1,240 ms, Read 7 / 19 / 230 ms, re-index on edit
    27 / 74 / 264 ms.
  - Daemon RSS stayed flat at 935–987 MB (about 843 MB is the F16 model), so no leak.
  - At about 48 prompts/s (far above human use) the single model scored about 2 prompts/s. The
    rest fell back to lexical ranking immediately instead of queueing, by design (busy flag):
    under overload quality degrades, latency does not.
- 320 tests green, `cargo clippy --workspace --all-targets -D warnings` and `cargo fmt --check`
  clean. The workspace also builds and tests on Linux (arm64 native, x86_64 emulated).

## 7. Honest caveats

- One repo (Rust), one model (Sonnet), one task type (localisation/explanation, no edits), n=20.
- References are name-based (no type resolution): common method names can link to unrelated
  definitions; ambiguous names (>3 definitions) are skipped.
- Cost did not fall in v2 because cached input is cheap and output tokens dominate price variance.

## 8. Next steps (ranked by expected value)

1. **Close the time gap (−17% → −30%).** Time is now dominated by the final answer turn and
   verification turns (forensics §6). The candidates are:
   - fewer verification turns, e.g. richer usage lists for identifiers the answer names;
   - making `laya_search` (MCP) the cheap default for follow-up lookups;
   - checking the `warm_search.rs`-style third-file misses on multi-file tasks.
2. **Residual prompt noise.** A few generic chunks (`hash_write.rs` HgetexMode,
   `shortest_path.rs`) recur across tasks via the answer-format tail ("paths", "files"), and
   per-repo IDF-aware term selection is the principled fix. The laya-typed-decisions comparison
   is done: not better for relevance (AUROC 0.539 vs laya-code 0.713).
3. **More evidence**: 60+ tasks across 3 repos/languages, plus edit tasks (SWE-bench-style);
   n=20 cannot separate Laya from lexical-only or adaptive from v3.
4. **Moon**: upstream OR queries and DEL de-indexing (see `crates/laya-store/MOON_NOTES.md`),
   then the embedded `moon-embed` store (architecture D2 v2).
