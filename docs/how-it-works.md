# How laya-codex decides what to inject

This page explains what laya-codex adds to a Claude Code session, when it adds it, and how it picks the
code. The examples are real output, not mock-ups.

**How the examples were captured.** The v0.2.0 `laya-codex` binary ran on this repository at
v0.1.2 (`a38c8be`), with a scratch `LAYA_CODEX_HOME`, its own Moon port and
`LAYA_CODEX_NO_MODEL=1`. The hook JSON was piped into `laya-codex hook` the same way Claude Code
sends it. (The hook output is identical to what the v0.1.2 release produced for the same input;
v0.2.0 renamed the CLI, not the ranking.) Without the model the
ranking is lexical only, which keeps the output deterministic. With the model loaded, one more
step (Laya re-ranking, described below) reorders the same candidates, and the output has the
same shape. Paths are shortened to `/home/me/laya-codex`, and long code blocks are trimmed
where marked.

## The short version

laya-codex hooks into five points of a Claude Code session. It fails open: on any error, timeout or
missing index it prints nothing, and Claude Code carries on as if laya-codex were not installed.

| Hook | When it acts | What Claude sees |
|---|---|---|
| `UserPromptSubmit` | every prompt, except slash commands, `#` memory lines and prompts with fewer than 2 content words | up to 9,500 characters: a ranked map of locations, the full code of the top 2 files, definition and use lines, and related code |
| `PreToolUse` `Read` | the **first** whole-file Read of an indexed file of **250+ lines** | the Read is narrowed to the best region, plus an outline of the file. A second whole-file Read passes through untouched |
| `PreToolUse` `Agent`/`Task` | a subagent is launched and this session has a ranking | up to 5 `path:start-end symbol` lines appended to the subagent's prompt |
| `PostToolUse` edits | Edit/Write/MultiEdit/NotebookEdit | nothing. The edited file is re-indexed |
| `SessionStart` | `startup`/`resume` in a git repository; `compact`; `clear` | after compaction, the session's working set is injected again (up to 8 spans). On startup, indexing runs in the background. Folders that are not git repositories are never indexed automatically |

The MCP tool `search` runs the same ranking on demand, for queries Claude chooses itself.

## 1. A prompt comes in

This is the hook input Claude Code sends for a prompt (captured):

```json
{
  "session_id": "doc-session-1",
  "transcript_path": "/tmp/t.jsonl",
  "cwd": "/home/me/laya-codex",
  "permission_mode": "default",
  "hook_event_name": "UserPromptSubmit",
  "prompt": "How does laya decide which files to show for a prompt? Where is the lexical rank fused with the Laya probability?"
}
```

laya-codex uses the git root that contains `cwd` as the repository. It skips the prompt if the prompt
starts with `/` or `#`, is under 3 characters, or has fewer than 2 content terms. Otherwise it
asks the daemon to rank the prompt. The daemon has a time budget (`LAYA_CODEX_BUDGET_MS`, default
1,200 ms) and falls back to the lexical ranking when the budget runs out.

## 2. How the code is ranked

The pipeline has six steps. Each step is a function in `crates/laya-rank`.

1. **Signals** (`signals.rs`). laya-codex pulls three things out of the prompt: BM25 terms, identifiers
   (`snake_case`, `CamelCase`, `foo()`, `a::b`) and path mentions. Instruction boilerplate
   such as "find", "explain", "please" or "comma-separated" is on a stoplist. Otherwise it
   would outrank the task's own words, because BM25 favours the rarest terms.
2. **Candidates** (`retriever.rs`). Three ranked lists come from the index in Moon:
   - BM25 over the tree-sitter chunks (10–50 lines each, cut at AST boundaries), as an OR of
     the terms;
   - the chunks that define the named identifiers;
   - a path-boosted BM25 for mentioned files.

   Reciprocal Rank Fusion (k = 60) merges the three lists, and the top **24** candidates go on.
   Prose and config chunks have their score halved and move behind code, unless the prompt is
   about docs or config.
3. **Laya re-rank** (when the model is loaded). Each candidate gets one question from the
   typed-decision model, a `noul` (yes/no) question:

   ```
   noul question: Is this source code relevant to the software change: "<prompt>"?
   ```

   The state is `file: <path> (lines a-b)` followed by the chunk text, cut to 128 tokens. The
   answer is a calibrated probability **P** that the chunk is relevant. The model is the
   fine-tuned [laya-code](https://huggingface.co/tindang/laya-code), which runs on Metal on Apple
   Silicon. Probabilities are memoised per prompt and chunk, so a repeated prompt skips the model.
4. **Fusion.** Candidates are ordered by

   ```
   score = 0.5 · (1 − lexical_rank / 24) + 0.5 · P
   ```

   `lexical_rank` is the 0-based position after step 2. The lexical term keeps prose and near-miss
   chunks down. P lets a confident chunk climb: in the unit test, a chunk that ranks one place
   lower lexically but has P = 0.9 beats one with P = 0.1. Laya alone, and a weight of 0.7, both
   did worse end to end (MRR 0.602 and 0.678, against 0.724 for w = 0.5).
   `LAYA_CODEX_WEIGHT` changes the weight, and `LAYA_CODEX_WEIGHT=rrf` switches to rank fusion.
   Without the model, or if the model misses its budget, this step is skipped and the result
   says `mode=Lexical`.
5. **Span shaping** (`span.rs`). Adjacent and overlapping chunks from the same file are merged.
   The top 10 spans are kept, with at most 400 lines in total.
6. **Expansion** (`related.rs`). laya-codex follows one hop of references from the top spans: the
   definitions they use and the code that calls them. It also lists the lines that define or use the prompt's
   identifiers, in the style of grep output.

## 3. What gets injected

The rendered block has four parts, in this order, and never exceeds **9,500 characters**. Claude
Code replaces hook output over 10,000 characters with a file preview.

- **Ranked locations**: up to 10 spans, grouped by file.
- **Full code for the top 2 spans, one per file** (1 when the task is about a single function).
  One span from each of two files covers more of a task than two spans from one file. Benchmark v2
  set the number: the third block was the largest and the least often right (22% vs 52% and 33%),
  and dropping it cut the injection by a quarter. Any span that doesn't fit stays in the map
  without its code; code is never cut mid-span.
- **Batch reads** (opt-in, `LAYA_CODEX_BATCH_READS=1`). Claude otherwise Reads around an inlined block, or another listed span of the
  same file, in later turns (in benchmark v2, 43% of its Reads after an injection went to a file
  whose code was already inlined). So the top file's block is widened by 30 lines each side and
  snapped to the indexed chunks at its edges, and one more listed span of each inlined file is
  inlined too, merged when they touch, at most 150 lines per file. The added spans are dropped
  first when the 9,500-character cap is tight. The footer also asks Claude to Read several listed
  locations in one message. It is off by default: in a pilot it enlarged whatever ranked first,
  which was often not the code the task needed, and added text without saving Read turns.
- **Prefetch on Read** (opt-in, `LAYA_CODEX_PREFETCH=1`). On the first Read after a prompt, the
  hook attaches the next ranked code of up to two other files the session does not have yet,
  checked against disk, at most 4,000 characters. It adds `additionalContext` only: the Read and
  its permission are unchanged.
- **Only current code is inlined, and the injection says so.** Before rendering, the daemon
  compares each file it is about to inline with the hash recorded at indexing. A file edited
  since then (for example by `git checkout`, outside Claude) is shown as a location instead. When
  at least one block is inlined, a line before the code tells Claude these are the exact current
  contents and not to Read or grep to re-check them.
- **"All indexed uses shown".** When every indexed use of a prompt identifier is listed (or is
  visible inside the inlined code), its definition line ends with `(all indexed uses shown)`, so
  Claude can skip the grep for call sites. The claim is only made for a complete list: fewer uses
  than the lookup limit, each with a line, none cut by the 9,500-character cap.
- **Definitions and uses**: grep-style `path:line: text` lines. These come before the related
  list because they answer the Grep Claude would otherwise run next.
- **Related by references**: up to 8 one-hop neighbours, each with the reason it was listed.

This is the captured hook output for the prompt above (7,220 characters, captured with v0.2.0,
which still inlined three blocks and had no trust line). Two of the three code blocks are
trimmed:

````
{"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": "…"}}
````

````markdown
<!-- laya-codex: code located for this task by static analysis (tree-sitter chunks + BM25 + Laya relevance model). -->

Ranked locations:
1. crates/laya-rank/src/retriever.rs — 33-81 impl Retriever<'a> > fn query; 361-409 mod tests; 167-214 impl Retriever<'a> > fn laya_gate; 287-328
2. crates/laya-rank/src/read_narrow.rs — 522-556 mod tests; 43-74 impl ReadPolicy > fn read_region; 1-41
3. crates/laya-rank/src/lib.rs — 1-41
4. crates/laya-rank/src/sizing.rs — 290-333
5. crates/laya-rank/src/fusion.rs — 1-19 fn fuse_ranked_lists

### crates/laya-rank/src/retriever.rs:33-81 — impl Retriever<'a> > fn query
```rust
impl<'a> Retriever<'a> {
    …
    pub fn query(&self, repo_id: &str, prompt: &str) -> Result<QueryResult> {
        let start = Instant::now();
        let signals = extract_signals(prompt);
        …
        let fused = fuse_ranked_lists(&[&bm25_ids, &defining_ids, &path_ids], self.cfg.rrf_k);
        …
```

### crates/laya-rank/src/read_narrow.rs:522-556 — mod tests
```rust
    … (trimmed)
```

### crates/laya-rank/src/lib.rs:1-41
```rust
//! `laya-rank`: the retrieval brain. Turns a Claude Code prompt into the top-N ranked code
//! spans, …  (trimmed)
```

Use the code above directly. Search or Read further only for what is still missing, and prefer Read with offset/limit around the listed lines.

Definitions and uses:
- crates/laya-rank/src/read_narrow.rs:706: let o = ReadPolicy::default().outline(&file_chunks(), &sig(""), (51, 50)); — use of `file_chunks`
- crates/laya-rank/src/read_narrow.rs:578: .read_region(Some(&r), "a.rs", &file_chunks(), &sig(""), 1000) — use of `file_chunks`
- … (4 more)

Related by references:
- crates/laya-rank/src/retriever.rs:1-31 — defines `Retriever` (used by #1)
- crates/laya-rank/src/retriever.rs:497-516 mod tests > fn path_mention_boosts_matching_file_via_list_files — calls `query` (#1)
- crates/laya-core/src/lib.rs:168-214 trait Store — defines `Store` (used by #1)
- crates/laya-store/src/store.rs:462-511 impl Store for MoonStore > fn bm25 — calls `query` (#1)
- … (4 more)
````

The lexical-only run shows why the model matters. A test module (`read_narrow.rs:522-556`)
takes one of the three full-code slots because it repeats the prompt's words. With the model,
the P term in step 4 usually pushes such chunks down. Also, `laya_gate`
(`retriever.rs:167-214`), where the fusion happens, is in the map but its code is not inlined,
and `weighted_scores` itself (`retriever.rs:247-285`) is not listed. Re-ranking with the model
is meant to catch misses like these.

## 4. Follow-up prompts: what "adaptive" skips

Adaptive mode is on by default (`LAYA_CODEX_ADAPTIVE=0` turns it off). The daemon remembers, for each
session, which spans it has already sent in full and which files Claude has read whole. It
leaves those out of later injections, and the next-ranked spans move up into the freed slots.

A follow-up that carries almost no task content ("and where is that fusion weight
configured?") or explicitly refers back ("the same", "previous") is not ranked on its own words.
laya-codex ranks the session's **topic** (its last self-contained prompt) instead, plus any
identifiers or paths the follow-up names.

This is the second prompt in the same session (captured):

```json
{ "session_id": "doc-session-1", "hook_event_name": "UserPromptSubmit",
  "prompt": "and where is that fusion weight configured?", "cwd": "/home/me/laya-codex", "…": "…" }
```

The output (7,247 characters, trimmed) has the same map minus what was sent. The full code now
covers the next spans, `retriever.rs:361-409` (with the `weighted_scores` test),
`read_narrow.rs:43-74` and `sizing.rs:290-333`. The new line near the end is:

```
Already provided earlier in this session: crates/laya-rank/src/retriever.rs:33-81, crates/laya-rank/src/read_narrow.rs:522-556, crates/laya-rank/src/lib.rs:1-41
```

For comparison, the same two prompts were run in a new session with `LAYA_CODEX_ADAPTIVE=0` (also
captured). The follow-up got the same three code blocks as the first prompt, byte for byte
(7,220 characters each). If every relevant span is already in context, adaptive mode injects
nothing at all.

The two-prompt benchmarks put a number on this. In v6, with the session delta
(`laya-adaptive`), code-reading tokens fell by 39.4%, against 33.7% for the same injection
without it (`laya-refs`), and total input tokens fell by 17.1% against 4.7%. In v7,
code-reading tokens fell by 50.1% against 48.5%. Compaction or `/clear` resets the record (the
`SessionStart` hook), so code that is no longer in context is sent again.

## 5. Whole-file Reads: the Read plan

When Claude reads a large file whole, laya-codex narrows the **first** such Read to the region that
matters and adds an outline. Captured input and output for `retriever.rs` (773 lines), right
after the prompts above:

```json
{ "session_id": "doc-session-1", "hook_event_name": "PreToolUse", "tool_name": "Read",
  "tool_input": { "file_path": "/home/me/laya-codex/crates/laya-rank/src/retriever.rs" } }
```

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow",
    "updatedInput": {
      "file_path": "/home/me/laya-codex/crates/laya-rank/src/retriever.rs",
      "offset": 28,
      "limit": 192
    },
    "additionalContext": "[laya-codex] crates/laya-rank/src/retriever.rs has 773 lines. Showing lines 28-219 of 773 (best match for the task). Read again with offset/limit for any other part, or Read the whole file again to get all of it.\nOutline (start-end item; * = shown):\n* 1-31 Retriever\n* 33-129 impl Retriever<'a> > fn query\n* 131-165 impl Retriever<'a> > fn path_signal\n* 167-245 impl Retriever<'a> > fn laya_gate\n  247-285 weighted_scores, NON_CODE_INTENT\n  287-328 demote_non_code, path_matches, empty_result\n  330-359 fn call_scorer_bounded\n  361-409 mod tests\n  … (10 more test items)"
  }
}
```

How the plan is chosen (`read_narrow.rs`):

- The file must be indexed and at least 250 lines long. The Read must have no `offset`/`limit`,
  and it must be the first whole-file Read of that file in the session.
- The region comes from the file's spans in the session's last ranking (`basis: ranking`).
  Failing that, laya-codex picks the chunks that share the most distinctive task terms with the file
  (`basis: lexical`). If neither exists, the Read is left alone, so code is never hidden on a
  guess.
- The window covers at most 200 lines: the best candidates (ranked spans, or whole items for the
  lexical basis) plus 5 lines of context. It has to hide at least 100 lines, otherwise the Read
  passes through.
- **Escape hatch:** the same whole-file Read a second time passes through unchanged. In the
  capture, the second Read produced no hook output.

In the `v7` benchmark, 6 whole-file Reads were narrowed. Claude then read other ranges of those
files and never needed the whole file.

## 6. Why this saves tokens

Without laya-codex, Claude Code finds code by searching: Grep, Glob, then whole-file Reads, many of
them of the wrong file. laya-codex front-loads a small, ranked answer (about 1.5–2.5k tokens), so
fewer of those steps happen. From [RESULTS.md](RESULTS.md) (v0.1.0 defaults vs stock Claude
Code, 20 held-out tasks, two prompts per session, paired, Claude Sonnet):

| | stock Claude Code | with laya-codex |
|---|---|---|
| code-reading tokens (Read/Grep/Glob output) | 100% | **−50.1%** (95% CI −61.6%…−33.0%) |
| total input tokens | 100% | −26.8% |
| wall-clock | 100% | −17.4% (CI −31.5%…−1.0%) |
| read precision | 0.373 | 0.546 |
| first Read of a gold file | turn 8.2 | turn 4.0 |
| wasted Read tokens per task | 4,993 | 2,118 |
| answer recall | 0.975 | 0.933 (difference not significant) |

The injected text itself costs tokens. Counting reading and injected tokens together, the saving
is −27.9%. That is why the injection is capped at 9,500 characters and adaptive mode never
re-sends code. Inlining more saved little and cost more.

## 7. Tuning

| Variable | Default | Effect |
|---|---|---|
| `LAYA_CODEX_ADAPTIVE` | on | `0` re-sends spans already in context |
| `LAYA_CODEX_RELATED` | on | `0` drops "Related by references" |
| `LAYA_CODEX_RENDER` | compact | `full` inlines every span (bigger, and not what the benchmark measured) |
| `LAYA_CODEX_NO_MODEL` | unset | `1` gives lexical-only ranking with no model load |
| `LAYA_CODEX_BUDGET_MS` | 1200 | Laya's time budget per prompt. Past it, the lexical ranking is used |
| `LAYA_CODEX_WEIGHT` | 0.5 | Laya's weight in the fusion; `rrf` switches to rank fusion |

## 8. Troubleshooting with `laya-codex doctor`

`laya-codex doctor --repo /path/to/repo` runs eight checks. Each check prints `PASS`, `WARN` or `FAIL`,
and a `fix:` line when there is one. `--json` gives machine-readable output, and `--start`
starts the daemon. The exit status is 1 if any check fails. Here is a captured run on a repo
that is indexed but not yet set up:

```
laya-codex doctor: /home/me/laya-codex
PASS  home    /tmp/lhw is writable
PASS  moon    /home/me/moon/target/release/moon (running)
PASS  auth    laya-codex's password-protected Moon answers on port 16578
PASS  model   disabled (LAYA_CODEX_NO_MODEL=1): lexical-only ranking
PASS  daemon  up at /tmp/lhw/laya.sock (model loading or lexical-only)
PASS  index   207 files indexed for /home/me/laya-codex
FAIL  hooks   laya-codex hooks missing for SessionStart, UserPromptSubmit, PreToolUse, PostToolUse in /home/me/laya-codex/.claude
               fix: laya-codex init --repo /home/me/laya-codex, or install the Claude Code plugin: /plugin marketplace add pilotspace/laya-codex
WARN  mcp     no laya-codex server in /home/me/laya-codex/.mcp.json (the search tool is unavailable)
               fix: laya-codex init --repo /home/me/laya-codex
6 passed, 1 warnings, 1 failed
```

| Check | Symptom | What to do |
|---|---|---|
| `home` | `LAYA_CODEX_HOME` (default `~/.cache/laya-codex`) is not writable | Make it writable, or set `LAYA_CODEX_HOME` to a writable directory. The daemon socket lives there, so keep the path short. A very long path fails with "path must be shorter than SUN_LEN". |
| `moon` | `moon binary not found; tried …` or `not runnable` | laya-codex looks for `moon` beside its own binary (after resolving symlinks), then in `../libexec` next to it (the Homebrew layout), then on `PATH`. Re-run the installer, which puts both in one directory, or `brew reinstall laya-codex`. You can also set `LAYA_CODEX_MOON_BIN=/path/to/moon`. |
| `auth` | port served by another Moon, or `cannot load laya-codex's Moon password` | Another server holds laya-codex's port (default 16379): stop it, or set `LAYA_CODEX_MOON_PORT` to a free port. For a password error, delete `$LAYA_CODEX_HOME/moon.acl` and run `laya-codex stop`; both are recreated. A Moon started by an older laya-codex without a password is replaced at the next daemon start. |
| `model` | `no model found` (lexical-only), `incomplete`, or `runs on CPU here` | Get the re-ranker with `curl -fsSL https://raw.githubusercontent.com/pilotspace/laya-codex/main/install.sh \| sh -s -- --model-only`, which fetches and verifies it into `$LAYA_CODEX_HOME/models/laya-code` (or `hf download tindang/laya-code --local-dir ~/.cache/laya-codex/models/laya-code`), or set `LAYA_CODEX_MODEL_DIR`. On Linux the model runs on CPU and is too slow for interactive use, so set `LAYA_CODEX_NO_MODEL=1`. laya-codex still works lexically. |
| `daemon` | `not running` (WARN), or `running daemon is vX, this binary is vY` | The daemon starts on demand at the first hook, and `laya-codex doctor --start` starts it now. After an upgrade, run `laya-codex stop` so the next hook starts the new build. If it won't come up, see `$LAYA_CODEX_HOME/daemon.log`. |
| `index` | `has no indexed files` or `cannot read the index` | Run `laya-codex index /path/to/repo`. `SessionStart` indexes git repositories in the background, and edits are re-indexed file by file. |
| `hooks` | hooks missing, or `a laya-codex hook runs …, which is not an executable` | Run `laya-codex init --repo …` or install the plugin. If the hook points at a path that no longer exists (for example a versioned Homebrew Cellar path written by an older release), re-run `laya-codex init`; with `laya-codex` on `PATH` it writes the bare command. Hooks written by 0.1.x (`laya hook`) are not recognised: delete them. |
| `mcp` | no `laya-codex` server in `.mcp.json` (WARN) | `laya-codex init --repo …` adds it, and the plugin provides it. Only the `search` tool is missing; the hooks still work. |

When laya-codex adds nothing to a prompt, check these first: whether the prompt is a slash command or
has fewer than 2 content words; whether the repository is indexed (`laya-codex doctor`); and whether
adaptive mode has already sent everything relevant. Set `LAYA_CODEX_HOOK_LOG=/tmp/laya-hook.jsonl` to
log every hook decision, such as `inject`, `skip_prompt`, `already_in_context`, `narrow_read`
or `escape_hatch`.
