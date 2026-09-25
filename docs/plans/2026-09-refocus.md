# Plan: refocus on the three levers (v0.4.0)

Status: **proposed** (2026-09-25) · North star: [docs/VISION.md](../VISION.md)

## Outcome

laya-codex v0.4.0 moves the benchmark toward the two open targets without losing quality:
session wall-clock and code tokens (read + injected) both fall against v9, with Claude answering its
own lookups through laya-codex `search` and a retrained Laya ranking the right code first.

## Constraints

- Lexical candidates + Laya rerank stay; lexical-only is the fallback, not a mode to optimise.
- No Grep interception hooks. Pull happens through the MCP tool's description and instructions.
- Every paid Claude Code benchmark run needs the owner's approval first (v9 cost $28 for 180 sessions).
- Nothing is pushed, merged, closed or released without the owner's go-ahead. Squash-merge in
  dependency order, merge `main` into a PR branch to resolve conflicts, never force-push.
- Red/green TDD for every behaviour change. `cargo fmt --check`, `cargo clippy -D warnings` and
  `cargo test --workspace --release` pass before a PR is opened.

## Non-goals

Everything listed as frozen in [VISION.md](../VISION.md#out-of-scope-frozen).

## Workstreams

| id | workstream | lever | owner (agent) | depends on | cost |
|---|---|---|---|---|---|
| WS0 | Foundation: base, docs, tracking | — | orchestrator (main session) | — | free |
| WS1 | Measurement truth | 3 | bench engineer (`python-expert`, sonnet) | WS0 | free |
| WS2 | Pull: `search` replaces Grep | 1 | Rust engineer (`senior-rust-engineer`, opus) | WS0 | pilot ≈ $6 |
| WS3 | Precision: retrain Laya as reranker | 2 | ML engineer (`ml-expert`, opus) | WS1 replay + training data restored by the owner | local GPU time |
| WS4 | Simplify: remove frozen code paths | — | Rust engineer (`senior-rust-engineer`, sonnet) | WS2 merged | free |
| WS5 | Validate and release v0.4.0 | all | orchestrator + bench engineer | WS2, WS3 gate, WS4 | full run ≈ $30 |

Every workstream ends with a review by `athena:code-reviewer` before its PR leaves draft.

### WS0 — Foundation (orchestrator)

1. Fast-forward local `main` to `origin/main` (v0.3.0).
2. Land PR #15 as the new baseline (v9 measured it; CI green): the MCP steering, `score_top=16`,
   rank-mode logging, follow-up answers and the bench harness.
3. Land this plan, [VISION.md](../VISION.md) and a public `CLAUDE.md` (focus rules) in one docs PR.
4. Close draft PR #9 (batch reads) with a pointer to the frozen list.
5. `add init` locally (the ADD bundle stays out of the public repo) and record each workstream as
   one ADD task.

Acceptance: `main` = v0.3.0 + PR #15 + docs; #9 closed; `add status` names WS1/WS2 as next.

### WS1 — Measurement truth (bench engineer)

Files owned: `bench/**`, `docs/RESULTS.md` (numbers only).

1. **Token target counts injected code.** `stats.py`/`stats_pooled.py`/`headline.py` report
   `reading_plus_injected` as the primary token metric, with `reading_tokens` beside it.
2. **Tool-call ledger.** Summaries report per-arm tool calls per session split by tool
   (Grep, Read, Glob, `mcp__laya-codex__search`) and hook actions per session, as computed for v9.
3. **Offline replay as the reranker gate.** Commit a reproducible script (building on
   `bench/replay_hooks.py`) that replays the benchmark prompts through a daemon and reports, per
   ranking arm (keywords, blend w=0.5, model alone w=1, candidate checkpoint): gold files inlined,
   gold in the top 2, injected chars, rank mode and p50/p95 scoring latency. The 2026-09-24
   numbers (gold inlined: keywords 62, blend 63, model alone 47 of 115) are the baseline to reproduce.
4. **Pilot mode.** `run_bench.py --pilot` runs a fixed stratified 6-task subset per repository with a
   cost cap, for go/no-go checks before a full run.

Acceptance: re-running the scripts on the committed v9 data reproduces the VISION numbers
(4,230 / 4,424 tokens, 5.10 tool calls, 2.85 Grep, 0.02 search); the replay reproduces 62/63/47
within ±2; tests in `bench/test_bench.py` cover the new metrics.

### WS2 — Pull: `search` replaces Grep (Rust engineer)

Files owned: `crates/laya-cli/src/mcp.rs`, `crates/laya-rank/src/render.rs`, their tests.

Why: v9 sessions made 2.85 Greps, mostly callers and tests of code already injected, and 0.02
`search` calls; each tool call costs about 2 s.

1. Classify what the v9 Greps asked for (pattern, intent: definition / callers / tests / exact
   text / non-code) from the per-prompt stream logs; report the share `search` could answer.
2. Make `search` answer those intents in one call: code for definitions, usage lines for callers,
   test pointers for tests; say so in the tool description and server instructions (PR #15 has
   the first version). Keep results under Claude Code's inline limit.
3. Make the injected context point to `search` for follow-up lookups (one line, not a tutorial).
4. Pilot (≈ $6, owner approval): stock vs v0.3.0+PR #15 vs WS2.

Gate to continue: in the pilot, `search` ≥ 1 call per session **and** Grep per session falls by at
least a third against PR #15, with turn-1 recall not lower. If Claude ignores `search`, stop and
report to the owner before trying anything else (the Grep-hook alternative is an owner decision).

### WS3 — Precision: retrain Laya as the reranker (ML engineer)

Files owned: `finetune/**`, `release/hf-laya-code/**`, model loading only if the export format changes.

Blocked until the owner restores the training inputs on this machine: the 8 training repositories
under `~/src`, `laya-base` weights, `rl_common.py`, and `~/.cache/laya-codex/finetune`.

1. Labels from fixed commits: for each commit that fixes an issue, the chunks it changed are the
   positives for the issue text; hard negatives are the top lexical candidates it did not touch.
   No benchmark repository or task in training data (leakage check committed).
2. Train the reranker on the same input as production: the cleaned task focus plus the 24 lexical
   candidates, 128 state tokens.
3. Evaluate with the WS1 replay only. Gate to ship: gold inlined ≥ keywords + 5 (≥ 67 of 115) at
   no more injected chars, and scoring p95 within the 1.2 s budget on the M4 Pro (Metal).
4. On a pass: export, publish a new `laya-code` revision on Hugging Face (owner uploads), pin its
   checksum in the installer.

If the gate fails twice, stop and report: the model remains the reranker (owner decision), and
the owner decides what changes next.

### WS4 — Simplify (Rust engineer)

Files owned: `crates/laya-rank/src/sizing.rs`, `crates/laya-cli/src/daemon.rs` (config and render
only), `crates/laya-cli/src/session.rs`, `finetune/scope*.py`, related bench configs.

Remove code paths that are off by default and have negative evidence, with tests updated first:
scope classifier (runtime path, `LAYA_CODEX_SCOPE*`), probability-threshold sizing
(`LAYA_CODEX_TAU_*`), `LAYA_CODEX_SIZE_BY_REPO`, and bench arms that only exercised them. The
Read-narrowing hook stays (frozen, fail-open).

Acceptance: default behaviour byte-identical on the replay (same injected text for every prompt);
fewer env knobs documented in `docs/how-it-works.md`; all CI checks green.

### WS5 — Validate and release (orchestrator + bench engineer)

1. Full paid run v10 (≈ $30, owner approval): stock vs v0.4.0 vs v0.4.0 keywords-only, 60 tasks.
2. Report with the WS1 metrics, whatever the result; update README and RESULTS with the numbers.
3. Release v0.4.0 through the documented routine (version bump PR, tag, draft, verify, publish,
   Homebrew formula).

## Order

```
WS0 ──► WS1 ──► WS3 (when the training data is back) ──┐
   └──► WS2 ──► pilot gate ──► WS4 ──────────────────── ┴──► WS5
```

WS1 and WS2 run in parallel (disjoint files). WS4 waits for WS2 to merge because both touch the
render path. WS3 starts as soon as the replay exists and the data is restored.

## Risks

| risk | mitigation |
|---|---|
| Claude keeps using Grep however `search` is described | the WS2 pilot gate stops the lever early; the owner decides on alternatives |
| The retrained model still ties keywords | the replay gate keeps the current model; no paid run is spent on it |
| Smaller injection loses gold | WS1 reports gold inlined next to injected tokens for every change |
| Parallel agents collide | file ownership above; one PR per workstream; merge in the order shown |

## Rollback

Every workstream is one squash-merged PR and can be reverted on its own. Model changes ship as a
new Hugging Face revision; the installer pins the checksum, so the previous model stays installable.
