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

## 3. Headline result — v2 (compact injection, the shipped default)

`bench/results/claude-v2/stats-laya-compact.txt` — laya-compact vs baseline, paired n=20:

| metric | change | 95% CI | lower in |
|---|---|---|---|
| code-reading tokens (Read/Grep/Glob output) | **−47.2%** | [−67.3%, −14.2%] | 13/20 |
| reading + injected tokens | −31.5% | [−56.6%, +8.1%] | 9/20 |
| total input tokens (all turns) | **−31.8%** | [−50.8%, −7.0%] | 15/20 |
| wall-clock | **−21.3%** | [−34.3%, −7.9%] | 16/20 |
| turns | **−27.7%** | [−39.4%, −13.8%] | 17/20 |
| cost (USD) | −2.5% | [−27.1%, +28.1%] | 6/20 |
| answer recall / precision | 0.875 / 0.398 vs 0.958 / 0.342 | | |

Grep calls per task fell from 5.45 to 3.1.

## 4. Against the goals

| goal | result | verdict |
|---|---|---|
| −50% codebase-reading tokens | −47% code-reading tokens (significant); −31.5% counting laya's own injected context (not significant) | **close, not met** |
| −30% task time | −21% (CI −34%…−8%) | **not met** (significant improvement) |
| no quality drop | precision +5.6 pts, recall −8.3 pts | **recall regression to fix** |

## 5. What each component contributed (ablations)

| | v1 full injection (~3.1k tok) | v2 compact injection (~1.6k tok) |
|---|---|---|
| with Laya model | wall −6%, total input −23% | **wall −21%, total input −32%** |
| lexical only (no model) | wall −17%, total input −33% | wall −3.5%, total input −21% |

- The **injection format** was the biggest lever: halving injected tokens turned −6% into −21% wall.
- **Laya vs lexical-only flipped between runs** (v1 favoured lexical, v2 favoured Laya). At n=20
  the model's marginal contribution is not separable from run-to-run noise; its latency cost
  (~0.77 s per prompt on Metal) is small next to the ~7 s saved per task.
- **Read narrowing** fired once in 40 treated runs: Sonnet already reads with offset/limit.
- Offline retrieval (dev set, `bench/results/retrieval_dev.jsonl`): lexical MRR 0.644 → 0.700 with
  the prose-demotion prior; Laya re-rank 0.708–0.735; laya-code is calibrated (ECE 0.46 → 0.06)
  but did not beat laya-base on MRR (see `finetune/` model card).

## 6. Engineering facts

- Index: 783 files → 8,842 chunks in 2.8 s; no-op re-index 75 ms (content-hash skip).
- Query (warm): lexical 13 ms p50; with Laya (24 candidates × 128 tokens, F16 Metal) ~0.77 s.
- Laya Rust port parity with the Python reference: CPU f32 logits |Δ| ≤ 7.3e-6; Metal f16 ≤ 9e-3.
- Hooks: UserPromptSubmit injection 16 ms client-side; every hook fails open.
- 191 tests green, `cargo clippy --workspace --all-targets -D warnings` clean.

## 7. Honest caveats

- One repo (Rust), one model (Sonnet), one task type (localisation/explanation, no edits), n=20.
- Recall dropped: the agent trusts the injected map and lists fewer secondary files.
- Cost did not fall in v2 because cached input is cheap and output tokens dominate price variance.

## 8. Next steps (ranked by expected value)

1. **Recall**: add "also check callers/tests of these symbols" edges (def/ref graph) to the map;
   inject only 2 full spans + more map lines.
2. **More evidence**: 60+ tasks across 3 repos/languages, plus edit tasks (SWE-bench-style), to
   settle the Laya-vs-lexical question with power.
3. **Laya**: longer fine-tune on dedicated hardware (validation AUROC was still rising); or use
   Laya for the cheap `noul` "is this prompt code-related" skip decision.
4. **Moon**: upstream OR queries and DEL de-indexing (see `crates/laya-store/MOON_NOTES.md`),
   then the embedded `moon-embed` store (architecture D2 v2).
