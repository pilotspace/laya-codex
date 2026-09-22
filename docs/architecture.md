# laya-codex — Architecture (DRAFT v0.1, for discussion)

Status: **draft — decisions D1–D5 open**. Evidence: `docs/research/*.md` (2026-09-23).

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
   (`moondb`) exists. → Moon runs as a **sidecar**; we need an embedded fallback so the
   hook never breaks when Moon is down.
3. **The hook surface is real**: `UserPromptSubmit` can inject `additionalContext`
   deterministically; `PreToolUse` can rewrite Read/Grep/Glob `tool_input`. MCP tools are
   model-chosen (less reliable) but good for follow-up. No existing tool combines
   tree-sitter chunking with deterministic hook injection, so this is an open gap.
4. **Chunking**: cAST (EMNLP 2025) split-then-merge along the AST, with tree-sitter
   `tags.scm` for symbols plus aider-style PageRank over the def/ref graph for priors.

## 3. Proposed architecture

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
      └──────────► [4] Store: Moon sidecar (FT.* BM25/HNSW, chunk hash, query memo)
                          └─ fallback: embedded redb + tantivy (circuit breaker)
```

### 3.1 Indexer (Rust, `tree-sitter` 0.27)
- Grammars: static crates for a core set (Rust, Python, TS/JS, Go, Java, C/C++, C#,
  Ruby, PHP, Kotlin, Swift); a long tail via vendored `.wasm` grammars (no network
  download at runtime). "All languages" means core set plus WASM, with a line-window fallback for anything unknown.
- Chunking: a Rust port of cAST, split to ≤50 lines and merge siblings to ≥10 lines.
  Each chunk records `{path, byte/line range, symbol path, kind, lang, content_hash}`.
- Graph: `tags.scm` def/ref → symbol graph → PageRank (repo-level prior, recomputed lazily).
- Incremental: `notify` watcher; the content hash skips unchanged files; tree-sitter `Tree::edit`
  handles hot files.

### 3.2 Candidate generation (cheap, recall-oriented)
- BM25 over chunk text, symbol names and path tokens (Moon `FT.SEARCH` or tantivy).
- Signals: identifiers/paths extracted from the prompt, `git diff` and recently touched
  files, graph neighbours (callers/callees of matched symbols), PageRank prior.
- Fusion by Reciprocal Rank Fusion → top-K (K≈32, tunable).
- *Optional (D4)*: code bi-encoder embeddings (e.g. CodeRankEmbed / nomic-embed-code) in
  Moon HNSW for semantic recall on vague prompts.

### 3.3 Laya decision stage (precision)
- Runtime: `candle` (Metal on macOS, CPU fallback) runs the ModernBERT backbone; the decision
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
Resilience: client timeout 50 ms, 2 retries with jitter, and a circuit breaker to the embedded
redb+tantivy store; the index can be rebuilt from source, so it's never the source of truth.

### 3.5 Claude Code integration
- **UserPromptSubmit hook**: injects ≤10 spans (token budget ~3–5k) as `additionalContext`,
  with a header that tells the agent "prefer these ranges; Read with offset/limit".
- **MCP server** (`laya_search`, `laya_expand`, `laya_symbol`): model-directed follow-up.
- **PreToolUse(Read)** *(optional, D3)*: when Read targets a large file without a range,
  rewrite it to the ranked range(s) via `updatedInput`. This saves the most tokens but is the most intrusive.
- Packaged as a Claude Code plugin (hooks + MCP + install script).

### 3.6 Rust workspace
```
crates/ core (types, errors) · parse (tree-sitter, cAST, tags) · graph (PageRank)
        store (Moon client + redb/tantivy fallback, trait Store) · rank (BM25/RRF)
        laya (candle model, head, tokenizer) · daemon (layad, unix socket, tracing)
        cli (laya index|query|hook|mcp) · bench (token/time harness)
```
Release profile: `lto="fat"`, `codegen-units=1`, `panic="abort"`, `strip=true`, mimalloc,
cargo-pgo on the index and query paths; no `target-cpu=native` for distributed builds.

## 4. Open decisions

- **D1 — Laya's role**: final reranker over the stage-2 shortlist (recommended), with a
  Phase-0 spike measuring zero-shot precision@10 and latency on real repos. Fine-tuning
  Laya on code-relevance data would be a later phase if zero-shot is weak.
- **D2 — Moon deployment**: sidecar with embedded fallback (recommended), vs Moon-only, vs
  embedded-only.
- **D3 — Integration depth**: UserPromptSubmit + MCP (recommended) · plus PreToolUse Read
  rewrite · MCP-only.
- **D4 — Semantic embeddings in v1**: BM25 + graph only for v1 (recommended; Laya supplies
  the semantics) vs adding a code bi-encoder now.
- **D5 — Language scope for v1**: core static set plus a line-window fallback (recommended) vs full
  WASM long tail from day one.

## 5. Phasing
0. **Spike** (1–2 d): Laya in candle on 3 repos, then measure P@10, latency on K=32, and the 0.5 threshold's
   calibration. Go/no-go for D1.
1. Indexer + BM25 + CLI `laya query` (red/green TDD, golden-chunk tests per language).
2. Laya stage + Moon store + daemon.
3. Hook + MCP plugin.
4. Benchmark harness, then tune K, threshold and budget against the −50% tokens / −30% time goals.
