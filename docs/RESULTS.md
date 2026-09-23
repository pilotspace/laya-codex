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

## 3. Headline result — v3 (compact injection + one-hop reference expansion, the shipped default)

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

## 4. Against the goals

| goal | result (v3) | verdict |
|---|---|---|
| −50% codebase-reading tokens | −44.7% code-reading tokens; −29.6% counting laya's own injected context; −42% total input | **close, not met** |
| −30% task time | −24.6% (CI −42%…−4%); −30.4% at 16/20 tasks | **not met** (significant improvement) |
| no quality drop | answer recall 0.950 vs 0.938, precision 0.420 vs 0.337, read recall 0.871 vs 0.767 | **met** |

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
- 232 tests green, `cargo clippy --workspace --all-targets -D warnings` clean.

## 7. Honest caveats

- One repo (Rust), one model (Sonnet), one task type (localisation/explanation, no edits), n=20.
- References are name-based (no type resolution): common method names can link to unrelated
  definitions; ambiguous names (>3 definitions) are skipped.
- Cost did not fall in v2 because cached input is cheap and output tokens dominate price variance.

## 8. Next steps (ranked by expected value)

1. **Adaptive loading (in progress)**: session delta injection (never re-send spans already in
   context), confidence-sized context from calibrated Laya P, and a Laya `choice` scope classifier
   (function / file / multi-file / cross-cutting) sizing how much to inject.
2. **laya-typed-decisions** checkpoint comparison (specialised for choice/score/noul questions).
3. **More evidence**: 60+ tasks across 3 repos/languages, plus edit tasks (SWE-bench-style).
4. **Moon**: upstream OR queries and DEL de-indexing (see `crates/laya-store/MOON_NOTES.md`),
   then the embedded `moon-embed` store (architecture D2 v2).
