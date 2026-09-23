# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased] — 0.1.0

First public release: a proof of concept for ranked code retrieval in Claude Code, supported on
macOS arm64 (Laya on Metal) and Linux x86_64 (Laya on CPU, or lexical-only with
`LAYA_NO_MODEL=1`).

### Benchmark

<!-- TODO: headline numbers from the v0.1.0 paired benchmark (tokens, wall-clock, turns, cost,
     answer recall/precision, n, 95% CIs). Fill in from docs/RESULTS.md when the run finishes. -->
- TODO: code-reading tokens, total input tokens, wall-clock, turns, cost vs stock Claude Code.
- TODO: answer quality (recall / precision) vs baseline.

### Added

- **Tree-sitter chunking** (`laya-parse`): cAST-style AST-aligned chunks of 10–50 lines for 14
  languages (Rust, Python, TypeScript, TSX, JavaScript, Go, Java, C, C++, C#, Ruby, PHP, Kotlin,
  Swift) with symbols, definitions and references (callees, used types, imports); language
  detection, gitignore-aware repo walk, per-file hashing.
- **Moon store** (`laya-store`): Moon (Redis-compatible) sidecar as the BM25 index over a
  resilient pooled RESP client; OR-BM25 fan-out planned by document frequency, circuit breaker,
  chunk references and `chunks_referencing`, and a supervisor that spawns, health-checks and
  stops `moon`.
- **Laya re-ranking** (`laya-model`): native candle inference of the Laya typed-decision model
  (ModernBERT-large + decision head), Metal F16 on macOS and CPU F32 elsewhere, parity-tested
  against the Python reference; prefers the fine-tuned `laya-code` checkpoint, falls back to
  `laya-base`, and to lexical ranking within a time budget.
- **laya-code fine-tuning** (`finetune/`): weakly supervised code-relevance data from git
  history of 8 OSS repos (Moon and pilot-space held out), calibration and held-out evaluation;
  task-scope labels and a zero-shot choice evaluation.
- **Ranking** (`laya-rank`): prompt signal extraction, RRF and weighted Laya fusion, span
  shaping, prose demotion unless the prompt is about docs, compact injection format and guarded
  Read narrowing.
- **Reference expansion**: one-hop expansion over chunk references injects related locations
  (callers, callees, used types) next to the ranked spans.
- **Adaptive session delta**: the daemon tracks which spans a session already has in context,
  skips code already sent, and sizes injected context by predicted task scope and rank; context
  resets on compaction.
- **Prompt stoplist**: agent-instruction boilerplate and English function words are kept out of
  BM25 terms (identifiers and paths never filtered); follow-up prompts are detected by explicit
  back-reference and retrieved together with the session topic.
- **Hooks** (`laya hook`): SessionStart, UserPromptSubmit, PreToolUse (Read/Agent/Task) and
  PostToolUse (Edit/Write re-index) handlers; every hook fails open when the daemon, Moon or the
  model is unavailable.
- **MCP server** (`laya mcp`): `laya_search` tool for follow-up queries.
- **CLI** (`laya`): daemon over a unix socket, incremental indexer, `index`, `query`, `status`,
  `stop`; tuning knobs via `LAYA_*` environment variables.
- **Benchmark harness** (`bench/`): paired Claude Code benchmark with bootstrap statistics,
  retrieval evaluation, read-accuracy analysis and a session timeline view.
- Release engineering: Apache-2.0 `LICENSE` and `NOTICE`, license audit
  (`docs/release/LICENSES.md`), CI (fmt, clippy, tests on macOS arm64 and Linux) and a draft
  release workflow producing tarballs with checksums and third-party licenses; Hugging Face
  package for `laya-code` (`release/hf-laya-code/`).

### Changed

- Laya gate is `Arc`-based (no `unsafe` transmute).
- Tuned defaults from the benchmark: compact rendering, 0.5 fusion weight, 128 state tokens,
  adaptive sizing thresholds.

### Fixed

- Instruction-heavy benchmark prompts no longer outrank the task terms in retrieval.
- Long follow-up prompts are no longer treated as new tasks.

[Unreleased]: https://github.com/pilotspace/laya-codex/commits/HEAD
