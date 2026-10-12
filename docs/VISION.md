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

Measured against stock Claude Code on held-out tasks (`bench/tasks-heldout`: 3 repositories,
60 tasks laya-codex was not tuned on; paired, 95% bootstrap intervals). Figures are from
benchmark v15 (2026-10-09, v0.5.0 with laya-code-r2), with v14 (v0.4.0's model, r1) for
comparison. Tokens are counted at the rates measured from billed cache writes (2026-10-07)
([docs/RESULTS.md](RESULTS.md)). In v15 the score cache was off for every follow-up because of a
full disk, which adds about 0.14 s of hook time per session.

| target | definition | stock | laya-codex now | goal |
|---|---|---|---|---|
| **Cost −15%** | dollars per session: every token Claude is billed for, including the code laya-codex injects | $0.066 | $0.062 (−6.6% [−11.6, −1.1]), not met; v14 −3.4% (n.s.) | ≤ $0.056 |
| **Time −20%** | session wall-clock | 21.8 s | 18.5 s (−15.2% [−23.7, −1.6]), not met; −20.8% without one API outlier; v14 −12.1% | ≤ 17.4 s |
| **Quality** | answer recall of the gold files: first question up, both questions no loss | 0.733 / 0.919 | 0.844 / 0.914 (first +0.111 [+0.043, +0.185]; both −0.006, n.s.), **first and both met**; v14 first +0.099 (not met) | first ≥ +0.10; both not worse |
| **Tokens** | code read plus injected | 3,437 | 4,544 (+32.2% [+18.0, +49.0]), not met; v14 +50.9% | not significantly worse |

A goal is met when the point estimate reaches it and the 95% interval excludes zero.

**Regression check on the tuned tasks.** laya-codex was tuned on `bench/tasks-v8` from v8 to v13,
so those tasks now check that a change does not break what already works.
- **Where v13 stands on them:** cost −14.8% [−18.8, −10.4] ($0.081 → $0.069), time −19.2%
  [−23.3, −14.7], first-question recall +0.154, both questions −0.024 (n.s.), code read + injected
  +10.4% (n.s.).
- **Why held-out tasks decide (2026-09-30):** v14 showed the cost saving does not carry over from
  the tuned tasks to new ones.

**Why the targets were reset (2026-09-30).** The earlier goals, cost −50% and time −30%, are out
of reach of anything laya-codex controls:
- **Cost has a floor** (`bench/cost_floor.py`, v13 raw transcripts). Claude Code's own context is
  written on every session's first call (5,125 tokens), Claude writes its answers (about 1,500
  tokens), and each prompt re-reads the conversation. With zero lookups and no injection a session
  would cost $0.042 (−49%), already above the $0.040 that −50% requires; with zero lookups and
  today's injection, $0.053 (−34%). A smaller injection makes Claude read more (v11).
  (Re-derived 2026-10-07 with the answers and the follow-up's injection counted at the measured
  token rates; at 3.5 characters per token they were $0.035 (−57%) and $0.045 (−44%). Those
  were themselves re-derived on 2026-09-30 at Sonnet 5.5 prices with each session's cost counted
  once; the first figures, $0.052 and $0.068, used Sonnet 5 prices against double-counted costs
  and gave −59% and −47%.)
- **Time has a floor** (`bench/time_breakdown.py`). The final answers take about 12.4 s in both
  arms; with no lookups and no hook a session would still take about 13.5 s. The only lever left
  of any size, lookups on the follow-up, failed its offline replay (41% of follow-ups covered
  against a 69% bar), so −30% would need Claude to stop looking things up.
The new goals sit just ahead of v13 and are reachable with named levers: not injecting where stock
Claude Code is already cheap (httpx: cost +1.3%, n.s.), and the small time levers (about −21%).

The current figures are from benchmark v15 (2026-10-09, 60 held-out tasks, Claude Sonnet 5.5
(`claude-sonnet-5-5`) at medium effort on Claude Code 2.1.292, laya-code-r2 on `main` 4f0d2fb, both
prompts in one live session, a prompt-cache tag per arm; see [docs/RESULTS.md](RESULTS.md)). The goals are relative, so their absolute values follow each
run's stock baseline.

**Why cost replaced code tokens (2026-09-29).** A re-baseline on Claude Code 2.1.284 with Claude
Sonnet 5.5 (`claude-sonnet-5-5`: the `sonnet` alias had moved since v10), 8 paired tasks on httpx
and hono, found that stock Claude now reads about 2,035 code tokens per session, against 4,635 in
v10. Half of that is below what laya-codex injects on its own (about 1,570). (These pilot
figures are at 3.5 characters per token and were not re-derived.) That injection is what
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
  - laya-codex sessions make 2.3 tool calls against 5.9 for stock Claude, and 3.7 API calls
    against 5.9. Each API call avoided saves a re-read of the conversation and an answer, about
    $0.005.
  - The injected code replaces tool output roughly one for one, so cache writes, the new text each
    turn, barely move (−3.2%). At list prices they are 64% of laya-codex's bill ($4 per million
    tokens for the one-hour cache); cache re-reads are 12% and output 24%.
  - Most of that is Claude Code's own per-session context and Claude's answers, which both arms
    pay. The next ideas checked offline (skip low-confidence code; inject the follow-up's lookups)
    come to about 1% each; the old −50% goal was below the session's cost floor (above).
- **Time follows output.** Each 1,000 output tokens adds about 7.6 s (R² 0.82), whoever found the
  code; laya-codex cuts output 25.6%.
- **Pull is met.** Grep fell from 4.2 to 0.8 per session and total tool calls to 2.3, the lever's
  success line (Grep ≤ 1, ≈ 2 calls).
- **Tokens read plus injected (reported beside cost): the injection cancels the reading savings.**
  - Claude reads 58% less, but the injected code (about 2.9k tokens per session) brings the total
    back above stock (+10.4% pooled, n.s.; +22.6% with repos weighted equally, up on httpx and hono).
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
| 2026-09-30 | Targets reset to cost −15%, time −20%, first-question recall +0.10 with no both-question loss, and code read plus injected not significantly worse; met means the estimate reaches the goal and the 95% interval excludes zero. The −50% cost goal is below the session's cost floor ($0.068 with zero lookups and today's injection, `bench/cost_floor.py`), and −30% time needs Claude to stop looking things up: answers take about 12.4 s in both arms, and the follow-up completeness replay covered 41% of follow-ups against a 69% bar, so it is not built (`bench/followup_completeness.py`). Next: don't inject where stock is already cheap (httpx) and confirm on real sessions |
| 2026-09-30 | Benchmark v14 on 60 held-out tasks. The low-confidence gate showed no effect and is not merged (PR #39 closed, branch kept). Three harness bugs are fixed: costs counted prompt 1 twice (v12–v14 re-derived: v13 cost −13.7% → −14.8%; the cost floor with today's injection is $0.045, −44%, so −50% stays out of reach); arms with the same system prompt shared Anthropic's prompt cache (each arm now gets its own tag); and one seed fixed the arm order (now balanced). On held-out tasks `main` vs stock: cost −3.4% (n.s.), time −12.1%, first-question recall +0.099, code read + injected +49.9%. The targets are unchanged; progress is judged on held-out tasks from now on (`bench/tasks-heldout`), and the tuned `tasks-v8` become a regression check |
| 2026-10-07 | Token counts re-derived for v12–v14. The harness counted code at 3.5 characters per token; regressing billed cache writes on characters gives 0.42–0.46 tokens per character on Sonnet 5.5 (`bench/runs.py` keeps one rate per kind: Read, Grep, Glob, `search`, injected), so code read and injected were 1.5–1.6× low on both sides. v14 code read + injected +49.9% → +50.9% (stock 1,911 → 2,998, `main` 2,863 → 4,522 tokens); v13 +9.5% → +10.4% (n.s.). The cost floor with today's injection rises from $0.045 (−44%) to $0.053 (−34%), and without injection ($0.042, −49%) it is now above the −50% line too. Costs, time, turns and recall are unaffected; the targets are unchanged |
| 2026-10-07 | Plan for fewer lookup round trips (docs/plans/2026-10-lookup.md): quality must improve, hook p50 ≤ 0.5 s, the cost target binds. Follow-up picks are parked: even an oracle covers 40% of held-out follow-up lookups against a 50% bar, because about a quarter are for names Claude first thinks of on the follow-up |
| 2026-10-09 | laya-code-r2 becomes the default: ModernBERT-base with laya's decision head, trained by this project on laya-codex candidate lists (labels only), served at 128 tokens × 12 candidates. Offline it inlines 141 of 238 gold files against r1's 132; the hook takes about 130 ms (p50) instead of 517 ms with four exact Metal speed-ups (bit-identical output). "Laya" stays the name of the method: the candidate lists, the decision head and the blend; the docs say the encoder is ModernBERT-base. Longer windows (256, 384) cost time and inlined no more gold |
| 2026-10-09 | The Apple Neural Engine path is parked: 1.6–1.7× faster than the optimized candle (2× only against the old one), with a 14–41 s first compile, about 315 MB more download, a CoreML bridge and ranking that is not bit-identical |
| 2026-10-09 | Metal on macOS before 15 fixed: candle 0.11 panicked opening a device there, so every release since v0.1.0 ranked by keywords only on those Macs. A vendored `candle-metal-kernels` guards it |
| 2026-10-09 | Benchmark v15 (held-out, stock / v0.4.0 / v0.5.0 candidate, $11.37): v0.5.0 is not worse than v0.4.0 (cost −1.2%, first-question recall +0.035, both n.s.; lookup time −16.7%, significant; wall +2.0%, n.s., from one API outlier). Against stock: cost −6.6%, time −15.2%, first-question recall +0.111 (met), both questions −0.006 (met), code read + injected +32.2%. The score cache was off by accident (disk 2% free; Moon stops writes under 5%); `run_bench` now refuses such a disk |
