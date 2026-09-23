# laya-codex roadmap

laya-codex hands Claude Code the code a task needs before Claude goes looking for it. v0.1.0
shows the idea works: **−50% code-reading tokens** and **−17% wall-clock** against stock
Claude Code on 20 held-out tasks ([docs/RESULTS.md](docs/RESULTS.md)). The rest of the road is
about three things: reaching the time goal, proving the effect beyond one repository, and making
laya something people can install in one line and forget about.

## Goals and how they are measured

| goal | target | v0.1.0 | measured by |
|---|---|---|---|
| Code-reading tokens | −50% | **−50.1%** [−61.6, −33.0] | `bench/stats.py`, paired bootstrap |
| Task wall-clock | −30% | −17.4% [−31.5, −1.0] | same |
| Answer quality | no loss | recall 0.933 vs 0.975, n.s. | `bench/read_accuracy.py` |
| Evidence | 3+ repos, 60+ tasks, edit tasks | 1 repo, 20 tasks | benchmark suite |
| Install | one command, < 2 min | `install.sh` (macOS arm64, Linux x86_64) | installer test in CI |
| Robustness | hooks never break Claude Code | 151k hook calls, 0 failures (soak) | `bench/soak.py` |

## Distribution channels

| channel | audience | status |
|---|---|---|
| `install.sh` from GitHub Releases (laya + pinned Moon + model from Hugging Face, checksums verified) | everyone on macOS arm64 / Linux x86_64 | **v0.1.0** |
| Hugging Face model `tindang/laya-code` | the re-ranker weights | **v0.1.0** |
| Build from source (`cargo build --release -p laya-cli`) | contributors, other platforms | **v0.1.0** |
| Claude Code plugin (hooks + MCP server in one `/plugin install`) | Claude Code users — the most direct channel | v0.2 |
| Homebrew tap `pilotspace/tap/laya` | macOS developers | v0.2 |
| `laya upgrade` / `laya uninstall` | existing users | v0.2 |
| Linux aarch64 and static musl builds | servers, containers, Graviton | v0.2 |
| crates.io (`cargo install laya-cli`) | Rust users | after the embedded store removes the Moon sidecar (v0.4) |

## v0.1.x — hardening (patch releases)

Security and robustness items from the v0.1.0 release review that did not block the release.

Done (unreleased, see [CHANGELOG.md](CHANGELOG.md)):

- **Panic isolation:** release builds unwind; panics are caught per request, index job and
  file, and hooks swallow them. Writes to a closed stdout exit quietly.
- **Daemon limits:** 64 connections, 1 MiB request lines, 30 s read and 10 s write
  timeouts; `top_n`, `budget_ms` and the render budget are clamped on the socket path.
- **CLI usability:** `laya index`, `laya query --repo` and `laya init --repo` reject a
  path that does not exist or is not a directory.

Open:

- **Hook permission decision:** the Read and Agent hooks return `permissionDecision: "allow"`
  alongside `updatedInput`. Claude Code still applies deny and ask rules, but `allow` can skip a
  prompt; switch to `updatedInput` without a decision once Claude Code confirms that form.
- **Moon upgrade:** move the pinned Moon from `8bba3ced` to the current release (v0.8.9+) after
  re-running the benchmark and soak test against it; fix Moon's `LICENSE` file so it matches its
  `Cargo.toml` (see [docs/release/LICENSES.md](docs/release/LICENSES.md)).
- **Moon password off the command line:** at the pinned Moon commit an ACL file alone does not
  require authentication, so laya also passes `--requirepass` and the password shows in `ps`.
  Needs Moon to enforce its ACL file (or read the password from a file); then drop the flag.
  Related: `ACL SAVE` against Moon rewrites `moon.acl` with a hashed password laya cannot read.
- **One walker:** re-index-on-edit copies the full walk's ignore settings; export one helper from
  `laya-parse` so they cannot drift.
- **Upgrades:** the installer stops the running daemon; add a version handshake so a new `laya`
  never talks to an old daemon.

## v0.2 — adoption

Make the first five minutes painless and the tool visible where Claude Code users look.

- **Claude Code plugin:** package the hooks and the `laya` MCP server as a plugin in a
  marketplace repo, so `/plugin install laya` replaces `laya init` for most users. The binary
  still comes from `install.sh`, and the plugin checks for it and points to the installer.
- **Homebrew tap**, `laya upgrade`, `laya uninstall` (removes hooks, the MCP entry, caches).
- **More platforms:** Linux aarch64, static musl builds, and macOS x86_64 (lexical-only).
- **First-run experience:** `laya init` asks for nothing, indexes in the background and prints
  a one-screen summary. `laya doctor` checks for a newer release.
- **Docs:** a short demo (recorded session before/after), a "how laya decides what to inject"
  page built from the request/response examples, and troubleshooting for every doctor check.
- **Feedback loop:** a `laya report` command that writes an anonymised, local-only session
  summary (tokens injected, spans used) that users can attach to an issue. No telemetry.

## v0.3 — reach the time goal and widen the evidence

- **Close the time gap (−17% → −30%).** Time is now spent in the final answer and verification
  turns (forensics §6). Candidates:
  - richer usage lists for the identifiers the answer names;
  - `laya_search` (MCP) as the cheap default for follow-up lookups;
  - fix third-file misses on multi-file tasks (e.g. the `warm_search.rs` pattern).
- **Per-repo IDF-aware term selection** so generic chunks stop recurring across unrelated tasks.
- **Benchmark v2:** 60+ tasks over 3+ repositories and languages, plus SWE-bench-style edit
  tasks; enough power to separate Laya from lexical-only and adaptive from fixed injection.
- **Linux performance:** a smaller distilled re-ranker or a CUDA path, so Linux users get the
  model inside the time budget instead of lexical-only.

## v0.4 — no sidecar

- **Embedded store:** run Moon's index in-process (`moon-embed`, architecture D2 v2). No second
  process, no port, no password file; `cargo install` becomes a real channel.
- **Upstream Moon features** laya works around today: OR queries and DEL de-indexing
  ([crates/laya-store/MOON_NOTES.md](crates/laya-store/MOON_NOTES.md)).

## 1.0 — criteria, not a date

- Both goals met with confidence intervals that exclude zero on benchmark v2.
- Answer quality non-inferior on every repo in the suite.
- A plugin install and `install.sh` that work on all supported platforms, tested in CI.
- A stable hook, MCP and CLI surface documented as a compatibility promise.
- No open HIGH-severity security findings; a security policy and a disclosure address.
