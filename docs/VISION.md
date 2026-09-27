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
| **Tokens −50%** | code tokens reaching Claude per session: tokens Claude reads **plus** tokens laya-codex injects | 4,635 | 4,924 (+6.2%, n.s.) | ≤ 2,318 |
| **Time −30%** | session wall-clock | 38.3 s | 33.7 s (−12.0%) | ≤ 26.8 s |
| **Quality: no loss** | answer recall of the gold files, turn 1 and both turns | 0.758 / 0.953 | 0.904 / 0.940 | not worse on either |

The current figures are from benchmark v10 (2026-09-27, 51 paired tasks, laya-code-r1; see
[docs/RESULTS.md](RESULTS.md)). The goals are relative, so their absolute values follow each run's
stock baseline: v9 measured stock at 33.4 s and 4,230 tokens.

Why the two gaps exist (v10 `runs.jsonl`, `bench/ledger.py`):

- **Time follows tool calls.**
  - Each tool call costs about 2 s.
  - laya-codex sessions make 5.3 tool calls against 9.0 for stock Claude.
  - Since name lookups (#18), Claude calls laya-codex `search` about once per session (v9: 0.02),
    and Grep fell from 5.3 to 2.0. But it swapped Greps for searches one for one, so the total
    stayed at v9's level.
  - Reaching −30% means about 2 tool calls per session.
- **The injection cancels the reading savings.**
  - Claude reads 32% less, but the injected code (about 1.7k tokens per session) brings the total
    back above stock.
  - Reaching −50% needs a smaller injection that still carries the right code.
  - The retrained reranker (laya-code-r1) ranks the right code higher, 70 vs 62 of 115 offline,
    and shrinking the injection with it is the current work.

## The levers (the only work in scope)

1. **Pull:** Claude answers its own lookups (where is X, who calls X, which tests cover X) with
   laya-codex `search` instead of Grep. Success: Grep ≤ 1 and total tool calls ≈ 2 per session.
2. **Precision:** Laya ranks the right code first, so fewer injected blocks carry the gold file.
   Success: the retrained model beats keyword ranking in the offline replay (more gold inlined at
   the same or smaller injection), then in the benchmark.
3. **Honest measurement:** every claim comes from the paired benchmark or the offline replay,
   with the rank mode recorded per prompt, and the token target counts injected code.

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
