# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

Follow-up prompts get answers instead of more code, the model scores only the candidates that
matter, and a benchmark run now records what it needs to settle the questions benchmark v2 left
open. Replayed offline on the 60 benchmark v2 tasks; not yet measured with Claude.

### Changed
- **Follow-up prompts inline no more code when they ask for tests or callers.**
  - Why: a follow-up ("now find the tests and call sites") is ranked on the session's topic, so
    it used to inline the topic's next-ranked blocks. Replaying the benchmark v2 sessions, the
    second prompt injected as much as the first (about 4.2k characters) and added 5 new correct
    files of 35 (httpx) and 5 of 46 (hono).
  - Now it gets a location list of up to 6 spans not sent yet, the tests that use the task's
    identifiers (``test using `x` ``, at most two per file), and a longer "Definitions and uses"
    list (16 lines). A follow-up that asks for neither gets one code block; one that names code
    (`and replay_wal3?`) keeps the normal sizing.
  - Replay against v0.3.0: the second prompt's injection fell 60–64% (moon 4,554 → 1,842
    characters, httpx 4,131 → 1,474, hono 3,926 → 1,443), 29–32% per session. Correct test files named stayed
    the same (22/23 hono, 12/13 httpx, 2/2 moon); correct files named fell 29 → 27 of 35 on
    httpx and stayed the same on hono and moon.
- **Documentation is listed, not inlined**, unless the task asks about docs or configuration:
  a changelog line shares the task's words but is rarely the code to change. README, CHANGELOG,
  LICENSE, `.md`, `.rst`, `.adoc` and similar files stay in the location list.
- **The model scores the top 16 candidates**, not all 24; the rest keep their keyword order
  below (`LAYA_CODEX_SCORE_TOP`, `0` = all). Replay: the correct files inlined stayed the same
  (63 of 115 on the first prompt, both ways), and the hook's median time per prompt fell from
  0.81–0.97 s to 0.52–0.67 s.
- An injection with no code blocks no longer ends with "Use the code above directly".

### Added
- **Rank mode in the hook log:** each prompt's line records `rank_mode` (`laya`, `laya-partial`
  when the time budget stopped the model early, or `lexical`), `scored`, `offered` and
  `candidates`. Query results carry `scored` and `offered` (additive fields).

### Removed
Unused options, taken out to keep the ranking path to what the benchmarks measured. The injected
context is unchanged: replaying the 60 benchmark tasks (two prompts each) with keyword and
blended ranking gave byte-identical text for every prompt before and after.
- **The task-scope classifier** (`LAYA_CODEX_SCOPE`, `LAYA_CODEX_SCOPE_P`). It was off by
  default: zero-shot macro-F1 was at most 0.28, and even the true scope barely changed what
  loaded. Query replies keep a `scope` field, now always null, so older clients still parse them.
- **Probability-threshold sizing** (`LAYA_CODEX_TAU_FULL`, `LAYA_CODEX_TAU_MAP`). The defaults
  were already zero; full code now always goes to the top-ranked files.
- **Read narrowing.** The first whole-file Read of a large file is no longer cut to one region
  with an outline; in benchmark v3 it narrowed no Read, because Claude reads with offset/limit
  after a Grep. The `PreToolUse` `Read` hook now only records each Read and outputs nothing, so a
  file Claude already read whole is not injected again. Older hooks that still ask for a read
  plan get an error and let the Read through.
- The removed variables are ignored if still set.
- Hook log: Reads are logged as `note_read` (whole file) or `note_ranged_read` (with
  offset/limit), replacing `narrow_read`, `outline_read`, `already_ranged`, `escape_hatch`,
  `small_file`, `too_large`, `unreadable`, `not_narrowed` and `stale_plan`.

### Benchmark tooling
- `run_bench.py run`: `--repeat N` (the stats average a task's repeats before pairing),
  `--max-total-usd`, `--effort` (pins `CLAUDE_EFFORT`), `--rerun-unhealthy`, and arms with their
  own binary (`name[:template][@binary]`, each on its own home and Moon port) to compare builds.
  Rows record the model, effort, versions, rank modes, per-prompt output tokens, answer length,
  turns and time, and whether every prompt got its injection (`injection_ok`).
- `bench/replay_hooks.py` replays the benchmark's two prompts per task through the real hook
  without Claude; `bench/runs.py` holds the shared loaders; `bench/test_bench.py` tests them.
- `stats.py` and `stats_pooled.py` report output tokens, which drive wall-clock time.

## [0.3.0] — 2026-09-24

Better-aimed and steadier ranking, fewer re-checks by Claude, and a debug trace:
- ranking uses the task rather than the instructions around it;
- the Laya model ranks within its time budget even under load;
- inlined code carries trust labels, and at most two files are inlined;
- `laya-codex trace` records what Claude Code and laya-codex exchange;
- a nearly full disk no longer disables laya-codex silently.

Also: benchmark v2 results across three repositories. No breaking changes.

### Added
- **Trust line:** when code is inlined, the injection says it is the exact current content of
  those line ranges and asks Claude not to Read or grep to re-check it. The claim is backed by a
  check: before rendering, the daemon compares each file it is about to inline with its index
  hash, and a file edited since indexing is listed as a location instead of inlined.
- **"All indexed uses shown":** a prompt identifier's definition line carries this label when
  every indexed use is listed or visible in the inlined code, so Claude can skip the grep for call
  sites. It is only claimed for a complete list and is dropped if the 9,500-character cap cuts a
  line.
- **`laya-codex trace`** records what Claude Code and laya-codex exchange, for debugging: per hook
  call, the JSON Claude Code sent and the JSON laya-codex returned; per MCP message, the message and
  the reply; and for both, every daemon call behind it (request, response with ranked spans and
  `p` scores, time). `trace on` / `trace off` (a marker file, so it also reaches plugin hooks) or
  `LAYA_CODEX_TRACE=1|<path>|0`; `trace show [--session S] [--last N] [--full] [--json]
  [--follow]`, `trace status`, `trace clear`. Off by default because a trace contains prompts and
  code; the file is private (0600 in a 0700 directory) and rotates at 64 MiB. Writing a trace
  never changes a hook's output: its errors are ignored.

### Changed
- **At most 2 inlined code blocks** by default, down from 3; 1 for single-function tasks.
  - Why: in benchmark v2 Claude re-checked code it had been given (about 0.5 Reads of an inlined
    block and 1 grep for an already-given symbol per session), and every prompt carried about
    5.8k characters.
  - The third block was the largest (~2.5k characters) and the least often a correct file (22%).
  - Replaying the v2 injections, this cuts them by 25% and loses an inlined correct file on 3 of
    60 tasks; that file stays in the location list.
- **Ranking searches the task, not the instructions around it.** Keyword search and the
  Laya model now see only the task part of a prompt:
  - a quoted passage of 3 or more content words, when there is one;
  - otherwise the prompt without sentences about the answer's format ("Be efficient…",
    "End your answer with … of the form FILES: …").

  Identifiers and file paths still come from the whole prompt. Without this, wrapper words
  such as *source*, *change*, *files* and *paths* could push `CHANGELOG.md` or docs above the
  code. Replaying the 60 benchmark v2 tasks (model on every query), the correct file reached
  the two inlined files in 48 tasks instead of 42 with the benchmark wording, 49 instead of 46
  with reworded instructions, and 49 either way with free-form prompts.

### Fixed
- **A nearly full disk no longer disables laya-codex silently.** Moon pauses all writes when free
  space falls below its floor (5% by default), and every prompt then failed without a trace:
  - ranking sent a create-index command (a write) on every prompt. When Moon refuses it, laya-codex
    now checks that the index exists (a read) and ranks from it, so indexed code keeps arriving;
  - `laya-codex doctor` now tries a small write and reports FAIL, with the fix: free disk space, or
    run Moon with a lower floor;
  - the daemon logs failed requests to `daemon.log` (a repeated message at most once a minute),
    and paused writes read as one short error instead of one line per command.
- `laya-codex doctor` reported FAIL for hooks whose command sets environment variables to paths
  (`LAYA_CODEX_HOME=/… laya-codex hook`): it took the assignments for the program. It now reads
  the command the way the hook matcher does.
- **The Laya model no longer silently drops out under load.**

  *The problem:*
  - Scoring 24 candidates takes about 0.6–1.4 s on an Apple-silicon GPU, depending on the
    prompt's length (it grows with candidates × tokens). The default budget is 1,200 ms.
  - A slow run meant no model ranking at all.
  - A timed-out run also kept the model busy, so the next prompt went straight to keyword
    ranking.

  *The fix:*
  - The model now scores candidates best-first in batches of 8.
  - It starts a batch only if its measured speed says the batch will finish within 85% of the
    budget.
  - The candidates it doesn't reach keep their keyword order below the scored ones.
  - The speed estimate ignores the first run after loading (Metal compiles kernels then), and
    it heals after a skipped batch.

  *Measured on the 60 benchmark v2 tasks, back-to-back queries at the default budget:*
  - **Quiet machine:** same results as before. The model ran on 60 of 60 queries and scored all
    24 candidates in 118 of 123 runs.
  - **Busy GPU:** the model ran on 59 of 60 queries instead of 2 (bench wording) and 0
    (free-form), scoring the top 8. Median query time fell from 1.18–1.21 s to 0.80–0.84 s.
    The correct file reached the two inlined files in 47 tasks instead of 49 with the bench
    wording, and 45 instead of 43 with free-form prompts.
- Adaptive injection could inline code from a file edited outside Claude since the last index
  (for example after `git checkout`); such files are now shown as locations only.
- An inlined block merged from two chunks 1–3 lines apart left out the lines between them while
  its heading claimed the whole range, so line numbers inside the block were off. Inlined code is
  now taken from the file's own lines for the stated range.

### Documentation
- Benchmark v2 results (v8: moon, httpx, hono; 60 tasks) replace the single-repository numbers
  in the README, charts, how-it-works and use cases. Claude reads 38% less code, takes 21% fewer
  turns, costs 10% less and names the right files more often on the first question; task time does
  not change (+3.5%, not significant). The earlier −50% reading / −17% time result on moon did not
  replicate.
- The README states what the model-vs-keywords comparison showed (no end-to-end gain yet, sessions
  13% longer) and why the Laya model stays on by default. A caveat in `docs/RESULTS.md` adds that
  the model probably fell back to keyword ranking on part of that run's prompts, which it didn't
  record, so the comparison is inconclusive.

### Benchmark tooling
- `bench/stats_pooled.py` and `bench/headline.py` for multi-repository runs; `read_accuracy.py`
  pools several run directories; task sets in `bench/tasks-v8/`.
- `scripts/charts.py` draws increases and non-significant changes honestly (grey, left of zero),
  and the journey chart handles tasks that never reached a correct file.


## [0.2.0] — 2026-09-23

One name everywhere. The tool is **laya-codex**; "Laya" now only means the upstream
[Laya model](https://huggingface.co/convaiinnovations/laya) that laya-codex re-ranks code with
(through its fine-tune, [laya-code](https://huggingface.co/tindang/laya-code)).

### Breaking: renamed `laya` → `laya-codex`

This is a clean break: there are no aliases and no fallbacks, so the old names stop working.

| 0.1.x | 0.2.0 |
|---|---|
| command `laya` | `laya-codex` |
| environment variables `LAYA_*` | `LAYA_CODEX_*` (every one, e.g. `LAYA_CODEX_HOME`) |
| hook command `laya hook` | `laya-codex hook` |
| MCP server `laya` in `.mcp.json` | `laya-codex` |
| MCP tool `laya_search` | `search` (Claude sees `mcp__laya-codex__search`) |
| release asset `laya-<version>-<target>.tar.gz` | `laya-codex-<version>-<target>.tar.gz` |
| installer output `laya-install:`, installer variables `LAYA_VERSION`, `LAYA_INSTALL_DIR`, ... | `laya-codex-install:`, `LAYA_CODEX_VERSION`, `LAYA_CODEX_INSTALL_DIR`, ... |
| plugin scripts `laya-hook` / `laya-mcp`, override `LAYA_BIN` | `laya-codex-hook` / `laya-codex-mcp`, `LAYA_CODEX_BIN` |

Unchanged: the data directory `~/.cache/laya-codex` (your index, Moon password and models are
kept), the Moon port 16379, the model name `laya-code`, the plugin `laya-codex@laya-codex`, the
Homebrew formula `pilotspace/tap/laya-codex` and the `moon` release asset.

#### Upgrade guide

1. **Reinstall.**
   ```sh
   curl -fsSL https://raw.githubusercontent.com/pilotspace/laya-codex/main/install.sh | sh
   rm ~/.local/bin/laya
   ```
   The installer stops the running daemon (a 0.1.x one too) and installs `laya-codex` next to
   `moon`. It leaves the old `laya` binary alone and prints the `rm` command for it; if you
   installed with `--dir DIR`, pass the same `--dir` and remove `DIR/laya`. With Homebrew, run
   `brew update && brew upgrade laya-codex`, then `laya-codex stop` so the next hook starts the
   new daemon.
2. **Plugin users**, inside Claude Code: run `/plugin marketplace update laya-codex`, then
   `/plugin update laya-codex@laya-codex`, then restart Claude Code. From a shell:
   `claude plugin marketplace update laya-codex && claude plugin update laya-codex@laya-codex`.
3. **Repositories set up with `laya init`:** run `laya-codex init --repo /path/to/repo`, then
   delete what 0.1.x wrote, which `laya-codex init` does not touch:
   - in `.claude/settings.local.json`, every hook whose command ends in `laya hook` (for example
     `laya hook` or `LAYA_ADAPTIVE=1 /Users/me/.local/bin/laya hook`);
   - in `.mcp.json`, the `"laya"` entry under `mcpServers`.

   Left in place, the old hooks fail on every event because `laya` no longer exists, and they no
   longer make the plugin step aside. `laya-codex doctor --repo /path/to/repo` should then pass
   the `hooks` and `mcp` checks.
4. **Environment variables:** rename every `LAYA_*` you set, in your shell profile, CI,
   `env` blocks of `.claude/settings*.json` or custom hook commands, to `LAYA_CODEX_*`: for
   example `LAYA_HOME` → `LAYA_CODEX_HOME`, `LAYA_NO_MODEL` → `LAYA_CODEX_NO_MODEL`,
   `LAYA_MOON_PORT` → `LAYA_CODEX_MOON_PORT`, `LAYA_BIN` → `LAYA_CODEX_BIN`. Old names are ignored
   without a warning.
5. **Scripts and permissions:** replace `laya <command>` with `laya-codex <command>`, and any
   permission rule or instruction that names the `laya_search` tool with the `search` tool of the
   `laya-codex` server.

### Fixed

- **Homebrew and other symlinked installs:** `laya-codex` looks for `moon` next to its real
  location (after resolving symlinks), then in `../libexec` relative to it, then on `PATH`.
  Before, a symlinked binary never found the Moon installed with it.
- **`init` under Homebrew** writes the bare `laya-codex` command instead of a versioned Cellar
  path that broke on the next `brew upgrade`: the formula now links the binary itself rather
  than a wrapper script, so `laya-codex` on `PATH` resolves to the running binary.
- **`doctor`'s missing-model hint** now points at the laya-code re-ranker: `curl -fsSL
  https://raw.githubusercontent.com/pilotspace/laya-codex/main/install.sh | sh -s -- --model-only`,
  or `hf download tindang/laya-code --local-dir ~/.cache/laya-codex/models/laya-code`. It used to
  suggest downloading the upstream base model.

### Changed

- The Homebrew formula installs `laya-codex` into `bin` and `moon` into `libexec` (homebrew-core's
  unrelated `moon` owns `bin/moon`), and `scripts/update-formula.sh` generates formulas for
  laya-codex releases only.
- The installer stops the daemon with the newly downloaded binary, so an upgrade from any version
  restarts it, and it reports a leftover 0.1.x `laya` binary with the command to remove it.
- Daemon log lines and the injected context header say `laya-codex`.

## [0.1.2] — 2026-09-23

A Claude Code plugin, plus hardening items from the v0.1.0 release review.

### Added

- **Claude Code plugin:** this repository is a plugin marketplace. `/plugin marketplace add
  pilotspace/laya-codex` followed by `/plugin install laya-codex@laya-codex` enables laya's
  hooks and the `laya_search` MCP tool in every repository, without running `laya init` in each.
  The plugin finds `laya` (`LAYA_BIN`, `PATH`, `~/.local/bin`), stays idle with a one-line
  install hint when the binary is missing, and steps aside in repositories that `laya init`
  already set up. `laya doctor` recognises the enabled plugin.

### Changed

- Session start only indexes git repositories automatically, so an everywhere-enabled plugin
  never indexes a home directory or `/tmp`. `laya index <dir>` still indexes any directory.

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

[0.2.0]: https://github.com/pilotspace/laya-codex/releases/tag/v0.2.0
[0.1.2]: https://github.com/pilotspace/laya-codex/releases/tag/v0.1.2
[0.1.1]: https://github.com/pilotspace/laya-codex/releases/tag/v0.1.1
[0.1.0]: https://github.com/pilotspace/laya-codex/releases/tag/v0.1.0
