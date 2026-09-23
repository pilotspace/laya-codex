<div align="center">

# laya-codex

**Claude Code gets the right code with your prompt: it reads 38% less, takes 21% fewer turns, and
names the right files more often.**

laya-codex indexes your repository on your machine and, before Claude starts each task, gives it the
code that task needs. Claude skips most of the grep-and-open-files hunt.

[![Latest release](https://img.shields.io/github/v/release/pilotspace/laya-codex?label=release&color=2da44e)](https://github.com/pilotspace/laya-codex/releases/latest)
[![CI](https://github.com/pilotspace/laya-codex/actions/workflows/ci.yml/badge.svg)](https://github.com/pilotspace/laya-codex/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![Model on Hugging Face](https://img.shields.io/badge/%F0%9F%A4%97%20model-laya--code-yellow)](https://huggingface.co/tindang/laya-code)
[![Claude Code plugin](https://img.shields.io/badge/Claude%20Code-plugin-d97757)](#install)

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/benchmark-savings-dark.svg">
  <img alt="With laya-codex, Claude Code reads 38% fewer code tokens, takes 21% fewer turns and costs 10% less; total input (−3.7%) and wall-clock time (+3.5%) show no significant change (paired benchmark, 60 tasks on three repositories, 95% confidence intervals)" src="docs/assets/benchmark-savings-light.svg" width="760">
</picture>

</div>

## Install

**macOS (Apple Silicon)** or **Linux x86_64**. Installation takes about a minute, plus the model download on macOS.

**1. Install the `laya-codex` binary**

```sh
curl -fsSL https://raw.githubusercontent.com/pilotspace/laya-codex/main/install.sh | sh
```

**2. Turn it on in Claude Code**, for every repository at once:

```
/plugin marketplace add pilotspace/laya-codex
/plugin install laya-codex@laya-codex
```

That's it. Open Claude Code in any git repository: laya-codex indexes it in the background and starts
helping from the next prompt. To check the setup, run `laya-codex doctor --repo .`.

<details>
<summary>Other ways to set it up</summary>

- **One repository only, without the plugin:** `laya-codex init --repo /path/to/repo` writes laya-codex's hooks into
  that repository's `.claude/settings.local.json` and its MCP server into `.mcp.json`.
- **Share with your team:** choose *project* scope when you run `/plugin install`. That records
  the plugin in `.claude/settings.json`.
- **Homebrew:** installs the same prebuilt binaries. The model is a separate step:
  ```sh
  brew install pilotspace/tap/laya-codex
  curl -fsSL https://raw.githubusercontent.com/pilotspace/laya-codex/main/install.sh | sh -s -- --model-only
  ```
- **Installer options:**
  - `--model-only` fetches and verifies only the model and leaves the binaries alone; use it after `brew install`;
  - `--version vX.Y.Z` installs a specific release;
  - `--dir DIR` installs somewhere other than `~/.local/bin`;
  - `--no-model` skips the ~850 MB model, and laya-codex ranks by keywords alone;
  - `--model` downloads the model on Linux too, where it runs on the CPU and is slow.
- **Build from source:** see [Building from source](#building-from-source).

</details>

## What changes for Claude: one example

**You ask Claude:**

> Bug: `init` writes through a symlinked `.claude` folder into another repo. Find the check and fix it.

| | Stock Claude Code | With laya-codex |
|---|---|---|
| **What Claude has before its first turn** | Only your prompt | Your prompt, plus the function that does the check (`check_target`), its code, and the line that calls it |
| **What Claude does first** | Searches with grep and opens files until it finds the check | Reads the check it was given and starts fixing it |
| **Turn at which a correct file is in context** (benchmark median, 60 tasks) | 5.5 | **0** (46 of 60 tasks) |

What laya-codex added to the prompt, trimmed from real output on this repository:

````markdown
Ranked locations:
1. crates/laya-cli/src/init.rs — 394-425 fn apply; 285-325 fn check_target; …

### crates/laya-cli/src/init.rs:394-425 — fn apply
```rust
pub fn apply(p: &Planned) -> anyhow::Result<()> { …
```

Definitions and uses:
- crates/laya-cli/src/init.rs:288: fn check_target(root: &Path, root_canon: &Path, path: &Path) … — definition of `check_target`
- crates/laya-cli/src/init.rs:472: check_target(root, &root_canon, path)?; — use of `check_target`
````

Across the whole benchmark, stock Claude Code reached a correct file at turn 4 at the earliest.
laya-codex had one in context before Claude's first turn in most tasks:

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/benchmark-journey-dark.svg">
  <img alt="Share of tasks with a correct file in Claude's context by turn: with laya-codex 77% before the first turn (median turn 0); stock Claude Code starts at turn 3 (median 5.5)" src="docs/assets/benchmark-journey-light.svg" width="760">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/benchmark-reads-dark.svg">
  <img alt="Read behaviour: precision 0.59 → 0.62, relevant code found 82% → 91%, first relevant Read at turn 5.9 → 3.3, wasted read tokens 2,005 → 1,187" src="docs/assets/benchmark-reads-light.svg" width="760">
</picture>

**More examples** in [docs/use-cases.md](docs/use-cases.md):
- a follow-up in the same session that doesn't resend code;
- impact analysis before changing a signature;
- a first look at an unfamiliar codebase;
- large files, subagents and teams;
- where laya-codex helps less.

## Why you might want it

- **Claude starts with the right code.** On 46 of 60 benchmark tasks, the code the task needed
  arrived with the prompt, so Claude skipped most of the grep-and-open-files hunt and took 21%
  fewer turns.
- **Longer sessions, lower bills.** Claude reads 38% fewer code tokens, so it uses up the context
  window more slowly, and a task costs 10% less.
- **Better first answers.** On the first question of each task, Claude named the right files more
  often (answer recall 0.82 → 0.90).
- **Nothing to learn.** It runs through Claude Code's own hooks. You keep prompting as usual.
- **Everything stays on your machine.** Indexing and ranking run locally, with no server, account or
  telemetry. Only the code laya-codex adds to a prompt goes to Anthropic, the same way code Claude
  reads with its own tools does.
- **It can't break Claude Code.** Every hook fails open: if laya-codex is missing, stopped or
  slow, Claude Code carries on exactly as it would without it.

## Benchmark

We compared stock Claude Code with Claude Code plus laya-codex on 60 real code-change tasks from
three open-source repositories: [moon](https://github.com/pilotspace/moon) (Rust),
[httpx](https://github.com/encode/httpx) (Python) and [hono](https://github.com/honojs/hono)
(TypeScript), 20 tasks each, none of them used in training. Each task comes from a commit in the
repository's history, and each session asks two questions: where the change goes, then which tests
cover it. Both arms use the same model (Claude Sonnet), and all numbers are paired, with 95%
bootstrap confidence intervals.

| vs stock Claude Code | change | 95% CI |
|---|---|---|
| Code-reading tokens | **−38.2%** | −50.8% … −21.8% |
| Turns | **−20.9%** | −27.7% … −13.7% |
| Cost | −9.9% | −18.7% … −0.4% |
| Total input tokens | −3.7% | −16.9% … +11.9%, not significant |
| Wall-clock time | +3.5% | −6.2% … +14.8%, not significant |
| Answer recall, first question | **0.90 vs 0.82** | difference +0.03 … +0.14 |
| Answer recall, both questions | 0.96 vs 0.93 | difference −0.01 … +0.08, not significant |

Code reading fell on every repository: −37% on moon, −32% on httpx and −47% on hono.

**Limits of this result:**
- **Not faster yet.** Task time did not change. Most of a session goes to checking and writing
  the answer after the right code is already in context (87–90% of it on moon), and laya-codex
  doesn't shorten that part yet. Our −30% time goal is not met.
- **An earlier single-repository run didn't replicate.** The first benchmark (v7 in the results)
  measured −50% reading and −17% time on the same 20 moon tasks; this run measured −37% and +14%.
  Treat any single 20-task result as noisy.
- **Small repositories trade reads for injected code.** Stock Claude already reads little on httpx
  and hono, so the code laya-codex adds roughly cancels what it saves, and total input doesn't fall.
- **Scope.** Tasks that find and explain code, not edits; one model (Sonnet).

Full method, per-repository results, the model-vs-keywords comparison and raw data:
[docs/RESULTS.md](docs/RESULTS.md). To regenerate the charts:
`python3 scripts/charts.py bench/results/headline-v8.json docs/assets`.

## How it works

```
your prompt ─► laya-codex hook ─► local daemon ─► BM25 keyword search + symbol and path matches
                                   │            (Moon index, tree-sitter chunks of 10–50 lines)
                                   ├─► Laya re-ranker: "is this code relevant to this task?"
                                   ▼
          ≤ 9,500 chars added to the prompt: a ranked map, the code of the top 3 files,
          definitions and uses of the names in your prompt, and related callers and callees
```

- **Indexing.** laya-codex splits your code into 10–50-line chunks along function and class boundaries using
  [tree-sitter](https://tree-sitter.github.io/), for 14 languages. Moon, a small local search server that
  laya-codex runs for you, stores the chunks. Edits are re-indexed as Claude makes them.
- **Ranking.** Keyword search picks the 24 best candidates, and laya-codex re-ranks them with the
  [Laya](https://huggingface.co/convaiinnovations/laya) model:
  [laya-code](https://huggingface.co/tindang/laya-code), a code-tuned Laya fine-tune running on the
  Metal GPU, scores how relevant each one is to your task, and the two rankings are blended. If
  the model is busy or absent, laya-codex falls back to keyword ranking alone.
- **Adding code without repeating it.** laya-codex remembers what the session has already seen, so a
  follow-up prompt doesn't get the same code twice. The first whole-file Read of a large file returns
  the most relevant region plus an outline; reading the file again returns all of it.
- **Follow-up search.** Claude also gets an MCP tool, `search` (server `laya-codex`), for follow-up lookups.

What gets injected, when and why, with real hook input and output:
[docs/how-it-works.md](docs/how-it-works.md). Design and decisions:
[docs/architecture.md](docs/architecture.md).

### Why a Laya model on top of keyword search?

Keyword search is fast and finds the right neighbourhood, but it ranks by shared words. A
function that mentions `password` five times outranks the one that actually decides whether a
server is trusted. Picking the right piece of code needs a judgment about the task.

- **It judges relevance directly.** [Laya](https://huggingface.co/convaiinnovations/laya) is a
  decision model: it reads your task and one piece of code together and answers *"is this code
  relevant to this task?"* with a probability. That is a cross-encoder, which reads both texts
  at once. It is more precise than embedding search, where the task and the code are turned
  into vectors separately and only their similarity is compared.
- **It stays cheap.** A cross-encoder is too slow to run over a whole repository, so laya-codex
  uses it only where it counts. Keyword search narrows the repository to 24 candidates, and the
  model scores just those, in about 0.8 s on the Metal GPU. Indexing needs no model and no
  vector database, so a repository indexes in seconds (Moon's source: 485 files in about 1.2 s).
- **It has to be tuned for code.** Laya was trained for triage, moderation and routing, not code,
  and out of the box it ranks code no better than keywords. laya-code is Laya fine-tuned on the
  git history of 8 open-source repositories, where each commit's changed files are the right
  answers for its message. The two repositories used for evaluation were excluded from training.
- **The two rankings are blended, not replaced.** The final score is
  `0.5 × keyword rank + 0.5 × model probability`. The keyword rank keeps documentation and prose
  from crowding out code, and the model reorders the code candidates.

How well each stage ranks the files a real change touched, over the 40 most recent Moon commits
(24 keyword candidates per task, [model card](https://huggingface.co/tindang/laya-code)):

| ranking | MRR (higher is better) | share of the top 10 that is right (P@10) | calibration error (lower is better) |
|---|---|---|---|
| keyword search (BM25) alone | 0.480 | 0.340 | – |
| base Laya, not tuned for code | 0.479 | 0.348 | 0.362 |
| **laya-code** | **0.702** | **0.405** | **0.049** |

On a separate development set, the full blended pipeline reached an MRR of 0.724, against
0.602 for the model alone and 0.678 with the model weighted more heavily.

**What the end-to-end benchmark shows so far.** Benchmark v2 included a keyword-only arm: the
same hooks with the model switched off.
- Over 60 tasks, the model did not put more correct files in front of Claude than keyword ranking
  alone (73 vs 76 files inlined).
- Sessions with the model ran 13% longer (95% CI +2% … +27%). Its own scoring accounts for about
  1 s per prompt, around 4% of a session; the rest came from Claude taking more turns.
- The effect changes sign by repository (hono was 10% *faster* with the model) and between earlier
  runs.

We keep the model on. Choosing which code blocks Claude gets, instead of what it would find by
searching and reading, is the decision laya-codex exists to make, and the model makes it far
better than keywords on ranking quality (table above). Turning that into an end-to-end gain is the
top item on the roadmap. To rank by keywords only, set `LAYA_CODEX_NO_MODEL=1` or install with
`--no-model`; everything else works the same.

## FAQ

<details>
<summary><b>What does it cost to run?</b></summary>

- **Money:** nothing. laya-codex is free and runs locally, and it lowers what you pay Claude (−10% per task in the benchmark).
- **Disk:** about 45 MB for the binaries and about 850 MB for the model.
- **Memory:** the daemon uses about 1 GB of RAM while it is running.

</details>

<details>
<summary><b>Which languages and platforms are supported?</b></summary>

- **Languages:** Rust, Python, TypeScript, TSX, JavaScript, Go, Java, C, C++, C#, Ruby, PHP, Kotlin and Swift.
- **macOS on Apple Silicon:** the full experience, with the model on the Metal GPU.
- **Linux x86_64:** keyword ranking only by default. The model runs on the CPU there, which is too slow to be useful.
- **Other platforms:** build from source.

</details>

<details>
<summary><b>Is my code sent anywhere?</b></summary>

laya-codex itself makes no network calls after installation. The index, the model and the daemon all
live in `~/.cache/laya-codex`, which only your user can read, and the local search server
requires a password that laya-codex generates. The code snippets laya-codex adds to a prompt reach
Anthropic as part of your Claude Code conversation, exactly like code Claude reads with its own
tools.

</details>

<details>
<summary><b>Will it get in Claude's way?</b></summary>

- **It never blocks a tool call** and never fails a prompt: every hook fails open.
- **The first time Claude reads a whole large file**, it gets the relevant region plus an outline; asking again returns the whole file.
- **Only git repositories are indexed automatically.** Opening Claude Code in your home directory indexes nothing.

</details>

<details>
<summary><b>How do I uninstall it?</b></summary>

```sh
laya-codex stop
rm -f ~/.local/bin/laya-codex ~/.local/bin/moon   # or: brew uninstall laya-codex
rm -rf ~/.cache/laya-codex
```

Then, inside Claude Code, run `/plugin uninstall laya-codex@laya-codex`. If you used `laya-codex init`,
also remove laya-codex's entries from that repository's `.claude/settings.local.json` and `.mcp.json`.

</details>

<details>
<summary><b>Something isn't working</b></summary>

Run `laya-codex doctor --repo .`. It checks the binary, the search server and its password, the
model, the daemon, the index and the hooks, and prints a fix for anything that fails. If that
doesn't help, [open an issue](https://github.com/pilotspace/laya-codex/issues) and include its
output.

</details>

## Roadmap

- **v0.2.0, current:** one name everywhere: the CLI is `laya-codex` (was `laya`), env vars are
  `LAYA_CODEX_*`; a Homebrew formula; the plugin, crash isolation and daemon limits from 0.1.x.
- **Next:**
  - turning the model's better ranking into an end-to-end gain (benchmark v2 shows none yet);
  - shortening the checking-and-answering tail, where the time goal is lost;
  - smaller injections on small repositories;
  - compacting Moon's data log automatically (it reached 4.1 GB during the benchmark);
  - a faster model for Linux.

See [ROADMAP.md](ROADMAP.md) for the full plan to 1.0.

## Reference

<details>
<summary><b>Commands</b></summary>

```sh
laya-codex init --repo /path/to/repo      # add hooks and the MCP server to one repository, then index it
laya-codex doctor --repo /path/to/repo    # check everything; prints a fix for each problem (--json available)
laya-codex index /path/to/repo            # incremental; re-run any time
laya-codex query "where is WAL replay implemented" --repo /path/to/repo
laya-codex status | laya-codex stop
```

`laya-codex init` merges laya-codex's hooks and MCP server into the repository's settings:
- **It keeps everything else:** every other key, hook and server is left alone.
- **Re-running it is safe:** a second run changes nothing.
- **It writes the bare `laya-codex` command** when `laya-codex` on `PATH` is the binary you ran
  (after resolving symlinks, so a Homebrew install writes `laya-codex`, not a versioned Cellar path).
- **It refuses symlinks:** it won't write through a symlinked `.claude` directory or settings file.

Flags:
- `--dry-run` prints the result without writing anything.
- `--no-index` skips indexing.
- `--force` replaces a settings file that isn't valid JSON; the original is kept as `*.bak`.

</details>

<details>
<summary><b>Configuration (environment variables)</b></summary>

| var | default | meaning |
|---|---|---|
| `LAYA_CODEX_HOME` | `~/.cache/laya-codex` | socket, logs, Moon data and password (`moon.acl`), models (mode 0700) |
| `LAYA_CODEX_MODEL_DIR` | `laya-code`, else `laya-base` | model directory |
| `LAYA_CODEX_NO_MODEL` | unset | `1` = lexical-only ranking |
| `LAYA_CODEX_BUDGET_MS` | `1200` | Laya time budget per prompt (falls back to lexical) |
| `LAYA_CODEX_RENDER` | `compact` | `full` injects every span's code |
| `LAYA_CODEX_WEIGHT` / `LAYA_CODEX_STATE_TOKENS` / `LAYA_CODEX_K` / `LAYA_CODEX_P_THRESHOLD` | `0.5` / `128` / `24` / `0` | ranking knobs (daemon start) |
| `LAYA_CODEX_ADAPTIVE` | on | `0` = fixed compact injection; default skips code already sent or read in the session |
| `LAYA_CODEX_SCOPE` | off | `1` = let a Laya scope classifier size the injection (measured no-op; see RESULTS) |
| `LAYA_CODEX_MOON_START_SECS` | `30` | how long a freshly started Moon may take to answer |
| `LAYA_CODEX_MOON_PORT` / `LAYA_CODEX_MOON_BIN` | `16379` / `moon` beside the real `laya-codex` binary, else in `../libexec` (Homebrew), else on `PATH` | Moon sidecar; a missing binary is reported with every path tried |
| `LAYA_CODEX_BIN` | unset | the `laya-codex` binary the Claude Code plugin should use |

</details>

<details>
<summary><b>Manual hook setup (what <code>laya-codex init</code> writes)</b></summary>

`.claude/settings.local.json`:

```json
{
  "hooks": {
    "SessionStart":     [{"hooks": [{"type": "command", "command": "laya-codex hook", "timeout": 5}]}],
    "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "laya-codex hook", "timeout": 8}]}],
    "PreToolUse":  [{"matcher": "Read|Agent|Task", "hooks": [{"type": "command", "command": "laya-codex hook", "timeout": 5}]}],
    "PostToolUse": [{"matcher": "Edit|Write|MultiEdit|NotebookEdit", "hooks": [{"type": "command", "command": "laya-codex hook", "timeout": 5}]}]
  }
}
```

and `.mcp.json`: `{"mcpServers": {"laya-codex": {"command": "laya-codex", "args": ["mcp"]}}}` (Claude sees the tool as
`mcp__laya-codex__search`).

</details>

<details id="building-from-source">
<summary><b>Building from source</b></summary>

Requirements:
- **Rust:** 1.90+ (edition 2024).
- **Moon:** a [Moon](https://github.com/pilotspace/moon) binary built with its `text-index` feature. laya-codex uses `LAYA_CODEX_MOON_BIN` if set, else looks beside the (symlink-resolved) `laya-codex` binary, then in `../libexec`, then on `PATH`.
- **Model weights (optional):** `hf download tindang/laya-code --local-dir ~/.cache/laya-codex/models/laya-code`. Without them, laya-codex ranks by keywords alone.

```sh
cargo build --release -p laya-cli        # target/release/laya-codex (fat LTO, mimalloc, Metal on macOS)
cargo test --workspace --release
```

</details>

<details>
<summary><b>Reproducing the benchmark</b></summary>

Pinned commits and task sets are listed in [docs/RESULTS.md](docs/RESULTS.md) (v8). For each
repository:

```sh
python3 bench/run_bench.py tasks --repo <clone> --skip 40 --n 20 [--code-only] --out bench/tasks-v8/<repo>.jsonl
laya-codex index <clone>
python3 bench/run_bench.py run --repo <clone> --tasks bench/tasks-v8/<repo>.jsonl \
    --arms baseline,laya-adaptive,laya-lex --turns 2 --out /tmp/bench/<repo>
```

Then pool the three runs:

```sh
python3 bench/stats_pooled.py laya-adaptive baseline /tmp/bench/moon /tmp/bench/httpx /tmp/bench/hono
python3 bench/read_accuracy.py /tmp/bench/moon bench/tasks-v8/moon.jsonl /tmp/bench/httpx bench/tasks-v8/httpx.jsonl \
    /tmp/bench/hono bench/tasks-v8/hono.jsonl
```

</details>

<details>
<summary><b>Repository layout</b></summary>

| path | what |
|---|---|
| `crates/laya-core` | shared types, `Store`/`Scorer` contracts, code-aware term splitting |
| `crates/laya-parse` | tree-sitter (14 languages) cAST chunker, symbols, repo walk |
| `crates/laya-store` | Moon RESP store: OR-BM25 fan-out, circuit breaker, supervisor, auth |
| `crates/laya-model` | Laya (ModernBERT-large + decision head) in candle, parity-tested |
| `crates/laya-rank` | candidate generation, Laya gate, fusion, span shaping, rendering |
| `crates/laya-cli` | `laya-codex` binary: daemon, hooks, MCP, indexer, init, doctor |
| `plugin/`, `.claude-plugin/` | the Claude Code plugin and its marketplace entry |
| `install.sh`, `scripts/` | installer, chart generator, installer and plugin tests (run in CI) |
| `finetune/`, `spike/` | Laya fine-tuning and the zero-shot spike (Python) |
| `bench/` | paired Claude Code benchmark, retrieval eval, results |

</details>

## License

[Apache-2.0](LICENSE). Moon, the search server laya-codex runs, is distributed under its own license
(shipped with its binary). The laya-code model is Apache-2.0 on
[Hugging Face](https://huggingface.co/tindang/laya-code). Third-party notices:
[NOTICE](NOTICE) and [docs/release/LICENSES.md](docs/release/LICENSES.md).
