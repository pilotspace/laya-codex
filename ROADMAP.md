# laya-codex roadmap

laya-codex hands Claude Code the code a task needs before Claude goes looking for it. Across
three repositories and 60 tasks (benchmark v2), Claude reads **38% less code** and takes **21%
fewer turns**, and its first answers name the right files more often; task time does not change yet
([docs/RESULTS.md](docs/RESULTS.md)). The rest of the road is about three things: turning the
Laya model's better ranking into an end-to-end gain, reaching the time goal, and keeping laya-codex
something people can install in one line and forget about.

## Goals and how they are measured

| goal | target | v0.1.0 (v7: moon, 20 tasks) | v0.1.2 (v8: 3 repos, 60 tasks, pooled) | measured by |
|---|---|---|---|---|
| Code-reading tokens | −50% | **−50.1%** [−61.6, −33.0] | −38.2% [−50.8, −21.8], not met | `bench/stats.py`, `bench/stats_pooled.py` (paired bootstrap) |
| Task wall-clock | −30% | −17.4% [−31.5, −1.0] | +3.5% [−6.2, +14.8], no effect (moon did not replicate v7) | same |
| Answer quality | no loss | recall 0.933 vs 0.975, n.s. | recall 0.899 vs 0.815, +0.083 [+0.028, +0.144], met | `bench/read_accuracy.py` |
| Evidence | 3+ repos, 60+ tasks, edit tasks | 1 repo, 20 tasks | 3 repos (Rust, Python, TS), 60 tasks; no edit tasks yet | benchmark suite |
| Install | one command, < 2 min | `install.sh` (macOS arm64, Linux x86_64) | — | installer test in CI |
| Robustness | hooks never break Claude Code | 151k hook calls, 0 failures (soak) | — | `bench/soak.py` |

## Distribution channels

| channel | audience | status |
|---|---|---|
| `install.sh` from GitHub Releases (laya-codex + pinned Moon + model from Hugging Face, checksums verified) | everyone on macOS arm64 / Linux x86_64 | **v0.1.0** |
| Hugging Face model `tindang/laya-code` | the re-ranker weights | **v0.1.0** |
| Build from source (`cargo build --release -p laya-cli`) | contributors, other platforms | **v0.1.0** |
| Claude Code plugin (`/plugin marketplace add pilotspace/laya-codex`) | Claude Code users — the most direct channel | **v0.1.2** |
| Homebrew tap `pilotspace/tap/laya-codex` | macOS developers | v0.2 |
| `laya-codex upgrade` / `laya-codex uninstall` | existing users | v0.2 |
| Linux aarch64 and static musl builds | servers, containers, Graviton | v0.2 |
| crates.io (`cargo install laya-cli`) | Rust users | after the embedded store removes the Moon sidecar (v0.4) |

## v0.1.x — hardening (patch releases)

Security and robustness items from the v0.1.0 release review that did not block the release.

Done in v0.1.2 (see [CHANGELOG.md](CHANGELOG.md)):

- **Panic isolation:** release builds unwind; panics are caught per request, index job and
  file, and hooks swallow them. Writes to a closed stdout exit quietly.
- **Daemon limits:** 64 connections, 1 MiB request lines, 30 s read and 10 s write
  timeouts; `top_n`, `budget_ms` and the render budget are clamped on the socket path.
- **CLI usability:** `laya-codex index`, `laya-codex query --repo` and `laya-codex init --repo` reject a
  path that does not exist or is not a directory.

Open:

- **Hook permission decision:** the Read and Agent hooks return `permissionDecision: "allow"`
  alongside `updatedInput`. Claude Code still applies deny and ask rules, but `allow` can skip a
  prompt; switch to `updatedInput` without a decision once Claude Code confirms that form.
- **Moon upgrade:** move the pinned Moon from `8bba3ced` to the current release (v0.8.9+) after
  re-running the benchmark and soak test against it; fix Moon's `LICENSE` file so it matches its
  `Cargo.toml` (see [docs/release/LICENSES.md](docs/release/LICENSES.md)).
- **Moon password off the command line:** at the pinned Moon commit an ACL file alone does not
  require authentication, so laya-codex also passes `--requirepass` and the password shows in `ps`.
  Needs Moon to enforce its ACL file (or read the password from a file); then drop the flag.
  Related: `ACL SAVE` against Moon rewrites `moon.acl` with a hashed password laya-codex cannot read.
- **One walker:** re-index-on-edit copies the full walk's ignore settings; export one helper from
  `laya-parse` so they cannot drift.
- **Upgrades:** the installer stops the running daemon; add a version handshake so a new `laya-codex`
  never talks to an old daemon.

## v0.2 — adoption

Make the first five minutes painless and the tool visible where Claude Code users look.

- ~~**Claude Code plugin**~~ — shipped in v0.1.2. Next: list it in the community plugin
  directories. Original plan: package the hooks and the `laya-codex` MCP server as a plugin in a
  marketplace repo, so `/plugin install laya-codex` replaces `laya-codex init` for most users. The binary
  still comes from `install.sh`, and the plugin checks for it and points to the installer.
- **Homebrew tap**, `laya-codex upgrade`, `laya-codex uninstall` (removes hooks, the MCP entry, caches).
- **More platforms:** Linux aarch64, static musl builds, and macOS x86_64 (lexical-only).
- **First-run experience:** `laya-codex init` asks for nothing, indexes in the background and prints
  a one-screen summary. `laya-codex doctor` checks for a newer release.
- **Docs:** a short demo (recorded session before/after), a "how laya-codex decides what to inject"
  page built from the request/response examples, and troubleshooting for every doctor check.
- **Feedback loop:** a `laya-codex report` command that writes an anonymised, local-only session
  summary (tokens injected, spans used) that users can attach to an issue. No telemetry.

## v0.3 — reach the time goal and widen the evidence

- **Close the time gap (no measurable change today → −30%).** Time is spent in the final answer and
  verification turns, after the right code is already in context (forensics §6, v8). Candidates:
  - ~~richer usage lists for the identifiers the answer names~~: follow-ups that ask for tests or
    callers now get test pointers and a 16-line usage list instead of more code (unreleased;
    replay in [RESULTS](docs/RESULTS.md#since-benchmark-v2-what-the-limits-pointed-at));
  - `search` (MCP) as the cheap default for follow-up lookups (called once in v8's 180 sessions);
  - fix third-file misses on multi-file tasks (e.g. the `warm_search.rs` pattern).
  - **Finding (2026-09-24):** wall-clock follows output tokens (≈ 10.8 s per 1,000, R² 0.92 over
    v8's 180 sessions), not reading. laya-codex cut turns 21% but each turn wrote more, so time
    didn't move. Reaching −30% needs about 30% fewer output tokens per session; retrieval alone is
    unlikely to get there. **Decision (2026-09-24): the −30% wall-clock goal stays**, with output
    tokens reported next to it.
- **Per-repo IDF-aware term selection** so generic chunks stop recurring across unrelated tasks.
- **Benchmark v2:** done for localisation tasks (v8: moon, httpx, hono; 60 tasks;
  [docs/RESULTS.md](docs/RESULTS.md)).
  - Pooled, sessions with the Laya model were 13% longer than with lexical-only ranking [+2, +27].
    The model's scoring is ~4% of that; the rest is extra turns, and the sign flips by repository
    (hono −10%). Dropping the 3 worst tasks leaves +7% [−4, +18], not significant.
  - **Decision (2026-09-23): the model stays on by default.** Choosing which code blocks replace
    Claude's own searching and reading is what laya-codex is for, and the model ranks those blocks
    far better offline (MRR 0.702 vs 0.480). The work item is to make that show up end to end:
    fewer verification turns after an injection, a tighter score budget, and a re-run that
    separates model from keywords with more tasks. `LAYA_CODEX_NO_MODEL=1` stays the opt-out.
  - **Moon's append-only log grew to 4.1 GB** during the run and Moon paused writes on a nearly
    full disk. laya-codex should compact it automatically.
  - The fixed ~3.4k-token injection exceeds what stock Claude reads on small repos. The follow-up
    change cuts the injection about 30% per session on every repo (replay). Repo-size caps
    (`LAYA_CODEX_SIZE_BY_REPO`) are opt-in: they dropped correct inlined files on httpx.
  - **Next run** (needs approval, ≈ $50): baseline / this build / this build keywords-only, 60
    tasks, rank mode and output tokens logged, `--rerun-unhealthy` for failed injections.
  - SWE-bench-style edit tasks are still open; a 10-task httpx pilot comes first.
- **Linux performance:** a smaller distilled re-ranker or a CUDA path, so Linux users get the
  model inside the time budget instead of lexical-only.

## v0.4 — no sidecar

- **Embedded store:** run Moon's index in-process (`moon-embed`, architecture D2 v2). No second
  process, no port, no password file; `cargo install` becomes a real channel.
- **Upstream Moon features** laya-codex works around today: OR queries and DEL de-indexing
  ([crates/laya-store/MOON_NOTES.md](crates/laya-store/MOON_NOTES.md)).

## 1.0 — criteria, not a date

- Both goals met with confidence intervals that exclude zero on benchmark v2.
- Answer quality non-inferior on every repo in the suite.
- A plugin install and `install.sh` that work on all supported platforms, tested in CI.
- A stable hook, MCP and CLI surface documented as a compatibility promise.
- No open HIGH-severity security findings; a security policy and a disclosure address.
