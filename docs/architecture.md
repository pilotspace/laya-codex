# laya-codex — Architecture (v1.0, decided 2026-09-23)

Status: **accepted — see §4 Decision record**. Evidence: `docs/research/*.md` (2026-09-23).

## 1. Goal and success metrics

Give Claude Code the *right* 10–50-line code spans before it goes exploring, so it
reads less and decides faster.

| Metric | Target | How measured |
|---|---|---|
| Input tokens spent on codebase reading (Read/Grep/Glob results) | **−50%** vs baseline | `claude -p --output-format stream-json` usage, paired runs |
| Wall-clock per task | **−30%** | process time, same task set, n ≥ 30 |
| Task success (resolve rate) | **no regression** (guardrail) | SWE-bench-Lite-style subset + in-house tasks |
| Hook latency p95 | < 300 ms warm, hard timeout 1.5 s → no-op | daemon tracing |

Savings figures without a success-rate check are meaningless, so we always report both.

## 2. What the research changed in the draft

1. **Laya is not a code model.** It is a ModernBERT-large (421M) *typed-decision*
   cross-encoder (`choice` / `score` / `noul` → calibrated probabilities), trained on
   triage/moderation/routing, 512–1024 token context, T4-only benchmarks, safetensors
   only with a custom head. Scoring every chunk with it is O(N) forward passes, which
   comes to seconds for about 50 chunks on CPU (extrapolated). → **Laya cannot be the retriever;
   it fits as the final *decision* stage over a small shortlist**, which is exactly its
   "probability / choice / score" strength.
2. **pilotspace/moon is a server** (Redis-compatible, RESP, `FT.*` HNSW + BM25, WAL,
   pre-1.0, monoio/io_uring on Linux, tokio fallback on macOS). Only a client crate
   (`moondb`) exists. → v1 runs Moon as a **sidecar** behind a `Store` trait; v2 swaps in an embedded
   moon engine crate (D2). The hook fails open whenever Moon is unavailable.
3. **The hook surface is real**: `UserPromptSubmit` can inject `additionalContext`
   deterministically; `PreToolUse` can rewrite Read/Grep/Glob `tool_input`. MCP tools are
   model-chosen (less reliable) but good for follow-up. No existing tool combines
   tree-sitter chunking with deterministic hook injection, so this is an open gap.
4. **Chunking**: cAST (EMNLP 2025) split-then-merge along the AST, with tree-sitter
   `tags.scm` for symbols plus aider-style PageRank over the def/ref graph for priors.

## 3. Architecture

```
 Claude Code
   │ UserPromptSubmit / PreToolUse(Read|Grep)        MCP: laya_search, laya_expand
   ▼                                                  ▼
 laya-hook (tiny client, 1.5 s timeout, fail-open) ──► layad (daemon, unix socket)
                                                        │
      ┌─────────────────────────────────────────────────┤
      ▼                     ▼                           ▼
 [1] Indexer           [2] Candidate gen (<50 ms)   [3] Laya decision (≤250 ms)
  tree-sitter parse     BM25 over chunks + symbols    rerank top-K (K≈30–48)
  cAST chunk 10–50 ln   symbol/path exact match        score q: "P(chunk needed)"
  tags → def/ref graph  graph proximity (PageRank,     choice q: span size 10/25/50
  PageRank priors        files in prompt / git diff)   keep P ≥ 0.5, top 10
  incremental (notify   RRF fusion → top-K
  + Tree::edit)
      │                     │                           │
      └──────────► [4] Store trait ─► Moon sidecar via RESP/moondb (v1: FT.* BM25, chunk, memo)
                                     └► moon-embed in-process (v2, same trait)
```

### 3.1 Indexer (Rust, `tree-sitter` 0.27)
- Grammars (D5): statically linked crates for the core set (Rust, Python, TS/JS/TSX, Go,
  Java, C/C++, C#, Ruby, PHP, Kotlin, Swift). Any other text file gets a line-window chunker
  (30-line windows, 10-line overlap, split at blank lines). A vendored WASM long tail is deferred to v2.
- Chunking: a Rust port of cAST, split to ≤50 lines and merge siblings to ≥10 lines.
  Each chunk records `{path, byte/line range, symbol path, kind, lang, content_hash}`.
- Graph: `tags.scm` def/ref → symbol graph → PageRank (repo-level prior, recomputed lazily).
- Incremental: `notify` watcher; the content hash skips unchanged files; tree-sitter `Tree::edit`
  handles hot files.

### 3.2 Candidate generation (cheap, recall-oriented)
- BM25 over chunk text, symbol names and path tokens (Moon `FT.SEARCH`).
- Signals: identifiers/paths extracted from the prompt, `git diff` and recently touched
  files, graph neighbours (callers/callees of matched symbols), PageRank prior.
- Fusion by Reciprocal Rank Fusion → top-K (K≈32, tunable).
- *Deferred (D4)*: code bi-encoder embeddings (CodeRankEmbed / nomic-embed-code) in Moon
  HNSW, to be added only if the benchmark shows recall misses on vague prompts.

### 3.3 Laya decision stage (precision)
- Runtime: `candle` (Metal on Apple Silicon, CPU on Linux; CUDA later) runs the ModernBERT backbone; the decision
  head is ported from `rl_agent_api.py`; the `tokenizers` crate handles tokenization. The model stays warm in `layad`.
- Per candidate: state = task prompt (truncated) + chunk header + chunk text (≤~400 tok);
  questions: `score: relevant-to-task` and `choice: span {tight,function,block}`.
- Output: keep `P ≥ 0.5`, top 10, then expand or trim each span to 10–50 lines at AST boundaries.
- Budget guard: if the deadline is hit, return stage-2 ranking with its RRF scores (degraded but never blocking).
- **Risk**: Laya zero-shot on code is unproven, so there's a Phase-0 spike (D1).

### 3.4 Store / cache (Moon)
| Key | Value | Invalidation |
|---|---|---|
| `chunk:{content_hash}` | chunk meta + text | content-addressed (never stale) |
| `file:{path}` | hash list, mtime | watcher |
| FT index `idx:chunks` | BM25 (+ HNSW if D4) | on HSET |
| `q:{hash(prompt_norm, index_epoch)}` | ranked result | epoch bump on reindex |
| `laya:{hash(prompt_norm, chunk_hash)}` | Laya probability | content-addressed |

The Laya score memo is the big win: repeat and similar prompts in a session skip inference.
Access goes through a `Store` trait: v1 = `MoonRespStore` (moondb client, local moon on a
unix socket or 127.0.0.1), v2 = `MoonEmbedStore` once moon ships an in-process engine crate.
Resilience: per-call timeout 50 ms, 2 retries with jitter, and a circuit breaker (open after 5
failures, half-open after 10 s). While it's open, `layad` serves an in-memory LRU of recent results and
the hook **fails open** (no injection, no rewrite), so Claude Code is never blocked. `layad`
supervises the moon process (spawn, health-check `PING`, restart with backoff). The index
can be rebuilt from source, so it's never the source of truth.

### 3.5 Claude Code integration
- **UserPromptSubmit hook**: injects ≤10 spans (token budget ~3–5k) as `additionalContext`,
  with a header that tells the agent "prefer these ranges; Read with offset/limit".
- **MCP server** (`laya_search`, `laya_expand`, `laya_symbol`): model-directed follow-up.
- **PreToolUse(Read) guarded rewrite** (D3): rewrite to the ranked range via `updatedInput`
  (`offset`/`limit`, covering the top ranked spans of that file plus 5 lines of context) **only if all hold**:
  file > 300 lines · the Read has no offset/limit · a span in that file has P ≥ 0.7 for the
  current prompt · this is the first Read of that file this session. The response adds a note
  ("narrowed by laya: lines a–b; Read again for the full file"). **Escape hatch**: a
  second Read of the same file passes through untouched. Every rewrite is logged, so the
  benchmark can measure how often the agent needed the full file.
- Packaged as a Claude Code plugin (hooks + MCP + install script).

### 3.6 Rust workspace
```
crates/ core (types, errors) · parse (tree-sitter, cAST, tags) · graph (PageRank)
        store (trait Store; MoonRespStore v1, MoonEmbedStore v2) · rank (BM25/RRF)
        laya (candle model, head, tokenizer) · daemon (layad, unix socket, tracing)
        cli (laya index|query|hook|mcp) · bench (token/time harness)
```
Release profile: `lto="fat"`, `codegen-units=1`, `panic="abort"`, `strip=true`, mimalloc,
cargo-pgo on the index and query paths; no `target-cpu=native` for distributed builds.

## 4. Decision record (2026-09-23)

| # | Decision | Chosen | Consequence |
|---|---|---|---|
| D1 | Laya's role | **Final reranker over a ~32-chunk shortlist**, gated by a Phase-0 spike | If zero-shot P@10 is too low, the fallback is fine-tuning Laya on code relevance (not a model swap) |
| D2 | Moon | **Embedded library is the target; sidecar over RESP now** | `Store` trait; moon-embed crate built in the moon repo, swapped in at v2 |
| D3 | Integration | **UserPromptSubmit injection + MCP + guarded PreToolUse(Read) rewrite** | Biggest token lever; guarded by thresholds and escape hatch (§3.5) |
| D4 | Embeddings | **None in v1** (BM25 + graph + Laya) | Revisit only if the benchmark shows recall misses on vague prompts |
| D5 | Languages | **Core static grammars + line-window fallback** | WASM long tail in v2 |
| D6 | Platforms | **macOS Metal + Linux CPU** | CI matrix: macos-14 arm64, ubuntu x86_64 |

## 5. Phasing
0. **Spike** (1–2 d): Laya in candle (Metal + CPU) on 3 repos, then measure P@10, latency for
   K=32, and the 0.5/0.7 thresholds' calibration. **Go/no-go gate for D1** (target: P@10 ≥ 0.6,
   p95 ≤ 250 ms on M-series Metal). If it fails, a fine-tune track opens before Phase 2.
1. Indexer: tree-sitter core grammars, cAST chunker, tags/PageRank, CLI `laya index|query`
   (red/green TDD, golden-chunk tests per language).
2. `Store` trait + `MoonRespStore` (moon sidecar supervised by `layad`) + BM25/RRF candidate
   generation + Laya stage + memo cache.
3. Claude Code plugin: UserPromptSubmit injection, MCP tools, guarded PreToolUse(Read) rewrite.
4. Benchmark harness (paired baseline/treatment, n ≥ 30), then tune K, thresholds and token budget
   against −50% tokens / −30% time with no drop in resolve rate. Release profile + PGO.
5. (moon repo) `moon-embed` engine crate → `MoonEmbedStore`, removing the sidecar.
