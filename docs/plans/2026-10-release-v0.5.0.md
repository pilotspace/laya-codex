# Plan: release v0.5.0 (the faster, more accurate reranker)

Status: proposed 2026-10-09. It follows [2026-10-lookup.md](2026-10-lookup.md).

## What ships

| change | evidence (offline, real hook, 120 tasks, 238 gold files) |
|---|---|
| `laya-code-r2`: a ModernBERT-base student trained on whole chunks, served at 128 tokens × 12 candidates, as the default model | 141 gold inlined vs r1's 132. Hook p50 517 → 167 ms on today's candle; every prompt ranked by the model |
| Exact Metal kernel speed-ups (decision head at the markers only, rotary, attention layout, bias) | the student's model stage 156 → ≈ 121 ms and hook p50 ≈ 130 ms, with outputs bit-identical (max \|ΔP\| = 0); r1 is also faster |
| Benchmark measurement fixes | follow-ups are served from their session's cache (salted); stage timers in each capture; atomic hook-log lines; a lookup-time metric; token counts at calibrated rates, with v12–v14 re-derived |
| Write-up: follow-up picks not built | best possible coverage is 40% of held-out follow-up lookups against a 50% bar |
| Training code and run manifest for r2 | reproducible like r1 (`finetune/runs/r2.json`) |

Not shipped:
- **Neural Engine (CoreML) path: parked.** It was ≈ 1.6–1.7× faster than the optimized candle, below
  the ≥ 2× rule, and adds 315 MB and a 14–41 s first compile. Branch `spike/coreml-ane` is kept.
- **Follow-up picks:** parked, with the write-up above.

## Gates before the tag

1. **CI is green on every PR**, with fmt, clippy and tests on macOS-14 and Ubuntu.
   - Hosted macOS runners may have no Metal device, so the Metal kernel tests can only be enforced
     locally. Run them before each merge with
     `LAYA_REQUIRE_METAL=1 cargo test --workspace --release --locked`.
2. **The r2 upload exists, and the installer pin points at it:**
   - the owner uploads to the `tindang/laya-code` branch `r2`;
   - the pin and the manifest sha in `install.sh` are filled in;
   - `scripts/test-install.sh` downloads and verifies it.
3. **The paid held-out run (benchmark v15)** shows the release doesn't make sessions worse.
   - Setup: `bench/tasks-heldout`, 3 arms (stock / v0.4.0 / release candidate), `claude-sonnet-5-5`,
     one live session per task, salted memo.
   - Estimate ≈ $21, cap $25. **Needs the owner's approval before launch.**
   - Pass means, against v0.4.0: lookup calls fall, first-question recall doesn't fall, cost isn't
     worse, and time falls. Code read + injected and the rank mode are reported in the same table
     (VISION's targets and evidence rules).
   - If it fails, the model change waits. The kernel and measurement changes can still ship as
     v0.4.1.

## Order of work

The order follows the PR dependencies. Squash-merge each, merge `main` into the next branch to
resolve conflicts, and never force-push. Every PR and merge needs the owner's go.

| # | branch → PR | contents | depends on |
|---|---|---|---|
| 1 | `docs/plan-2026-10-lookup` | the lookup plan with progress, plus this release plan | — |
| 2 | `fix/bench-lookup-time` | memo salt, stage timers, atomic hook log, lookup-time metric, calibrated token rates, re-derived docs | — |
| 3 | `bench/follow-up-picks` | replay script, results, RESULTS section | 2 (RESULTS.md conflict: merge `main` in) |
| 4 | `feat/laya-code-student` | student init, variable-window training, distillation option, MPS memory fixes, `runs/r2.json` | — |
| 5 | `perf/model-kernels` | exact kernel speed-ups and their review fixes; candle pinned `=0.11.0` | — |
| 6 | `feat/r2-default` | serving settings read from the model dir (r1 installs unchanged), r2 default, `search` at 12, doctor, installer pin, model card | 2, owner upload |
| 7 | paid run v15 (no PR) | from `main` after 1–6 | owner approval |
| 8 | `docs/results-v15` | README, RESULTS, VISION (targets table and decision-log rows: r2, the kernels, Neural Engine parked, follow-up picks parked), CHANGELOG, how-it-works, ROADMAP | 7 |
| 9 | `chore/release-0.5.0` | version bump in Cargo.toml, `plugin/.claude-plugin/plugin.json`, Cargo.lock and `finetune/candgen/Cargo.lock`; CHANGELOG heading; README roadmap line | 8 |
| 10 | tag `v0.5.0` | `release.yml` builds draft assets; check them and publish with hand-written notes | 9 |
| 11 | formula | `scripts/update-formula.sh 0.5.0`: one PR here and one in `pilotspace/homebrew-tap` | 10 |

## Clean-up (after the tag; each deletion confirmed with the owner)

The disk is 92% used, with 35 GiB free.

| item | size | proposal |
|---|---|---|
| merged branches and their worktrees (`.claude/worktrees/agent-*`, older named worktrees) | ≈ 13 GiB (mostly `target/`) | remove the worktrees once their branches are merged. Keep the branches, except the auto-named `worktree-agent-*` ones, which hold nothing extra |
| spike branches `spike/base-window`, `spike/coreml-ane` | small | keep, unmerged, as the record |
| `laya-code-student-*` exports (kd-best, kd-final, nokd-final and their `-w128`/`-w384` link dirs) | ≈ 1 GiB | delete; `laya-code-r2` is the kept copy |
| `laya-code-student-nokd-best-w128-coreml` | 4.3 GiB | delete; it can be regenerated from `spike/coreml-ane` |
| `modernbert-base-synthetic` | 0.9 GiB | delete (spike only) |
| `finetune/student-smoke`, `student-smoke-export` | 2.2 GiB | delete |
| `finetune/student-kd` | 3.3 GiB | delete (not shipped); keep its `train_log.json` in the run record |
| `finetune/student-nokd` | 3.3 GiB | keep `best.pt` (step 542, the shipped checkpoint); drop `last.pt` / `final-nokd.pt` |
| `laya-code-hf`, `laya-code-v1` (pre-r1 models) | 1.6 GiB | keep `laya-code-hf` (r1's warm start, needed to reproduce r1); delete `laya-code-v1` |
| benchmark and scratch directories from this round | ≈ 1.3 GiB | copy the committed results into `bench/results/`, then delete |

## Risks

| risk | mitigation |
|---|---|
| The offline gain doesn't show end to end (r1: +8 offline, n.s. end to end) | gate 3 decides; if it fails, ship v0.4.1 without the model |
| The r2 install pin is wrong | `test-install.sh` verifies the manifest sha; `LAYA_CODEX_MODEL_REVISION` overrides it |
| Existing r1 installs get the student's settings | serving settings come from the model dir; r1 keeps 128 × 16 (tested) |
| A candle upgrade breaks the internal kernel call | candle pinned to `=0.11.0`, with a comment |
| macOS 12–13 can't compile the bf16 kernels | guarded; falls back to candle's ops (covered by a forced-failure test; no real macOS 13 machine tried) |
| Linux CPU: the student is ≈ 17× slower than Metal | still budget-bounded, falling back to lexical, as today |
