# laya-codex — vision and scope

This page is the project's north star. Every change should move one of the targets below through
one of the levers below. Anything else is out of scope until this page changes.

## What laya-codex is

laya-codex gives Claude Code the code a task needs, so Claude stops hunting for it with Grep,
Glob and Read.

```
repository ──tree-sitter──► 10–50 line chunks ──► Moon (BM25 store, cache)
prompt / search ──► lexical candidates (BM25 ⊕ defining ⊕ path, top 24)
                ──► Laya reranks them (laya-code cross-encoder, 1.2 s budget)
                ──► the right code reaches Claude:
                      push: UserPromptSubmit hook injects the top blocks
                      pull: the MCP `search` tool answers Claude's own lookups
```

- **Lexical finds, Laya decides the order.** BM25, definitions and path matches supply the
  candidates; the Laya model reranks them. Lexical-only is the fallback when the model is absent
  or out of time, never the goal. (Owner decision 2026-09-25.)
- **Two delivery paths.** Push code with the prompt; answer Claude's own searches through the MCP
  `search` tool, steered there by its description and server instructions. Claude's Grep and Read
  calls are not intercepted or rewritten by hooks (owner decisions 2026-09-24 and 2026-09-25).
- **Local and fail-open.** Everything runs on the user's machine; a hook that errors or times out
  outputs nothing and Claude carries on unchanged.

## Targets

Measured against stock Claude Code on the benchmark suite (3 repositories, 60 tasks, paired,
95% bootstrap intervals).

| target | definition | stock | laya-codex now | goal |
|---|---|---|---|---|
| **Cost −50%** | dollars per session: every token Claude is billed for, including the code laya-codex injects | $0.128 | $0.110 (−13.7%) | ≤ $0.064 |
| **Time −30%** | session wall-clock | 23.2 s | 18.7 s (−19.2%) | ≤ 16.2 s |
| **Quality: no loss** | answer recall of the gold files, turn 1 and both turns | 0.726 / 0.954 | 0.881 / 0.931 (turn 1 +0.154; both −0.024, n.s.) | not worse on either |

The current figures are from benchmark v13 (2026-09-30, 60 paired tasks, Claude Sonnet 5.5
(`claude-sonnet-5-5`) at medium effort on Claude Code 2.1.284, laya-code-r1, both prompts in one
live session; see [docs/RESULTS.md](RESULTS.md)). The goals are relative, so their absolute values
follow each run's stock baseline.

**Why cost replaced code tokens (2026-09-29).** A re-baseline on Claude Code 2.1.284 with Claude
Sonnet 5.5 (`claude-sonnet-5-5`: the `sonnet` alias had moved since v10), 8 paired tasks on httpx
and hono, found that stock Claude now reads about 2,035 code tokens per session, against 4,635 in
v10. Half of that is below what laya-codex injects on its own (about 1,570). That injection is what
buys the rest of the result, stock → laya-codex:

| answer recall (findable gold), turn 1 / both turns | tool calls | time | code read + injected | cost |
|---|---|---|---|---|
| 0.50 → 0.90 / 1.00 → 0.94 | 5.0 → 3.4 | −13% | +48% | −36% |

Eight tasks is a small sample, not a headline; benchmark v13 replaced it.

Code tokens read plus injected stay reported, next to cost, but they are no longer the goal.
Benchmark v13 re-measured time on all 60 tasks with current Claude Code: −19.2%.

Why the gaps exist (v13 `runs.jsonl`, `bench/ledger.py`, and the `result` usage in the raw
transcripts):

- **Cost follows API calls.**
  - laya-codex sessions make 2.3 tool calls against 5.9 for stock Claude (4.3 turns against 7.9).
    Each call avoided saves a re-read of the conversation and an answer, about $0.008.
  - The injected code replaces tool output roughly one for one, so cache writes, the new text each
    turn, barely move (−3.2%). At list prices they are 64% of laya-codex's bill ($6 per million
    tokens for the one-hour cache); cache re-reads are 12% and output 24%.
  - Most of that is Claude Code's own per-session context and Claude's answers, which both arms
    pay. The next ideas checked offline (skip low-confidence code; inject the follow-up's lookups)
    come to about 1% each, so −50% is not in reach of the injection alone.
- **Time follows output.** Each 1,000 output tokens adds about 7.6 s (R² 0.82), whoever found the
  code; laya-codex cuts output 25.6%.
- **Pull is met.** Grep fell from 4.2 to 0.8 per session and total tool calls to 2.3, the lever's
  success line (Grep ≤ 1, ≈ 2 calls).
- **Tokens read plus injected (reported beside cost): the injection cancels the reading savings.**
  - Claude reads 57% less, but the injected code (about 1.8k tokens per session) brings the total
    back above stock (+9.5%, n.s.).
  - The retrained reranker (laya-code-r1) ranks the right code higher, 70 vs 62 of 115 offline.
  - Showing less of each block does not help. v11 cut the injection 27% with 18-line windows, and
    Claude read 23% more to see the rest: read + injected rose 10.6%.
  - Each injected token that Claude needed saves more than a token of reading. So the next step has
    to cut what Claude reads, not what laya-codex shows.

## The levers (the only work in scope)

1. **Pull:** Claude answers its own lookups (where is X, who calls X, which tests cover X) with
   laya-codex `search` instead of Grep. Success: Grep ≤ 1 and total tool calls ≈ 2 per session.
2. **Precision:** Laya ranks the right code first, so fewer injected blocks carry the gold file.
   Success: the retrained model beats keyword ranking in the offline replay (more gold inlined at
   the same or smaller injection), then in the benchmark.
3. **Honest measurement:** every claim comes from the paired benchmark or the offline replay,
   with the rank mode recorded per prompt, and reports quality, time, tool calls, cost and code
   tokens (read plus injected) together, so a gain on one is weighed against the others.

## Out of scope

Removed (owner decision 2026-09-25; the simplification workstream deletes the code):

- Batch reads and prefetch (PR #9: +63% injected text and +21% cost, no fewer reads).
- The scope classifier (off; zero-shot macro-F1 ≤ 0.28) and probability-threshold sizing (off;
  lost gold against rank-based sizing).
- Repository-size caps (`LAYA_CODEX_SIZE_BY_REPO`, opt-in; lost inlined gold on small repos).
- Read narrowing: zero narrowed Reads in v9, because Claude reads with offset/limit after Grep.
  The hook keeps silently recording Reads so the session delta never re-injects a file Claude
  already read.

Frozen (not developed unless this page changes):

- New distribution channels, renaming, charts and README work beyond reporting results.
- Grep interception hooks and embeddings.

## Decision log

| date | decision |
|---|---|
| 2026-09-22 | Rust; tree-sitter chunks of 10–50 lines; Laya as the final reranker; Moon as the store (sidecar now, embedded later); hooks + MCP + guarded Read rewrite; no embeddings in v1; macOS Metal + Linux CPU |
| 2026-09-23 | Apache-2.0; `laya-code` published on Hugging Face; release with honest numbers even when targets are missed; product name stays laya-codex, bare "Laya" means the upstream model |
| 2026-09-23 | The Laya model stays on by default: the aim is a balance of speed and accuracy, with Laya deciding which code replaces Claude's Search/Read |
| 2026-09-24 | Keep the −30% wall-clock goal. Retrain the model to decide what loads, labels from fixed commits first. Steer Claude to MCP `search` through its description, not Grep hooks |
| 2026-09-25 | Keep lexical candidates + Laya rerank (lexical mode is not dropped). Refocus the project on the three levers above; everything else is frozen |
| 2026-09-25 | The token target counts tokens read plus tokens injected. Remove the scope classifier, probability-threshold sizing, repository-size caps and the Read-narrowing hook (supersedes the 2026-09-22 guarded Read rewrite) |
| 2026-09-25 | `search` answers name lookups (callers, uses, tests, definitions) with an exact, complete scan of the files on disk that the indexer admits; Laya orders the files and picks the definition shown. Descriptions of concepts keep the lexical + Laya ranking. Completeness is what Claude reaches for Grep to get, and no ranker can promise it |
| 2026-09-25 | Merge name lookups (#18) although the pilot gate (≥ 1 `search` per session, Grep −⅓) was not met: Claude used `search` in half the sessions and read + injected fell 10.6%. Test lookups were dropped: Claude asked for them in 3 of 13 searches and tokens rose 8% |
| 2026-09-26 | Accept laya-code-r1: 70 of 115 gold inlined in the production blend against 62 for keywords and 63 for v1, at the same latency. The "no more injected chars" clause (+2.5%) is waived. Laya alone now beats keywords (72 vs 62) |
| 2026-09-27 | Release v0.4.0 with laya-code-r1, published as revision `25f97e5` on a separate `r1` branch of the Hugging Face repo (main keeps the first model) and pinned by the installer |
| 2026-09-28 | Park the smaller injection (18-line windows, no first-prompt uses): it passed the offline replay but raised read + injected 10.6% in benchmark v11, because Claude read the rest of each block. Not merged; v0.4.0 ships without it |
| 2026-09-29 | The token goal becomes cost per session −50% against stock, with no quality loss. Stock Claude Code 2.1.284 on Sonnet 5.5 reads about half the code it did in v10 (Sonnet 5), which puts −50% of read plus injected below laya-codex's injection alone. Read plus injected stays reported. The −30% time goal is kept until all 60 tasks are re-measured on current Claude Code. Every ranking is recorded locally by default (`laya-codex capture`, merged after v0.4.0). Answering Grep or Read through laya-codex may be explored again (reverses the 2026-09-24/25 rule). Stronger "answer from the shown code" wording failed a pilot (tokens −3%, a gold file lost) and is not merged. The next decisive run is all 60 tasks on Sonnet 5.5 at medium effort, pinned by model id, with stock, laya-codex and keywords-only arms (estimate $26, cap $32) |
| 2026-09-30 | Benchmark harness fixed: a task's prompts run in one live session (resuming per prompt charged laya-codex for rewriting the prompt cache in about half its sessions, v12), and benchmark repos stay out of the home directory (every session in every arm loaded the operator's `~/.claude/CLAUDE.md`). v13 on Sonnet 5.5: cost −13.7%, time −19.2%, first-question recall +0.154, both-question recall −0.024 (n.s.). Skipping low-confidence code (~0.3%) and injecting the follow-up's lookups (<1%) are not built. Whether −50% cost stays the goal is open |
