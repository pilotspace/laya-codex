# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

Hardening items from the v0.1.0 release review.

### Fixed

- **Panic isolation:** release builds now use `panic = "unwind"` instead of `abort`, so a panic
  on a strange input no longer kills the daemon. A panicking request gets an
  `internal error: ...` reply and the connection keeps serving. A panicking background index
  job is logged and its "indexing" flag is cleared, so the repo can be re-indexed. A file whose
  chunking panics is skipped and the rest of the repo is indexed. The session table recovers
  from a panic that happened while it was locked. A panic inside `laya hook` prints nothing
  and exits 0, so hooks still fail open. Cost: the binary is 0.9 MB larger (+2.8% on macOS
  arm64); indexing speed is unchanged within noise.
- `laya query ... | head` and other writes to a closed stdout exit 0 quietly instead of
  printing `failed printing to stdout: Broken pipe` and aborting.
- `laya index`, `laya query --repo` and `laya init --repo` exit 1 with `<path> does not exist`
  or `<path> is not a directory` instead of indexing or searching an empty repo. The daemon
  also refuses to index a path that is not a directory.

### Security

- **Daemon limits:** at most 64 connections at once; more get an immediate `daemon busy` error
  and are closed without blocking the accept loop. Request lines over 1 MiB get
  `request too long` and the connection is closed before the line is buffered. Each accepted
  socket has a 30 s read and a 10 s write timeout, so idle clients and clients that never read
  their replies are dropped. Accept errors (such as running out of file descriptors) back off
  instead of spinning.
- Size parameters on the socket are clamped: `top_n` to 1–20 (the same range the MCP tool
  uses), `budget_ms` to 60 s and the render budget to 32k tokens.

## [0.1.1] — 2026-09-23

Fixes found by installing v0.1.0 from the public release.

### Fixed

- **Linux:** the v0.1.0 Linux Moon never answered a request, so laya reported that Moon did not
  answer. It was built with Moon's Tokio runtime, whose Linux path waits for per-shard listeners
  that fail to bind. Moon is now built with its default monoio runtime on every platform. That
  runtime also works where io_uring is blocked, as in containers.
- **Installer:** a stalled download was only abandoned after 10 minutes, and the retry appended
  to the partial file, so the checksum check failed the install. Downloads now go to a file
  that curl truncates before retrying, and a transfer slower than 10 KB/s for 30 s is retried.
- A spawned Moon now gets 30 s to answer instead of 3 s, since a large index replay or a slow
  machine needs longer. Set `LAYA_MOON_START_SECS` to change it.

### Added

- The release workflow smoke-tests the built `laya` and `moon` together on macOS and Linux
  (index, query, doctor) before publishing.

## [0.1.0] — 2026-09-23

First public release: a proof of concept for ranked code retrieval in Claude Code, supported on
macOS arm64 (Laya on Metal) and Linux x86_64 (Laya on CPU, or lexical-only with
`LAYA_NO_MODEL=1`).

### Benchmark

v7: default configuration vs stock Claude Code. 20 held-out tasks from pilotspace/moon, two
prompts per session, paired bootstrap 95% CIs, Claude Sonnet, Laya scored cold in every arm
(`docs/RESULTS.md`).

- Code-reading tokens **−50.1%** [−61.6, −33.0]; reading + injected −27.9%; total input −26.8%.
- Wall-clock **−17.4%** [−31.5, −1.0]; turns −23.4%; cost −27.2%.
- Read precision 0.55 vs 0.37. Gold code seen 0.93 vs 0.87. The first relevant Read comes at
  turn 4.0 vs 8.2.
- Answer recall 0.933 vs 0.975: −0.042 [−0.108, 0.000], not significant.
- Goals: −50% reading tokens met on the point estimate; −30% time not met.

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
- **Injection format**: a hard 9,500-character cap on all hook output (Claude Code moves longer
  output to a file). Full code of the top 3 *distinct files*. A grep-style "Definitions and
  uses" list for task identifiers.
- **Read plan**: the first whole-file Read of a large indexed file returns the best region plus
  a file outline; a second whole-file Read returns everything.
- **Setup**: `laya init` (idempotent hook/MCP settings merge) and `laya doctor` (checks with fix
  hints). Daemon autostart is rate-limited across processes. The model is warmed up before it
  serves prompts.
- **Defaults**: adaptive injection on (`LAYA_ADAPTIVE=0` to disable). Scope classifier off
  (`LAYA_SCOPE=1`, a measured no-op).
- **MCP server** (`laya mcp`): `laya_search` tool for follow-up queries.
- **CLI** (`laya`): daemon over a unix socket, incremental indexer, `index`, `query`, `status`,
  `stop`; tuning knobs via `LAYA_*` environment variables.
- **Benchmark harness** (`bench/`): paired Claude Code benchmark with bootstrap statistics,
  retrieval evaluation, read-accuracy analysis and a session timeline view.
- Release engineering: Apache-2.0 `LICENSE` and `NOTICE`, license audit
  (`docs/release/LICENSES.md`), CI (fmt, clippy, tests on macOS arm64 and Linux) and a draft
  release workflow producing tarballs with checksums and third-party licenses; Hugging Face
  package for `laya-code` (`release/hf-laya-code/`).
- **Installer** (`install.sh`): one-line install of `laya` and its pinned Moon sidecar from
  GitHub Releases plus the `laya-code` model from Hugging Face; SHA-256-verified downloads,
  retries and time limits, atomic binary swap, idempotent re-runs, offline test in CI
  (`scripts/test-install.sh`). The release workflow now also builds Moon (pinned commit
  `8bba3ced`, `text-index` enabled) as a separate asset with Moon's own license.
- `ROADMAP.md`: goals, distribution channels and the plan to 1.0.

### Security

- Moon is password-protected: laya generates a password on first start (`$LAYA_HOME/moon.acl`,
  mode 0600), authenticates every connection, and refuses a server on its port that answers
  without the password. An unprotected Moon left by an earlier laya is replaced automatically;
  `laya doctor` has a new `auth` check.
- `$LAYA_HOME` and Moon's data directory are private (0700), the daemon socket is 0600, and
  connections from other users are rejected.
- `laya init` never writes through symlinks or outside the repository (temp files are created
  exclusively and renamed into place); a symlinked `.claude` directory or settings file is now
  refused.
- The Read hook never reads files over 1 MiB or non-regular files; re-index-on-edit applies the
  same gitignore, hidden-file and size filters as a full index.
- CI and release actions are pinned to commit SHAs; release builds use no shared cache.

### Changed

- Laya gate is `Arc`-based (no `unsafe` transmute).
- Tuned defaults from the benchmark: compact rendering, 0.5 fusion weight, 128 state tokens,
  adaptive sizing thresholds.
- Only one daemon runs per `LAYA_HOME` (a lock file); `laya stop` asks the daemon to exit over
  its socket and only signals a verified `laya` process.
- `laya` looks for `moon` beside its own binary first, then on `PATH` (`LAYA_MOON_BIN` still
  overrides both).
- `laya init` writes the bare `laya` command when `laya` on `PATH` is the same binary, so a
  committed `.mcp.json` no longer contains a machine-specific path; it only removes hooks whose
  program is `laya hook`.

### Known limitations

- At the pinned Moon commit the password must also be passed as `--requirepass`, so other local
  users can see it in the process list. Fixing this needs a Moon change (tracked in ROADMAP).
- Linux runs the re-ranker on CPU; the installer skips the model there by default.

### Fixed

- Instruction-heavy benchmark prompts no longer outrank the task terms in retrieval.
- Long follow-up prompts are no longer treated as new tasks.

[Unreleased]: https://github.com/pilotspace/laya-codex/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/pilotspace/laya-codex/releases/tag/v0.1.1
[0.1.0]: https://github.com/pilotspace/laya-codex/releases/tag/v0.1.0
