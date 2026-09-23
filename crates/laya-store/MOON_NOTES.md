# Moon notes: behaviour laya-store relies on or works around

Verified with redis-cli and the integration tests against `pilotspace/moon` at commit
`8bba3ced` (`target/release/moon --shards 1 --appendonly yes`, macOS arm64, 2026-09-22).
Each gap below is written as an upstream feature request / bug report, with what laya-store
does meanwhile.

## What works as needed

- `FT.CREATE idx ON HASH PREFIX 1 <pfx> SCHEMA terms TEXT`. Only schema fields are indexed, so
  the raw `text` field in the same hash is stored but not analysed (no `NOINDEX` needed; it
  is supported for TEXT/TAG/NUMERIC anyway, as are `WEIGHT`, `NOSTEM`, `SORTABLE`, `SEPARATOR`).
- A second `FT.CREATE` fails with `Index already exists`, so `ensure_index` treats that as OK.
- **BM25 is additive over terms.** A doc containing `parse` and `beta`: AND query `parse beta`
  = 4.928537; single-term scores 2.464268 + 2.464268 = 4.928536. So summing single-term
  searches reproduces Moon's own multi-term score (asserted in
  `bm25_ranks_docs_matching_more_terms_first_and_sums_like_and`), which gives exact OR-BM25.
- The analyzer lowercases, stems (`parsed` matches `parse`/`parsing`) and keeps `_`-joined
  identifiers as one token (`parse_config` is a term; `xyz` does not match `xyz_abc`).
- `HSET` on an indexed key re-indexes it: old postings are replaced.
- With `--appendonly yes` the text index (TEXT fields) and the data survive a restart
  (`restoring N text index(es) from sidecar`, then an auto-reindex of existing hashes).

## Gaps and upstream requests

### 1. No OR operator in FT.SEARCH (feature)
`"a | b"` is rejected; queries are AND-only.
**Workaround:** one pipelined single-term `FT.SEARCH` per term, scores summed client-side.
**Request:** disjunctive queries with top-k pruning (WAND / MaxScore). One round trip, and
it avoids scoring every posting of every term.

### 2. FT.SEARCH costs O(df²) per term (performance bug)
`TextStore::search_field` (`src/text/store.rs`, around line 500) finds each candidate's tf with
`posting.doc_ids.iter().position(|id| id == doc_id)`, a linear walk of the roaring bitmap for
every candidate. It also clones the key of every candidate and fully sorts all candidates
instead of keeping a top-k heap. Measured on 10k docs (avg length 431 tokens):

| term df | p50 | p95 |
|---|---|---|
| rare (≈10) | 0.07 ms | 0.10 ms |
| ≈2k | 13.8 ms | 23 ms (p99 588 ms) |
| 10k (in every doc) | 189 ms | 1.16 s |

**Workaround:** laya-store keeps its own `lc:{repo}:df` hash (term → chunks containing it). bm25
looks up the dfs (one `HMGET`), skips terms that were never indexed, and searches the rarest
terms while Σdf² ≤ `df_sq_budget` (default 2e7 ≈ 40 ms). High-df terms carry little idf weight
anyway.
**Request:** iterate the posting list alongside its tf array (or keep tf in doc-id order and
use `rank()`), a top-k heap, and no per-candidate key clone. Also see #3 for a possible
correctness issue in the same code.

### 3. Indexing cost grows with tf × df; tf order may be wrong (performance + possible bug)
`add_term_occurrence` (`src/text/posting.rs`, line 79) does the same linear `position()` scan
for every *repeated* occurrence of a term in a document. The scan crosses the whole posting
list because a new doc id is the largest. 10k chunks index at 124 chunks/s with full tf, 1132/s
when each term is sent once.
Also, `term_freqs` is kept in insertion order while `position()` walks the bitmap in sorted
order. The two agree only while doc ids are inserted in increasing order. If ids are ever
inserted out of order (reuse after removal?), tf values get attached to the wrong docs. The
code comment in `search_field` already notes "term_freqs is in insertion order, NOT sorted
doc_id order".
**Workaround:** index each term at most `max_tf` (default 2) times per chunk: 515 chunks/s.
**Request:** O(1) or O(log n) tf upsert (per-doc tf map or sorted parallel arrays).

### 4. DEL / UNLINK / HDEL / EXPIRE do not remove a document from the text index (bug)
After `DEL key`, `FT.SEARCH` keeps returning the deleted key with its old score, and
`num_docs` does not change. Only `FT.INVALIDATE_RANGE` calls `remove_doc_by_doc_id`.
**Workaround:** before deleting a chunk, `HSET key terms ""` (the re-index drops its postings),
then `DEL`. The empty doc still counts in `num_docs` / avg length until the next restart, when
the index is rebuilt from the keyspace. `get_chunks` also skips ids whose hash is gone.
**Request:** deindex on every key removal path (DEL, UNLINK, HDEL of indexed fields, expiry,
eviction, FLUSH*).

### 5. TAG and NUMERIC fields are lost from the index schema after restart (bug)
After a restart only TEXT fields come back (`FT.INFO` lists only `text_fields`).
`@tag:{v}` queries return 0 results, and `FT.INVALIDATE_RANGE` (which needs TAG + NUMERIC)
deletes nothing.
**Workaround:** `chunks_defining` does not use TAG at all. It keeps an exact, case-sensitive
`SET lc:{repo}:d:{ident}` of chunk ids (plain keyspace, which persists correctly).
**Request:** persist and restore the full schema, including TAG/NUMERIC/VECTOR fields.

### 6. TAG matching semantics differ from RediSearch (compatibility)
- Matching is case-insensitive (`@defines:{beta}` matches `Beta`). Identifiers need exact case.
- No backslash escaping: `@f:{a\-b}` matches nothing, while raw `@f:{a-b}`, `{has space}`,
  `{foo::bar}` and `{op+}` match. `}` and `|` cannot be expressed.
- `{a|b}` is rejected (`multi-tag OR syntax not supported in v1`).

**Request:** optional `CASESENSITIVE`, standard escaping, and multi-value OR.

### 7. FT.* commands are rejected inside MULTI (bug); MULTI persistence looked lossy
Inside `MULTI`, `FT.INVALIDATE_RANGE` returns `ERR unknown command`. In the same session, a
`DEL` executed in a MULTI/EXEC that also contained failing commands was not in the data after
restart (the key came back). Not isolated further.
**Workaround:** no MULTI. Writes are ordered, idempotent pipelines, and the file record is
written last so a partial write is detected (stale hash) and redone.
**Request:** allow FT.* in transactions, and confirm that EXEC with per-command errors still
logs the successful commands to the AOF.

### 8. Existing keys are not backfilled by FT.CREATE (compatibility)
Hashes created before `FT.CREATE` are not searchable until a restart. RediSearch backfills.
laya-store always calls `ensure_index` before writing.

### 9. FT.SEARCH has no default LIMIT (compatibility / safety)
Without `LIMIT`, every match is returned (1200 hits observed). RediSearch defaults to 10.
laya-store always sends `LIMIT 0 n`.

### 10. Reply shape: scores always included (compatibility)
The reply is always `[count, key, [__bm25_score, s], ...]`, with or without `NOCONTENT` /
`WITHSCORES`. `WITHSCORES` is accepted but does not change the shape, unlike RediSearch
(`[count, key, score, ...]`). The parser reads `__bm25_score` from the field array.

### 11. Stop-word-only query is an error, not an empty result (ergonomics)
`FT.SEARCH idx the` → `ERR empty query after analysis`. In a pipeline this is per command;
laya-store treats it (and `no such index`) as "term contributes nothing".
**Request:** return an empty result, or an explicit flag.

### 12. No term statistics command (feature)
There is no way to ask for a term's df. That is why laya-store maintains its own df hash (#2),
which can drift slightly if a bulk pipeline is replayed after a timeout.
**Request:** `FT.TERMSTATS idx term...` → df per term (the DFS pre-pass already computes it).

### 13. Operational gaps
- No `SHUTDOWN`; the supervisor stops moon with SIGTERM (then SIGKILL after 3 s).
- No `SLOWLOG`; `FT._LIST` returned an empty list after a restart while indexes existed.
- `TTL` right after `SET k v EX 1` reports 0 (Redis reports 1). Expiry itself works.
- With `--shards 1`, FT.SEARCH from concurrent clients is serialized: 4 query threads see
  bm25 p50 58 ms vs 12 ms single-threaded.
- Search results can include keys that no longer exist (from #4, and keys that expired before
  a restart were re-indexed from the AOF). Callers must tolerate missing hashes.

## Benchmark (laya-store, M4 Pro, release, 10k chunks, 10-term queries, limit 50)

```
cargo test --release -p laya-store --test bench_it -- --ignored --nocapture
indexed 10000 chunks in 19.43s (515 chunks/s, one put_file per 10-chunk file)
bm25 10 terms, limit 50, per_term 200     p50= 12.1ms  p95= 53.9ms  p99= 61.9ms
bm25 1 term                               p50=  0.12ms p95= 31.1ms  p99= 56.2ms
get_chunks 10                             p50=  0.15ms p95=  0.18ms p99=  0.24ms
chunks_defining 5 idents                  p50=  0.04ms p95=  0.05ms p99=  0.06ms
bm25 10 terms, 4 concurrent threads       p50= 58.1ms  p95=113.8ms  p99=137.4ms
```

Before df planning, a single 10-term query went past the 250 ms query timeout.
