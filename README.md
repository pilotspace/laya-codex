# laya-codex

Ranked code retrieval for Claude Code. `laya` indexes a repository with tree-sitter into
10–50-line AST-aligned chunks, stores them in [Moon](https://github.com/pilotspace/moon)
(BM25), re-ranks candidates with the [Laya](https://huggingface.co/convaiinnovations/laya)
typed-decision model running natively in Rust (candle, Metal), and hands Claude Code the most
relevant spans through hooks and an MCP tool.

**Measured effect** (20 held-out tasks, paired, Claude Sonnet, all significant): −45% code-reading
tokens, −42% total input tokens, −25% wall-clock, −37% turns, −29% cost, with answer recall
0.95 vs 0.94 and read precision 0.66 vs 0.55. See [docs/RESULTS.md](docs/RESULTS.md) for the
full evidence, ablations and caveats.

## How it works

```
prompt ──UserPromptSubmit hook──► laya daemon ──► Moon BM25 (+ symbol/path signals, RRF)
                                     │                  │ top 24 chunks
                                     │            Laya re-rank (Metal, ~0.8 s, memoized in Moon)
                                     ◄──── ranked map + top-3 spans injected as context
Read/Agent/Edit hooks: guarded Read narrowing · subagent hand-off · re-index on edit
MCP tool `laya_search` for follow-up queries
```

Architecture and decisions: [docs/architecture.md](docs/architecture.md). Build notes and
verified facts: [docs/build-context.md](docs/build-context.md).

## Requirements

- Rust 1.90+ (edition 2024); macOS arm64 for Metal (Linux runs the model on CPU — too slow
  for interactive re-ranking; set `LAYA_NO_MODEL=1` there).
- A Moon server binary (`moon` on PATH or `LAYA_MOON_BIN`); `laya` starts and supervises it.
- Model weights: the fine-tuned re-ranker (preferred),
  `hf download tindang/laya-code --local-dir ~/.cache/laya-codex/models/laya-code`
  ([model card](https://huggingface.co/tindang/laya-code)), or the base model,
  `hf download convaiinnovations/laya --local-dir ~/.cache/laya-codex/models/laya-base`.
  Without either, laya runs lexical-only.

## Build

```sh
cargo build --release -p laya-cli        # target/release/laya (fat LTO, mimalloc, Metal on macOS)
cargo test --workspace --release
```

## Use

```sh
laya init --repo /path/to/repo           # enable laya in Claude Code for this repo + start indexing
laya doctor --repo /path/to/repo         # check Moon, model, LAYA_HOME, daemon, index, hooks
```

`laya init` merges the four hooks into `<repo>/.claude/settings.local.json` and the `laya` server
into `<repo>/.mcp.json`, using the absolute path of the `laya` binary you ran. It keeps every other
key, hook and server and only replaces earlier laya entries, so re-running it is safe (and a
no-op). Flags: `--dry-run` prints the result without writing, `--adaptive` turns on adaptive
context sizing (`LAYA_ADAPTIVE=1` on the hook command), `--no-index` skips indexing, `--force`
replaces a file that is not valid JSON (the original is kept as `*.bak`; without `--force` such
files are left untouched and init exits 1). Any path inside the repo works; the git root is used.
Re-run `laya init` if you move the binary. Claude Code asks once to approve the project MCP server.

`laya doctor` prints PASS/WARN/FAIL with a fix for each problem and exits 1 if anything fails
(`--json` for scripts, `--start` to start the daemon if it is down). Every check has a time limit.

```sh
laya index /path/to/repo                 # incremental; re-run any time
laya query "where is WAL replay implemented" --repo /path/to/repo
laya status | laya stop
```

Manual setup (what `laya init` writes) — `.claude/settings.local.json`:

```json
{
  "hooks": {
    "SessionStart":     [{"hooks": [{"type": "command", "command": "laya hook", "timeout": 5}]}],
    "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "laya hook", "timeout": 8}]}],
    "PreToolUse":  [{"matcher": "Read|Agent|Task", "hooks": [{"type": "command", "command": "laya hook", "timeout": 5}]}],
    "PostToolUse": [{"matcher": "Edit|Write|MultiEdit|NotebookEdit", "hooks": [{"type": "command", "command": "laya hook", "timeout": 5}]}]
  }
}
```

and `.mcp.json`: `{"mcpServers": {"laya": {"command": "laya", "args": ["mcp"]}}}`.
Every hook fails open: if the daemon, Moon or the model is unavailable, Claude Code runs unchanged.

## Configuration (env)

| var | default | meaning |
|---|---|---|
| `LAYA_HOME` | `~/.cache/laya-codex` | socket, logs, Moon data, models |
| `LAYA_MODEL_DIR` | `laya-code`, else `laya-base` | model directory |
| `LAYA_NO_MODEL` | unset | `1` = lexical-only ranking |
| `LAYA_BUDGET_MS` | `1200` | Laya time budget per prompt (falls back to lexical) |
| `LAYA_RENDER` | `compact` | `full` injects every span's code |
| `LAYA_WEIGHT` / `LAYA_STATE_TOKENS` / `LAYA_K` / `LAYA_P_THRESHOLD` | `0.5` / `128` / `24` / `0` | ranking knobs (daemon start) |
| `LAYA_ADAPTIVE` | unset | `1` = size injected context by task scope, skip spans already sent (`laya init --adaptive`) |
| `LAYA_MOON_PORT` / `LAYA_MOON_BIN` | `16379` / `moon` on `PATH` | Moon sidecar; a missing binary is reported with every path tried |

## Reproduce the benchmark

```sh
python3 bench/run_bench.py tasks --repo <moon clone> --skip 40 --n 20 --out bench/tasks.jsonl
laya index <moon clone>
python3 bench/run_bench.py run --repo <moon clone> --tasks bench/tasks.jsonl \
    --arms baseline,laya-compact,lex-compact --out /tmp/bench-run
python3 bench/stats.py /tmp/bench-run laya-compact
```

## Layout

| path | what |
|---|---|
| `crates/laya-core` | shared types, `Store`/`Scorer` contracts, code-aware term splitting |
| `crates/laya-parse` | tree-sitter (14 languages) cAST chunker, symbols, repo walk |
| `crates/laya-store` | Moon RESP store: OR-BM25 fan-out, circuit breaker, supervisor |
| `crates/laya-model` | Laya (ModernBERT-large + decision head) in candle, parity-tested |
| `crates/laya-rank` | candidate generation, Laya gate, fusion, span shaping, rendering |
| `crates/laya-cli` | `laya` binary: daemon, hooks, MCP, indexer |
| `finetune/`, `spike/` | Laya fine-tuning and the zero-shot spike (Python) |
| `bench/` | paired Claude Code benchmark, retrieval eval, results |
