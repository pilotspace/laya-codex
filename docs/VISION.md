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
  `search` tool, steered there by its description and server instructions. Claude's Grep is not
  intercepted by hooks (owner decision 2026-09-24).
- **Local and fail-open.** Everything runs on the user's machine; a hook that errors or times out
  outputs nothing and Claude carries on unchanged.

## Targets

Measured against stock Claude Code on the benchmark suite (3 repositories, 60 tasks, paired,
95% bootstrap intervals). Baseline numbers are benchmark v3 (v9, 2026-09-24, PR #15 branch).

| target | definition | stock | laya-codex now | goal |
|---|---|---|---|---|
| **Tokens −50%** | code tokens reaching Claude per session: tokens Claude reads **plus** tokens laya-codex injects | 4,230 | 4,424 (+4.6%) | ≤ 2,115 |
| **Time −30%** | session wall-clock | 33.4 s | 29.6 s (−11.4%) | ≤ 23.4 s |
| **Quality: no loss** | answer recall of the gold files, turn 1 and both turns | 0.774 / 0.968 | 0.910 / 0.951 | not worse on either |

Why the two gaps exist (from v9 `runs.jsonl`):

- **Time follows tool calls.** Each tool call costs about 2 s. laya-codex sessions still make 5.1
  tool calls (2.85 Grep, 2.17 Read) against 8.45 for stock Claude, and call laya-codex `search`
  0.02 times. Reaching −30% means about 2 tool calls per session.
- **The injection cancels the reading savings.** Claude reads 36% less, but the injected code
  (1,704 tokens per session) brings the total back above stock. Reaching −50% needs a smaller
  injection that still carries the right code: that is the reranker's job.

## The levers (the only work in scope)

1. **Pull:** Claude answers its own lookups (where is X, who calls X, which tests cover X) with
   laya-codex `search` instead of Grep. Success: Grep ≤ 1 and total tool calls ≈ 2 per session.
2. **Precision:** Laya ranks the right code first, so fewer injected blocks carry the gold file.
   Success: the retrained model beats keyword ranking in the offline replay (more gold inlined at
   the same or smaller injection), then in the benchmark.
3. **Honest measurement:** every claim comes from the paired benchmark or the offline replay,
   with the rank mode recorded per prompt, and the token target counts injected code.

## Out of scope (frozen)

Not developed further unless this page changes. Existing code stays until a simplification task
removes it with evidence.

- Batch reads and prefetch (PR #9: +63% injected text and +21% cost, no fewer reads).
- The scope classifier (off; zero-shot macro-F1 ≤ 0.28) and probability-threshold sizing (off;
  lost gold against rank-based sizing).
- Repository-size caps (`LAYA_CODEX_SIZE_BY_REPO`, opt-in; lost inlined gold on small repos).
- The Read-narrowing hook: zero narrowed Reads in v9, because Claude reads with offset/limit
  after Grep. Kept fail-open, not extended.
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
