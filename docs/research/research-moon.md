# pilotspace/moon — deep-dive for Rust code-indexer cache/embedding/scoring store

Sources checked: GitHub API (`api.github.com/repos/pilotspace/moon`, `/readme`, `/license`, `/releases`, `/tags`, `/contributors`), raw README (531 lines, fetched in full), raw `LICENSE` file, crates.io API for `moondb`. crates.io search for org "pilotspace" was blocked (403 on direct crate-name lookup for bare "moon" — no crate named `moon` exists on crates.io; the published crate is `moondb`, the client SDK). No independent third-party coverage (blog posts, HN, benchmarks by others) was found via the checks run — everything below is first-party (repo + crates.io), so treat all performance and maturity framing as **self-reported**, not independently corroborated.

## 1. What it is

**VERIFIED** (github.com/pilotspace/moon README, fetched 2026-09-23): Moon is a from-scratch Rust reimplementation of a Redis-compatible **server** — RESP2/RESP3 wire protocol, 250+ commands, all standard Redis data types plus native `FT.*` vector + BM25 search, `GRAPH.*` Cypher subset, `TXN.*` cross-store ACID, durable message queues, bi-temporal MVCC, embedded web console.

**VERIFIED** (README "Credits"/"Why Moon"): thread-per-core shared-nothing architecture, `monoio` (io_uring on Linux, kqueue on macOS) with a `tokio` fallback for portability; forkless RDB snapshots (segment-level COW, no `fork()`); per-shard WAL v3 AOF; tiered disk offload (hot data → NVMe under `maxmemory`; idle vector-index segments demote HOT→WARM(mmap)→COLD).

**VERIFIED — it is a server, not an embeddable Rust library.** The only Rust-side artifact on crates.io is `moondb` (crates.io/api/v1/crates/moondb, fetched 2026-09-23), described as "Rust client SDK for Moon — high-performance Redis-compatible server with vector search and graph engine." It is a RESP-protocol **client**, not an in-process store. To use Moon from a Rust indexer you run the `moon` binary as a separate long-lived process (or Docker container) and talk to it over TCP/Unix RESP, same as talking to Redis. There is no `cargo add moon` that gives you an embedded engine.

- Language: Rust, edition 2024 (VERIFIED, README).
- License: **Apache-2.0** — VERIFIED by reading the raw `LICENSE` file directly (`raw.githubusercontent.com/pilotspace/moon/main/LICENSE`), full Apache-2.0 text. Note: GitHub's own API license-detector reports `"key": "other", "spdx_id": "NOASSERTION"` for this repo (checked via `/repos/pilotspace/moon/license`) — likely a `licensee`-tool false negative (non-standard file framing), not a real licensing ambiguity, since the file content is unmodified Apache-2.0. Flagging the discrepancy since automated license scanners in a pipeline may also misflag it.
- Platform: primary target Linux with io_uring; macOS is "first-class development platform" but README explicitly says production should target Linux (Tier 1 aarch64, Tier 2 x86_64) — macOS/dev is not the recommended production tier (VERIFIED, README "Production readiness").

## 2. Maturity, activity, adoption risk

All VERIFIED via GitHub API (fetched 2026-09-23):
- Created 2026-03-26, i.e. **~6 months old**.
- Last push 2026-09-21 (2 days before this research) — actively developed.
- **3 stars, 0 forks, 3 watchers.** Effectively zero external adoption signal.
- 117 open issues on a 6-month-old repo — either heavy self-dogfooding/issue-tracking discipline or real instability; README's own "issue #821", "#772" references confirm active internal issue tracking of benchmark/behavior regressions.
- 29 releases / 30 tags in 6 months (roughly one release per week) — fast-moving, pre-1.0 (current v0.8.9, roadmap targets v1.0 when the production-contract GA checklist is complete — not yet).
- Contributors: a small core team plus dependabot — not yet a broad maintainer base.
- crates.io `moondb` (the only published Rust artifact): 549 total downloads, 268 recent, 4 published versions since 2026-04-20 — negligible external usage.
- No PyPI/npm verification attempted beyond what README claims (`pypi.org/project/moondb/` referenced but not independently checked).

**Adoption risk: HIGH.** Pre-1.0, single-maintainer-scale, zero real-world star/fork traction, and — notably — the README itself is unusually candid about benchmark non-reproduction between runs (e.g., "This run did not reproduce two figures published from the 2026-09-04 run," multiple `[provisional]` SLO caveats). That candor is a **positive integrity signal** (rare for a project this small to self-report regressions/non-repros this openly) but it also confirms the numbers are still moving and self-measured only — no third-party benchmark exists to cross-check against.

## 3. Fit against the four jobs

**(a) content-hash → parsed-chunk cache.** Reasonable fit *functionally* (HSET/GET with TTL, per-key or per-field TTL via `HEXPIRE`), but it's a network-hop KV store, not an embedded map. Every cache lookup from the Rust indexer becomes a RESP round-trip to a sidecar process instead of an in-process `HashMap`/`redb` lookup. For a CLI tool like a Claude Code indexer that should be fast to cold-start and not require an always-on daemon, this is a real operational tax: you must spawn/supervise the `moon` process, manage its port/socket, and handle its own crash-recovery semantics on top of your own.

**(b) chunk embeddings + ANN search.** Best-fitting job for Moon. `FT.CREATE ... SCHEMA emb VECTOR HNSW DIM 384 TYPE FLOAT32 DISTANCE_METRIC COSINE` gives native HNSW ANN with TurboQuant 1–8-bit quantization, hybrid dense+sparse+BM25 fusion, auto-indexing on `HSET` (all VERIFIED, README "Features at a glance" / quick-start example). Self-reported benchmarks claim 1.6–2.3× faster time-to-searchable vs Qdrant and 2.7–3.4× search throughput (VERIFIED as *claimed*, README Benchmarks section, dated 2026-07-08, explicitly flagged in the README itself as "not re-measured on the current tree" — i.e. stale and unverified against the current build even by the vendor's own standard). No independent corroboration found.

**(c) memoizing query → ranked results.** Functionally fits as a KV/sorted-set cache (store query-hash → ranked list, with TTL). Nothing Moon-specific beyond what any KV store gives you; the vector/BM25 hybrid RRF fusion (README) could plausibly compute the ranking itself server-side rather than just cache it, which is a stronger fit than plain caching if the indexer wants server-side hybrid retrieval — but that means moving ranking logic into Moon's query language rather than Rust code.

**(d) incremental reindex on file change.** Partial fit. Moon has `CDC.READ` (polling change stream — VERIFIED) but the push-based `CDC.SUBSCRIBE` channel is explicitly listed as **not yet GA** ("deferred," README "Production readiness"). Auto-indexing on `HSET` means re-upserting a changed chunk's embedding is a single command, and deletion (`HDEL`/`DEL`) is stated to be tier-correct. But there's no file-watching or diffing primitive — that logic stays entirely in the Rust indexer; Moon only stores the result.

**Overall gaps:**
1. **Not embeddable** — requires running/supervising a separate server process; adds a daemon-lifecycle and IPC-latency dependency that an embedded library (redb/sled/LMDB) avoids entirely. For a per-project or per-invocation CLI indexer this is the biggest architectural mismatch.
2. **Pre-1.0, single-maintainer, 3 stars** — real abandonment/breaking-change risk over the project's lifetime; wire protocol and on-disk format are stated LTS as of v0.2 but CLI flags "may evolve until v1.0" (README).
3. **Clustering is alpha** — irrelevant for a local single-node CLI tool, not a real gap here.
4. **All performance numbers are vendor-self-reported and the vendor's own README flags several of them as non-reproduced or provisional** — do not plan capacity around the quoted vector benchmarks without independent re-measurement.
5. 117 open issues on a 6-month repo warrants actually skimming the issue tracker before depending on any specific command (e.g., FT.SEARCH had a stoplist bug zeroing queries, fixed only in v0.8.8 per the roadmap table — i.e. very recently).

## 4. Fallback alternatives (brief)

- **(a) content-hash → chunk cache:** `redb` (pure-Rust embedded KV, ACID, MVCC, no unsafe in the common path, actively maintained, in-process — best fit for a CLI tool) or `sled` (embedded, but effectively in maintenance/stalled-rewrite mode — check current status before choosing) or LMDB via `heed` (mature, battle-tested, mmap-based, in-process, C dependency).
- **(b) chunk embeddings + ANN:** `usearch` (Rust bindings, small/fast HNSW, embeddable, disk-backed) or `hnsw_rs` (pure Rust HNSW, in-process, no server) or `LanceDB` (embedded columnar + vector, Rust-native, growing adoption, on-disk, good for larger corpora) or SQLite + `sqlite-vec` (single-file, in-process, easiest ops story, good enough recall for small-to-mid repo sizes).
- **(c) memoizing query → ranked results:** same embedded KV as (a) (redb/sled/LMDB) keyed on a canonicalized query hash — no need for a separate system.
- **(d) incremental reindex on file change:** not really a storage-layer job — pair any of the above with a content-hash manifest (stored in the same KV) plus a file-watcher (e.g. `notify` crate) in the indexer process itself; `tantivy` is worth naming for BM25/full-text if lexical search is also wanted alongside vector, since it's embedded, mature, and widely adopted in Rust search tooling (unlike Moon, which is BM25 via a network server).

## Unresolved questions
- Whether the `moondb` Python/Rust SDKs support any local/offline mode (e.g., unix-socket-only, no-network) that would reduce the "server process" objection — not checked (out of scope for the four jobs, which are Rust-side).
- No independent (non-pilotspace-authored) source was found confirming any of the benchmark claims; could not verify claims via a third party since none exists in indexed search/GitHub results as of this check.
- Real-world production usage beyond the project's own docs was not found (3 stars, no forks) — could not verify "production-grade" framing against any external deployment.

Status: DONE
