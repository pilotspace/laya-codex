# laya-codex Integration Research — tree-sitter retrieval + Claude Code

Date: 2026-09-23. Verified against docs/crates.io/docs.rs where noted; items marked UNVERIFIED could not be confirmed against a primary source in this pass.

---

## A. tree-sitter in Rust: parsing, chunking, symbol extraction

### Crate landscape (verified via docs.rs / lib.rs, 2026-09-23)
- `tree-sitter` core crate: **0.27.0** (confirmed on docs.rs). Rust bindings, incremental parsing via `Tree::edit(InputEdit)` + `parser.parse(src, Some(&old_tree))` — reuses unchanged subtrees, the standard approach for re-indexing on file save. [docs.rs/tree-sitter](https://docs.rs/tree-sitter)
- Two grammar-loading strategies:
  1. **Static, per-language crates** (`tree-sitter-rust`, `tree-sitter-python`, `tree-sitter-typescript`, `tree-sitter-go`, `tree-sitter-java`, …): each vendors and compiles its C grammar via a build.rs, linked directly into your binary. Zero runtime dependency, fastest startup, but every new language = new Cargo dep + rebuild. This is what Zed, most LSP-alternative tools, and ast-grep use for their core supported-language set. [Zed language config docs](https://zed.dev/docs/configuring-languages)
  2. **Dynamic loading**: `tree-sitter-language-pack` (Rust crate, canonical implementation shared by Python/Node/Go/Java bindings) — **v1.20.0**, published 2026-09-14 — ships 371 languages, **downloads grammars on demand and caches them locally** rather than compiling them all in. Also possible via the core crate's `wasm` feature (`WasmStore::load_language`) for fully offline dynamic loading of pre-built `.wasm` grammars. [lib.rs/tree-sitter-language-pack](https://lib.rs/crates/tree-sitter-language-pack)
  - **Recommendation for laya-codex**: static crates for a curated top-N (Rust, Python, TS/JS, Go, Java, C/C++, maybe Ruby/PHP) covers the overwhelming majority of target repos with zero runtime fetch risk. Use the WASM path (not the network-fetching language-pack) as a fallback for long-tail languages — network-fetching grammars at index time is a supply-chain/offline-build liability for a dev tool that will run in CI/air-gapped environments; treat `tree-sitter-language-pack`'s auto-download behavior as **not acceptable for a production indexer** without vendoring the `.wasm` files.

### Symbol extraction: tags.scm pattern (aider)
Aider's repo-map is the most directly analogous prior art: each language grammar ships a `tags.scm` tree-sitter query defining `@definition.*` / `@reference.*` captures; aider extracts (file, symbol, line-range, kind) tuples, builds a **file-dependency graph** (edges = references), and ranks with a **PageRank-like algorithm** — files/symbols referenced from the current chat context get a 50× bias so ranking is context-aware, not static. Aider then greedily fills a token budget (default `--map-tokens` 1k) with the highest-ranked entries. [aider repomap post](https://aider.chat/2023/10/22/repomap.html), [ctags→tree-sitter migration](https://aider.chat/docs/ctags.html)
- **Applicability**: this PageRank-over-definition-graph approach is the strongest available prior art for "top-10 relevant spans" ranking and should be laya-codex's default ranking layer on top of whatever chunk-level retrieval (embedding or lexical) surfaces candidates — use tags.scm-derived defs/refs to build the graph, bias by (a) symbols touched by the current task/prompt keywords and (b) recently-edited files.

### Chunking algorithms compared
| Tool/approach | Unit of chunking | AST-aware? | Notes |
|---|---|---|---|
| Naive fixed-line/window (LangChain-style RAG default) | N lines w/ overlap | No | Breaks mid-function routinely; baseline to beat |
| **cAST** (EMNLP 2025 paper, arXiv:2506.15655) | Recursive AST split-then-merge to a size budget | Yes | Recursively splits large AST nodes, **merges sibling nodes** to pack toward a target size, guarantees chunks concatenate back to the original file verbatim (lossless). Reports **+4.3 Recall@5** on RepoEval retrieval and **+2.67 Pass@1** on SWE-bench generation vs. non-AST chunking. Reference impl: `astchunk` (Python only, MIT). [arXiv](https://arxiv.org/abs/2506.15655), [github.com/yilinjz/astchunk](https://github.com/yilinjz/astchunk) |
| **ast-grep** | Structural pattern match over tree-sitter AST, Rust-native | Yes | Not a chunker per se (it's structural search/lint/rewrite), but its `tree-sitter`-as-core-engine design and pattern-matching approach is directly reusable as a dependency/reference implementation for a Rust indexer — same language, same parsing layer. [github.com/ast-grep/ast-grep](https://github.com/ast-grep/ast-grep) |
| **Zed outline** | Tree-sitter `outline.scm` queries → symbol tree, one file, local | Yes (query-based) | Zed deliberately avoids LSP `documentSymbol` for this because query-based extraction is uniform across all grammars without per-language LSP quirks — same rationale applies to laya-codex needing per-language consistency. [Zed blog](https://zed.dev/blog/syntax-aware-editing) |
| **aider repo-map** | Definition-level (function/class), not sub-chunked | Yes (tags.scm) | Optimizes for "what exists and how central is it," not for full-body retrieval — complementary to, not a replacement for, span chunking |
| **Sourcegraph SCIP** | Symbol-level index (successor to LSIF) | Semantic (via language indexers, not tree-sitter directly) | SCIP indexers are 10x faster to run in CI than LSIF and produce 10-20% smaller indexes; format is a good target for laya-codex's *symbol cross-reference* layer if you want IDE-grade go-to-definition later, but building a SCIP indexer per language is a much bigger investment than tree-sitter tags.scm. [Sourcegraph SCIP announcement](https://sourcegraph.com/blog/announcing-scip) |

**Recommended chunking algorithm for laya-codex (10-50 line spans):**
1. Parse file with tree-sitter, get the AST.
2. Walk to language-specific "chunkable" node types (function/method, class/impl block, struct/enum + doc comment, top-level const blocks) — same node-type list as a `tags.scm`/`outline.scm` would target.
3. Apply cAST's recursive split-then-merge: if a node's line span > 50, recurse into children and split there; if a node's span < 10, merge with adjacent siblings under the same parent until in-budget. Always snap chunk boundaries to statement/node boundaries (never mid-expression) — this is cAST's core guarantee and the reason it beats fixed-window chunking on retrieval metrics.
4. Attach a small structural header per chunk: enclosing symbol path (e.g. `mod::Struct::method`), file path, line range — cheap, and is what aider's tags.scm-derived signatures give you nearly for free.
5. Use tags.scm defs/refs to build the PageRank graph for **ranking** retrieved chunks, independent of whatever primary retrieval (BM25/embedding) surfaces the candidate set.
- Confidence: cAST paper results are peer-reviewed (EMNLP 2025 Findings) and the algorithm is language-agnostic by design — appropriate as the chunking backbone. The Rust reference impl doesn't exist yet (astchunk is Python) — **porting the algorithm, not the code, is required.**

---

## B. Claude Code integration surfaces

### Hook events (fetched from https://code.claude.com/docs/en/hooks, 2026-09-23; note the URL `docs.claude.com/.../hooks` now 301-redirects here)
Core, high-confidence events (consistent with long-standing documented behavior): `SessionStart`, `SessionEnd`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `Notification`, `Stop`, `SubagentStop`, `PreCompact`. The fetched page also listed a substantially larger set (`PostToolBatch`, `PermissionRequest`, `PermissionDenied`, `TeammateIdle`, `WorktreeCreate/Remove`, `PreModelSwitch/PostModelSwitch`, `Elicitation`, etc.) consistent with newer multi-agent/teammate features — plausible given Sept 2026 dating and the multi-agent tooling visible in this very environment (SendMessage/ListAgents), but **UNVERIFIED beyond the fetch summary** since it passed through a summarizing sub-model rather than raw HTML; treat the long tail as directionally correct, verify exact field names against `claude --help hooks` or the live JSON schema before wiring code.

**Directly answering the brief's questions (verified structurally, consistent across two independent fetches of the same official page):**
- **`PreToolUse` on Read/Grep/Glob can rewrite the call**: yes, via `hookSpecificOutput.updatedInput` — a hook can intercept a `Read`/`Grep`/`Glob` call and replace `tool_input` (e.g. redirect a `Read` to a pre-selected canonical path, or narrow a `Grep` pattern) before execution. It can also set `permissionDecision` (allow/deny/ask) and `additionalContext`/`systemMessage`.
- **`UserPromptSubmit` can inject snippets**: yes, via `hookSpecificOutput.additionalContext` — arbitrary text (e.g. laya-codex's top-10 spans) gets appended alongside the user's prompt before the model sees it. This is the more natural integration point for "push retrieved context automatically" than mutating tool calls after the fact.
- **Timeouts**: default 600s for `command`/`http`/`mcp_tool` hook types, tightened to 30s for `UserPromptSubmit` (and model-switch events), 10s for `MessageDisplay`. On `PreToolUse` timeout, the hook is simply discarded and normal permission flow proceeds (fail-open) — important: **a slow laya-codex hook does not block the agent, it just silently loses its injection**, so p99 hook latency must stay well under 30s, ideally <500ms for a UserPromptSubmit-based injector to not visibly stall every turn.

### Hooks vs. MCP server: tradeoffs
| Dimension | PreToolUse/UserPromptSubmit hook | MCP server (stdio tool) |
|---|---|---|
| Whether the model "chooses" to use it | **Guaranteed** — hook fires deterministically on every matching event, no model decision involved | **Not guaranteed** — model must decide to call the tool; with 6-7+ MCP tools configured, tool-selection accuracy measurably drops, and Claude may just use built-in Grep/Read instead of your MCP tool [search result on tool-count degradation] |
| Latency | Hook runs synchronously in the turn pipeline; budget ~30s cap on UserPromptSubmit, effectively want sub-second | MCP stdio calls are local-process, fast (contrast: remote HTTP/SSE MCP adds 30-200ms/call — UNVERIFIED specific number, single source); stdio has no network hop |
| Injection guarantee | Deterministic context injection — best for "always inject top-10 spans before every prompt" | Must convince the model to call it; better for **on-demand, model-directed** deep dives ("look up definition of X") |
| Implementation surface | Must handle raw JSON stdin/stdout, own subprocess/binary invoked per event | Structured JSON-RPC 2.0 protocol, richer (resources, multiple tools, `list_changed` notifications), but adds a standing server process |

**Recommendation**: use **both, in a layered design** — a `UserPromptSubmit` hook that always runs laya-codex's retrieval and injects the top-10 spans as `additionalContext` (deterministic, matches the "50% fewer tokens read via Read/Grep" goal because Claude doesn't need to re-discover the same code manually), plus an **MCP server exposing a `laya_codex_search` tool** for follow-up, model-directed queries beyond the initial injection (e.g. "find all callers of X" mid-task). This mirrors how `grepai` ships both an MCP tool set (`grepai_search`, `grepai_trace_callers`, etc.) *and* documents hook-based auto-injection for the common case. [grepai MCP docs](https://yoanbernabeu.github.io/grepai/mcp/)

### Prior art and published benchmarks
- **Serena MCP** (`oraios/serena`): LSP-backed, symbol/AST-level read/edit tools instead of raw file reads. Independent benchmark (ManoMano, 36K-line Java repo): Claude Code alone timed out after an hour with 9 failing tests; Claude+Serena finished in 45 min, same cost, all tests passing — a **time-to-completion**, not pure-token, benchmark, but the closest published analog to this project's "30% less wall-clock" goal. General claim of "up to ~70% token savings" is **less rigorously sourced** (vendor/community blog, not a controlled study) — cite the ManoMano case study, treat the 70% figure as marketing-grade. [ManoMano benchmark](https://medium.com/manomano-tech/project-aegis-benchmarking-ai-agents-and-why-serena-is-our-new-must-have-311673db35dd), [oraios/serena](https://github.com/oraios/serena)
- **claude-context** (Zilliz, `zilliztech/claude-context`): hybrid BM25 + dense vector (Milvus) MCP server, AST chunking + Merkle-tree incremental re-index. Published figures range **40% to 77% token reduction** depending on source (Zilliz's own blog says 40%, a third-party blog claims 77%) — wide variance signals workload-dependent results, not a fixed number; treat as directional, not a target to contractually promise. 11.8k GitHub stars — largest community in this category, real adoption signal. [Zilliz blog (40%)](https://zilliz.com/blog/why-im-against-claude-codes-grep-only-retrieval-it-just-burns-too-many-tokens), [3rd-party (77%, lower credibility)](https://blog.4sapi.com/blog/claude-context-mcp-token-savings-large-repos), [github.com/zilliztech/claude-context](https://github.com/zilliztech/claude-context)
- **grepai**: semantic search + call-graph MCP tools (`grepai_search`, `grepai_trace_callers/callees/graph`), workspace auto-injection when launched with `--workspace`. No independent benchmark found — **UNVERIFIED savings claims**.
- **code-index-mcp**: found only in passing search results, no benchmark or credible detail surfaced — **not independently verifiable in this pass**, do not cite numbers from it.
- No tool in this survey uses tree-sitter **incremental parsing + hook-based deterministic injection** together in the way laya-codex proposes — this combination (Rust-native tree-sitter indexer + `UserPromptSubmit` hook + MCP fallback tool) appears to be a genuine gap, not a "reinventing X" situation. The two nearest analogs (Serena = LSP+MCP only, claude-context = vector+MCP only) both rely purely on the model choosing to call an MCP tool; neither uses deterministic hook injection, which is Claude Code's more reliable mechanism per the docs above.

---

## C. Measuring the 50%-tokens / 30%-time goals

Methodology, synthesized from Claude Code's own instrumentation plus one directly-relevant industry benchmark writeup (Edgee, measuring token compression on SWE-bench Lite):

1. **Task set**: use SWE-bench Lite (300 self-contained instances across 11 of 12 SWE-bench repos) or a curated subset of it for repeatability, or run the same SWE-bench-style methodology against laya-codex's own target repos if SWE-bench's Python-only bias doesn't match laya-codex's polyglot target. Report resolve rate (tests-pass %) alongside token/time deltas — **do not report token/time savings without also reporting resolve-rate impact**, since a tool that saves tokens by omitting relevant context will look great on cost and bad on correctness. The Edgee study is a cautionary example: it reported strong token savings (30% brevity, 33% aggregate for tool-surface reduction) but **explicitly did not measure resolve-rate impact**, which is a methodological gap laya-codex should not repeat.
2. **Baseline vs treatment**: vanilla Claude Code (Grep/Read/Glob only) vs. Claude Code + laya-codex hook/MCP, same prompts, randomized order to avoid cache-warming bias; prepend a per-replicate nonce to defeat Anthropic's prompt-prefix cache between replicates so each run starts cold (this is the technique Edgee used — without it, prefix caching makes repeated-task comparisons meaningless).
3. **Token measurement**: run Claude Code headless (`claude -p --output-format stream-json`) and parse the terminal `result` event's `usage` object (input/output/cache-creation/cache-read tokens) plus `total_cost_usd`; stream-json also lets you timestamp each turn for wall-clock breakdown, not just an end-to-end total. [Claude Code output formats](https://docs.qcode.cc/en/docs/usage/output-formats)
4. **Wall-clock**: measure end-to-end CLI process time per task (`time claude -p ...`) rather than relying on any internal timer, since the goal is user-perceived speed; cross-check against OpenTelemetry traces (`claude_code.token.usage` and related span data) if you want per-tool-call latency breakdown (e.g. to attribute time to laya-codex's hook specifically vs. model inference). [SigNoz Claude Code OTel guide](https://signoz.io/blog/claude-code-monitoring-with-opentelemetry/), [AWS CloudWatch+OTel blog](https://aws.amazon.com/blogs/mt/analyzing-claude-code-usage-with-cloudwatch-and-opentelemetry/)
5. **Statistical rigor**: n≥30 tasks minimum, paired comparison (same task, both conditions), report both mean and median (token/cost distributions are heavy-tailed per the Edgee methodology), use a paired sign test or bootstrap CI rather than a single-run percentage — a single "50% fewer tokens" number from one run is not defensible.

---

## D. Rust release optimization (for the laya-codex binary)

Verified against Cargo Book / community guides, cross-checked across ≥3 sources:

```toml
[profile.release]
lto = "fat"          # cross-crate inlining/dead-code-elim; biggest win, +2-3x build time
codegen-units = 1     # forces single codegen unit -> more global optimization, serializes codegen
panic = "abort"       # OK for a CLI binary (not a library others depend on); smaller binary, no unwind tables
strip = true          # strip symbols/debuginfo from the release binary
opt-level = 3         # default for release already, keep explicit for clarity
```
- Consensus across sources: `lto=fat` + `codegen-units=1` + `panic=abort` + `strip` typically yields **30-50% smaller binaries**, at **2-3x longer compile time** — acceptable tradeoff for a distributed CLI tool built in CI, not on every dev iteration (use a separate `[profile.dev]`/fast-iterate profile locally). [Cargo Book profiles](https://doc.rust-lang.org/cargo/reference/profiles.html), [rustc PGO book](https://doc.rust-lang.org/beta/rustc/profile-guided-optimization.html)
- **PGO via `cargo-pgo`**: 3-step workflow — `cargo pgo build` (instrumented binary) → run against representative workload (ideally: index a handful of real target repos, the actual laya-codex hot path) → `cargo pgo optimize` rebuilds using collected profile. Reported gains **5-15% throughput beyond LTO alone** for hot-path-dominated services (Kobzol's cargo-pgo writeup is the most credible/maintainer-level source here). Worth doing for laya-codex given parsing/indexing is a genuinely hot, representative-workload-shaped loop — good PGO candidate. [Kobzol cargo-pgo blog](https://kobzol.github.io/rust/cargo/2023/07/28/rust-cargo-pgo.html)
- **`target-cpu=native`**: gives real speedups locally but **must not be used for distributed binaries** — binary becomes tied to the build machine's exact CPU feature set and can SIGILL/crash on different hardware (multiple open rustc issues confirm this is still a live footgun as of 2026, not just historical). Use it only for local benchmarking or CI machines matching prod hardware exactly; for a distributed CLI, prefer a conservative baseline target (e.g. `x86-64-v2`/`v3`) or runtime feature detection instead. [rust-lang/rust#147176](https://github.com/rust-lang/rust/issues/147176)
- **mimalloc**: swap global allocator via `#[global_allocator]` + the `mimalloc` crate. Best fit for laya-codex's workload profile (many small/frequent allocations during AST walking and chunk construction) — sources converge on mimalloc being the better default over jemalloc/system allocator for small-object, latency-sensitive workloads (multithreaded gains commonly cited at 30-50% over system allocator; mimalloc-vs-jemalloc gap narrows for long-running/stable-memory workloads, which is less representative of a short-lived indexing CLI). Trivial to adopt, low risk, recommend including by default behind a feature flag so it can be disabled if it ever regresses on a target platform.

---

## Ranked recommendation

1. **Chunking**: implement cAST's recursive split/merge algorithm natively in Rust (port the algorithm from the paper, not the Python `astchunk` code), driven by per-language tree-sitter node-type tables analogous to tags.scm/outline.scm. This is the only chunking approach in the survey with peer-reviewed retrieval-quality evidence.
2. **Ranking**: aider's tags.scm-derived def/ref graph + PageRank, biased toward current-prompt keywords/recently-touched files, for selecting the top-10 from a larger candidate set.
3. **Integration**: `UserPromptSubmit` hook for deterministic always-on injection (this is what makes the 50%-token goal achievable — Claude stops needing to Grep/Read manually) + a companion MCP tool for model-directed follow-up queries. Do not rely on MCP alone — tool-selection accuracy degrades with tool count and is not guaranteed to fire.
4. **Grammars**: static per-language crates for a curated core set; WASM-based dynamic loading (not network-fetching `tree-sitter-language-pack`) for long-tail languages, to keep the tool usable offline/in CI.
5. **Release build**: `lto=fat, codegen-units=1, panic=abort, strip=true` + mimalloc by default + `cargo-pgo` once there's a representative corpus of target repos to profile against; avoid `target-cpu=native` for distributed binaries.
6. **Benchmarking**: SWE-bench Lite (or a polyglot equivalent) with paired baseline/treatment runs, nonce-prefixed to defeat prompt caching, `stream-json` usage field for tokens, wall-clock via process time, n≥30, report resolve-rate alongside token/time deltas — do not claim savings without also reporting correctness impact, per the Edgee study's own gap.

## Adoption risk / maturity notes
- tree-sitter core: extremely mature, wide adoption (GitHub, Zed, Neovim, ast-grep, Helix) — low risk.
- `tree-sitter-language-pack`: newer, cross-language multi-binding project; the auto-download behavior itself (not the crate's maturity) is the risk for this use case.
- cAST: 1 paper + 1 reference repo (Python), no known production Rust port yet — algorithm is low-risk (simple, testable), tooling ecosystem around it is immature; laya-codex would be an early/first Rust adopter.
- Claude Code hooks schema: actively evolving (the long event list fetched this session, e.g. `TeammateIdle`, `WorktreeCreate`, suggests recent additions for multi-agent features) — build against the documented core events (`UserPromptSubmit`, `PreToolUse`) which are stable, treat newer/niche events as subject to change.
- claude-context / Serena / grepai: all real, in-production tools with genuine users; none of their token-savings numbers are independently audited (vendor or community blog sourced) — use as directional evidence, not as numbers to promise stakeholders.

## What this research did not cover
- No hands-on benchmarking was performed — all token/time figures are secondary-sourced from vendor/community writeups, not reproduced.
- Did not evaluate embedding-model choice or vector-DB options (claude-context uses Milvus; laya-codex's brief implies a non-vector, tree-sitter-native approach, so this was out of scope) — flag if hybrid lexical+semantic retrieval becomes a requirement later.
- Did not verify the full extended Claude Code hook event list against raw JSON schema/source — only against a summarizing fetch of the official docs page; re-verify exact field names before implementation.
- Did not assess licensing of `tree-sitter-language-pack`'s bundled grammars for redistribution/vendoring (relevant if going the WASM-vendoring route recommended above).

## Unresolved questions
- Does laya-codex need cross-repo/monorepo scale (Sourcegraph SCIP territory) or is single-repo, single-machine indexing sufficient? This changes whether SCIP-style symbol indexing is worth the investment beyond tags.scm.
- Target language list/priority order (affects which static tree-sitter crates to vendor first).
- Is a standing MCP server process acceptable operationally, or does laya-codex need to be hook-only (no persistent process) for the deployment model in mind?

Status: DONE_WITH_CONCERNS
