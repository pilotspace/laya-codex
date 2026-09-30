<div align="center">

# laya-codex

### Claude Code gets the right code with your prompt, so it stops hunting for it.

laya-codex indexes your repository on your machine and, before Claude starts each task, hands it
the code that task needs. Claude skips most of the grep-and-open-files hunt.

[![Latest release](https://img.shields.io/github/v/release/pilotspace/laya-codex?label=release&color=2da44e)](https://github.com/pilotspace/laya-codex/releases/latest)
[![CI](https://github.com/pilotspace/laya-codex/actions/workflows/ci.yml/badge.svg)](https://github.com/pilotspace/laya-codex/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![Model on Hugging Face](https://img.shields.io/badge/%F0%9F%A4%97%20model-laya--code-yellow)](https://huggingface.co/tindang/laya-code)
[![Claude Code plugin](https://img.shields.io/badge/Claude%20Code-plugin-d97757)](#install)

| **−14%** | **−19%** | **5.9 → 2.3** | **73% → 88%** |
|:---:|:---:|:---:|:---:|
| cost per session | time per session | tool calls per session | right files named in Claude's first answer |

<sub>Benchmark v13 vs stock Claude Code: 60 paired tasks on three open-source repositories, Claude
Sonnet 5.5; cost, time, turns and first-answer recall all significant at 95%. The trade-off: code read plus injected is
+10% (not significant). <a href="#benchmark">Full results and limits</a></sub>

**[Install](#install)** · **[What changes](#what-changes-for-claude)** · **[How it works](#how-it-works)** · **[Benchmark](#benchmark)** · **[FAQ](#faq)**

</div>

## What changes for Claude

Without laya-codex, Claude starts every task with only your prompt and searches its way to the
code. With it, the code arrives with the prompt:

```mermaid
flowchart TB
    subgraph stock["Stock Claude Code"]
        direction LR
        s1(["Your prompt"]) --> s2["Grep ×4"] --> s3["Read, open files"] --> s4(["Answer"])
    end
    subgraph laya["With laya-codex"]
        direction LR
        l1(["Your prompt<br/>+ the ranked code it needs"]) --> l2["about 2 lookups<br/>(search, Grep or Read)"] --> l4(["Answer"])
    end
    stock ~~~ laya
    classDef stockNode stroke:#8b949e,stroke-width:2px
    classDef layaNode stroke:#2da44e,stroke-width:2px
    class s1,s2,s3,s4 stockNode
    class l1,l2,l4 layaNode
```

| per session, benchmark v13 (60 tasks) | stock Claude Code | with laya-codex |
|---|---|---|
| **What Claude has before its first turn** | only your prompt | your prompt, plus the ranked functions, their definitions and their callers |
| **Tool calls** | 5.9 (4.2 of them Grep) | **2.3** (0.8 Grep, 0.8 laya-codex `search`) |
| **Turn at which a correct file is in context** (median) | 4, and never in 19 of 60 tasks | **0**, before the first turn, in 52 of 60 tasks |
| **Cost / wall-clock time** | $0.128 / 23.2 s | **$0.110 / 18.7 s** |

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/benchmark-journey-dark.svg">
  <img alt="Share of 60 tasks with a correct file in Claude's context by turn (benchmark v13): with laya-codex 52 of 60 before the first turn (median turn 0); stock Claude Code median turn 4; most laya-codex tasks never needed to Read a correct file because it arrived with the prompt" src="docs/assets/benchmark-journey-light.svg" width="760">
</picture>

### One example

> **You ask Claude:** Bug: `init` writes through a symlinked `.claude` folder into another repo.
> Find the check and fix it.

Stock Claude Code greps and opens files until it finds the check. With laya-codex, Claude's first
turn already has the function that does the check (`check_target`), its code, and the line that
calls it, so it starts on the fix.

<details>
<summary>What laya-codex added to the prompt (trimmed from real output on this repository)</summary>

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

</details>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/benchmark-reads-dark.svg">
  <img alt="Finding the right code, benchmark v13, 60 paired tasks, stock vs laya-codex: relevant code found 50% → 78%, median turn the right code arrives 4 → 0, Reads on relevant code 70% → 76%, wasted read tokens 278 → 102" src="docs/assets/benchmark-reads-light.svg" width="760">
</picture>

**More examples** in [docs/use-cases.md](docs/use-cases.md): a follow-up in the same session that
doesn't resend code, impact analysis before changing a signature, a first look at an unfamiliar
codebase, large files, subagents and teams, and where laya-codex helps less.

## Why you might want it

| | |
|---|---|
| **Claude starts with the right code** | The code a task needs arrives with the prompt: 2.3 tool calls per session instead of 5.9, 19% less time. |
| **Lower bills** | A session costs 14% less (95% CI −18% … −9%), with 32% fewer input tokens and 26% fewer output tokens, so the context window fills more slowly. |
| **Better first answers** | On the first question of each task, Claude named the right files more often (answer recall 0.73 → 0.88). Over both questions, recall did not change significantly (0.95 → 0.93). |
| **Nothing to learn** | It runs through Claude Code's own hooks. You keep prompting as usual. |
| **Everything stays on your machine** | Indexing and ranking run locally, with no server, account or telemetry. Only the code laya-codex adds to a prompt goes to Anthropic, the same way code Claude reads with its own tools does. |
| **It can't break Claude Code** | Every hook fails open: if laya-codex is missing, stopped or slow, Claude Code carries on exactly as it would without it. |

It is not free: the injected code (about 1.8k tokens per session) makes code read plus injected
+10% pooled, and +22% when each repository counts equally. See [the limits](#what-limits-this-result-and-what-is-next).

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

## How it works

```mermaid
flowchart LR
    repo[("Your repository")] -->|"tree-sitter<br/>14 languages"| chunks["10–50-line chunks<br/>along functions and classes"]
    chunks --> moon[("Moon<br/>local keyword index")]
    prompt(["Your prompt"]) --> hook["laya-codex hook"]
    hook --> lex["Keyword search<br/>BM25 + symbol + path matches<br/>24 candidates"]
    moon --> lex
    lex --> rerank["Laya re-ranker<br/>'is this code relevant to this task?'<br/>top 16 in about 0.5 s"]
    rerank --> inject["Up to 9,500 characters added to the prompt<br/>ranked map · code of the top 2 files<br/>definitions, uses, callers and callees"]
    inject --> claude(["Claude Code"])
    claude -.->|"MCP search:<br/>where is X, who calls X"| lex
```

- **Indexing.** laya-codex splits your code into 10–50-line chunks along function and class
  boundaries using [tree-sitter](https://tree-sitter.github.io/), for 14 languages. Moon, a small
  local search server that laya-codex runs for you, stores the chunks. Edits are re-indexed as
  Claude makes them.
- **Ranking.** Keyword search picks the 24 best candidates, and laya-codex re-ranks the top 16 with
  [laya-code](https://huggingface.co/tindang/laya-code/tree/r1), a code-tuned fine-tune of the
  [Laya](https://huggingface.co/convaiinnovations/laya) model running on the Metal GPU. The two
  rankings are blended. The model scores the candidates best-first and stops inside its time
  budget, so a slow or busy machine re-ranks fewer of them rather than none; the rest keep their
  keyword order below. Only if the model is absent, or scores nothing in time, does keyword ranking
  stand alone.
- **No repeats.** laya-codex remembers what the session has already seen, so a follow-up prompt
  doesn't get the same code twice, nor code from a file Claude already read whole. Claude's own
  Reads are never changed.
- **Lookups without Grep.** Claude also gets an MCP tool, `search` (server `laya-codex`). Given a
  name (or `a|b`, optionally with a `path`), it returns every line that uses it, grouped by
  function or test, with the definition marked and a note saying whether the list is complete. So
  one call answers "where is it defined, who calls it, which tests cover it". Given a description in
  words, it returns ranked code.

What gets injected, when and why, with real hook input and output:
[docs/how-it-works.md](docs/how-it-works.md). Design and decisions:
[docs/architecture.md](docs/architecture.md).

### Why a Laya model on top of keyword search?

Keyword search is fast and finds the right neighbourhood, but it ranks by shared words: a function
that mentions `password` five times outranks the one that actually decides whether a server is
trusted. Picking the right piece of code takes a judgment about the task, so laya-codex splits the
work:

```mermaid
flowchart LR
    all["Whole repository"] -->|"keyword search"| k["24 candidates"]
    k -->|"Laya scores the top 16<br/>about 0.5 s on Metal"| blend["final score =<br/>0.5 × keyword rank<br/>+ 0.5 × model probability"]
    blend --> top["the code Claude gets"]
```

- **It judges relevance directly.** Laya is a decision model: it reads your task and one piece of
  code together and answers *"is this code relevant to this task?"* with a probability. That makes
  it a cross-encoder. It is more precise than embedding search, which turns the task and the code
  into vectors separately and compares only their similarity.
- **It stays cheap.** A cross-encoder is too slow to run over a whole repository, so it only scores
  the keyword candidates. Indexing needs no model and no vector database, so a repository indexes
  in seconds (Moon's source: 485 files in about 1.2 s).
- **It is tuned for code.** Out of the box, Laya ranks code no better than keywords. laya-code is
  Laya fine-tuned on git history, where the code a commit changed is the right answer for its
  message.
- **The rankings are blended, not replaced.** The keyword rank keeps documentation and prose from
  crowding out code, and the model reorders the code candidates.

How well each stage ranks the files a real change touched, over the 40 most recent Moon commits:

| ranking | MRR (higher is better) | share of the top 10 that is right (P@10) | calibration error (lower is better) |
|---|---|---|---|
| keyword search (BM25) alone | 0.480 | 0.340 | – |
| base Laya, not tuned for code | 0.479 | 0.348 | 0.362 |
| **laya-code** | **0.702** | **0.405** | **0.049** |

**The model does not yet show an end-to-end gain.** Better ranking has not yet turned into
significantly lower cost or better recall in full sessions (benchmarks v10 and v12). We keep it on
because choosing which code Claude gets is the decision laya-codex exists to make, and turning the
model's better ranking into an end-to-end gain is the top roadmap item. To rank by keywords only,
set `LAYA_CODEX_NO_MODEL=1` or install with `--no-model`; everything else works the same.

<details>
<summary>Model details and the model-vs-keywords evidence</summary>

- **Which model is installed.** The MRR table above measures the **first** laya-code (revision
  `f3d6bd2`, [model card](https://huggingface.co/tindang/laya-code/tree/f3d6bd2344e4750dd917f95d40ceacfa81bb81db)),
  scoring 24 keyword candidates per task. The version this release installs is revision `25f97e5`
  on the `r1` branch of [tindang/laya-code](https://huggingface.co/tindang/laya-code/tree/r1). It
  was retrained on fixed commits of 7 repositories, using the exact candidate lists laya-codex's
  retriever produces. The benchmark repositories and a second evaluation repository were excluded
  from training. The installer pins that revision; the repository's `main` branch still holds the
  first model.
- **Offline replay.** The retrained model inlines 70 of 115 gold files, against 63 for the first
  model and 62 for keywords ([its model card](https://huggingface.co/tindang/laya-code/blob/r1/README.md)).
  On a separate development set, the full blended pipeline reached an MRR of 0.724, against 0.602
  for the model alone and 0.678 with the model weighted more heavily.
- **Benchmark v10** included a keyword-only arm: the same build with the model switched off. The
  model ranked all 102 of its arm's prompts. Over 51 tasks, no measure differed significantly:
  cost −4.0% (−11.2% … +4.3%), code read plus injected −1.6% (−11.1% … +8.2%), wall-clock −4.2%
  (−19.1% … +12.3%), first-question recall 0.90 vs 0.91.
- **Benchmark v12** repeated the comparison on Sonnet 5.5 over 60 tasks: again no significant
  difference in cost (−0.2%, −3.1% … +2.9%) or first-question recall (0.90 vs 0.88); sessions with
  the model took 5% longer (+1.0% … +9.9%). 51–60 tasks are too few to show the offline gain in
  sessions.
- **Earlier runs** (benchmark v2) found the model 13% slower, but did not record how many prompts
  it actually ranked; see
  [docs/RESULTS.md](docs/RESULTS.md#caveat-the-model-may-not-have-ranked-every-prompt).

</details>

## Benchmark

We compared stock Claude Code with Claude Code plus laya-codex on 60 real code-change tasks from
three open-source repositories that were not used in training:

- **Repositories:** [moon](https://github.com/pilotspace/moon) (Rust),
  [httpx](https://github.com/encode/httpx) (Python) and [hono](https://github.com/honojs/hono)
  (TypeScript).
- **Tasks:** each task comes from a commit in the repository's history. Each session asks two
  questions, as you would type them in one live session: where the change goes, then which tests
  cover it.
- **Setup:** both arms use the same model (Claude Sonnet 5.5, `claude-sonnet-5-5`, at medium
  effort, Claude Code 2.1.284). This is benchmark v13 (2026-09-30): laya-codex 0.4.0 plus the
  changes since (the per-file candidate cap and request capture), with the laya-code-r1 model,
  all 60 tasks, no stalls.
- **Statistics:** all numbers are paired, with 95% bootstrap confidence intervals. Grey in the
  chart means not significant.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/benchmark-savings-dark.svg">
  <img alt="laya-codex vs stock Claude Code: cost, code tokens reaching Claude and turns. Paired benchmark v13, 60 tasks on moon, httpx and hono, claude-sonnet-5-5 at medium effort, 95% confidence intervals: cost −13.7%; code read plus injected +9.5% (not significant); code-reading tokens −57.0%; turns −45.5%; total input tokens −31.7%; output tokens −25.6%; wall-clock time −19.2%" src="docs/assets/benchmark-savings-light.svg" width="760">
</picture>

| vs stock Claude Code, per session | stock | laya-codex | change | 95% CI |
|---|---|---|---|---|
| Cost (goal: −50%) | $0.128 | $0.110 | **−13.7%** | −17.8% … −9.1% |
| Code read + code injected | 2,755 | 3,017 | +9.5% | −1.6% … +22.6%; +21.5% (+9.5% … +36.8%) with repos weighted equally: up on httpx and hono, down on moon |
| Code-reading tokens | 2,755 | 1,184 | **−57.0%** | −65.2% … −47.8% |
| Total input tokens | | | **−31.7%** | −37.3% … −25.0% |
| Output tokens | | | **−25.6%** | −28.9% … −22.2% |
| Wall-clock time (goal: −30%) | 23.2 s | 18.7 s | **−19.2%** | −23.3% … −14.7% |
| Turns (Claude Code's count: tool calls + one per prompt) | 7.9 | 4.3 | **−45.5%** | −49.8% … −40.7% |
| Tool calls | 5.9 | 2.3 | | Grep 4.2 → 0.8; laya-codex `search` 0.8 |
| Answer recall, first question | 0.73 | 0.88 | **+0.15** | +0.08 … +0.23 |
| Answer recall, both questions | 0.95 | 0.93 | −0.02 | −0.08 … +0.02, not significant |

### By repository

laya-codex helps most where finding code is hard. Stock Claude already finds httpx code in a few
calls, so there the injected code costs about what it saves:

| change vs stock | moon (Rust) | hono (TypeScript) | httpx (Python) |
|---|---|---|---|
| Cost (95% CI) | **−22.3%** (−28.0% … −15.4%) | **−11.0%** (−18.1% … −2.7%) | +4.2% (−4.3% … +13.9%), not significant |
| Wall-clock time | **−28.7%** | **−20.5%** | −4.5%, not significant |
| Turns | **−58.9%** | **−45.7%** | **−27.6%** |
| Code-reading tokens | **−72.0%** | **−44.9%** | **−38.1%** |
| Code read + injected | **−22.6%** | +28.9% | +58.2% |

Bold marks a significant improvement; the increases on hono and httpx are significant too.

With each repository weighted equally, cost falls 9.7% (−13.9% … −4.9%). Every per-repository
interval is in [docs/RESULTS.md](docs/RESULTS.md#benchmark-v13-2026-09-30-sonnet-55-one-live-session-per-task).

### Where the saving comes from

The saving comes from fewer API calls (3.7 per session instead of 5.9), not from less code
reaching Claude. The injected code replaces tool output roughly one for one, and every call it
avoids saves a re-read of the conversation and an answer. At list prices, laya-codex's bill is 64%
cache writes (the new text each turn), 12% cache re-reads and 24% output.

**Two benchmark bugs.** The first was fixed before this run: earlier runs resumed the session for
the second prompt (`claude -p --resume`). When laya-codex's code let Claude answer the first prompt
in one call, the resumed request missed the prompt cache and paid to rewrite the whole
conversation, in about half of the laya-codex sessions and none of stock's. v12 measured cost
+0.6% with it; in v13, without it, stock's cost did not move ($0.128 both runs) while
laya-codex's fell from $0.129 to $0.110. The second was found after this run: every session in
both arms, v13 included, loaded the operator's personal `~/.claude/CLAUDE.md`. How we found them:
[the write-up](docs/blog/2026-09-30-measuring-honestly.md).

### What limits this result, and what is next

| limit | why | next |
|---|---|---|
| **Cost**: −13.7%; our −50% goal is not met | Most of a session's bill is Claude Code's own context and Claude's answers, which both arms pay; laya-codex saves the calls it avoids (2.3 instead of 5.9) | Offline checks of the next ideas found ≤ 1% each (skipping low-confidence code, injecting follow-up lookups); the goal itself is under review |
| **Tokens** (reported beside cost): code read + injected +9.5%, not significant | The injection (about 1.8k tokens per session) replaces the reading it saves roughly one for one | Showing less of each block made Claude read more (benchmark v11) |
| **Time**: −19.2%; our −30% goal is not met | Time follows how much Claude writes (7.6 s per 1,000 output tokens, R² 0.82) | Fewer, shorter answers |
| **httpx**: cost +4%, not significant | Stock Claude already finds httpx code in a few calls; Laya is least sure there (all 8 first-prompt injections it scored below p 0.1 were httpx) | — |
| **Model vs keywords**: no end-to-end difference | laya-code-r1 inlines more right files offline (70 of 115 vs 62 for keywords), but sessions show no significant difference in cost or recall (v10, v12; [details](#why-a-laya-model-on-top-of-keyword-search)) | — |
| **Scope**: tasks that find and explain code, not edits; one model per run | — | An edit-task pilot |

<details>
<summary>More charts: time follows output, and follow-ups carry less code</summary>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/insight-time-dark.svg">
  <img alt="Scatter of 120 benchmark v13 sessions: wall-clock time rises about 7.6 s per 1,000 output tokens (R² 0.82), with stock and laya-codex sessions on the same line" src="docs/assets/insight-time-light.svg" width="760">
</picture>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/insight-followup-dark.svg">
  <img alt="Characters added to the second prompt, offline replay: moon 4,554 → 1,964, httpx 4,131 → 1,650, hono 3,926 → 1,629 (v0.3.0 → v0.4.0)" src="docs/assets/insight-followup-light.svg" width="760">
</picture>

To regenerate the charts:

```sh
cd scripts
python3 charts.py ../bench/results/headline-v13.json ../docs/assets
python3 insight_charts.py ../bench/results/claude-v13 ../docs/assets --arm laya --run-label "benchmark v13" \
    --before ../bench/results/replay-2026-09-24/ab-rank.jsonl v030 v0.3.0 \
    --after ../bench/results/replay-2026-09-26-r1/replay.jsonl candidate v0.4.0
```

</details>

The full report, with every number, the method, per-repository results, the model-vs-keywords
comparison, raw data and the plan for the next run: [docs/RESULTS.md](docs/RESULTS.md).

## FAQ

<details>
<summary><b>What does it cost to run?</b></summary>

- **Money:** nothing. laya-codex is free and runs locally, and it lowers what you pay Claude (−13.7% per session in benchmark v13, 95% CI −17.8% … −9.1%).
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

**What laya-codex records locally.** Each time it ranks code for a prompt or a `search`, it appends one
line to `~/.cache/laya-codex/capture/requests.jsonl`, readable only by you. The line holds:
- the prompt and the session id;
- the file and line range of each candidate, with its keyword rank and the Laya model's score;
- which blocks it inlined.

It never holds the code itself. The records stay on your machine and are never uploaded. They exist
so laya-codex can be measured on real sessions and its model retrained. To stop recording, run
`laya-codex capture off`; it takes effect at once, and `laya-codex capture status` shows the state.
Delete the folder to remove what was recorded.

</details>

<details>
<summary><b>Will it get in Claude's way?</b></summary>

- **It never blocks a tool call** and never fails a prompt: every hook fails open.
- **It never changes Claude's own searches or Reads.** It only notes which files Claude read.
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
model, the daemon, the index and the hooks, and prints a fix for anything that fails.

To see exactly what Claude Code and laya-codex exchanged, turn on the trace:

```sh
laya-codex trace on                  # record every hook call and MCP message (off by default)
laya-codex trace show                # what was asked, what the daemon ranked, what went back
laya-codex trace show --full --last 1   # the exact JSON in and out, including the injected code
laya-codex trace show --follow       # watch live while you use Claude Code
laya-codex trace off && laya-codex trace clear
```

The trace stays on your machine, in `~/.cache/laya-codex/trace/` (readable only by you), but it
contains your prompts and code, so review it before attaching it to an
[issue](https://github.com/pilotspace/laya-codex/issues).

</details>

## Roadmap

**Next:**
- lower cost per session (the −50% cost goal; −13.7% in benchmark v13), with code read plus
  injected reported beside it. Offline checks of the next ideas found about 1% each, so the goal
  itself is under review;
- less time per session (the −30% time goal; −19.2% in benchmark v13): time follows how much
  Claude writes;
- turning the retrained model's better offline ranking into an end-to-end gain (benchmarks v10
  and v12 show none yet);
- compacting Moon's data log automatically (it reached 4.1 GB during the benchmark);
- a faster model for Linux.

See [ROADMAP.md](ROADMAP.md) for the full plan to 1.0.

<details>
<summary>Released so far</summary>

- **v0.4.0, current:**
  - `search` answers name lookups (definitions, callers, uses, tests), so Claude Greps 61% less;
  - the reranker is retrained on the candidates laya-codex ranks, and the installer pins it;
  - follow-up prompts get lists of tests and callers instead of more code;
  - the benchmark counts code injected as well as code read.
- **v0.3.0:**
  - ranking uses the task, not the instructions around it;
  - the model ranks within its time budget even under load;
  - trust labels on inlined code, and at most two inlined files;
  - `laya-codex trace` for debugging;
  - a nearly full disk no longer disables laya-codex silently.
- **v0.2.0:** one name everywhere: the CLI is `laya-codex` (was `laya`), env vars are
  `LAYA_CODEX_*`; a Homebrew formula; the plugin, crash isolation and daemon limits from 0.1.x.

Details: the [CHANGELOG](CHANGELOG.md).

</details>

## Reference

<details>
<summary><b>Commands</b></summary>

```sh
laya-codex init --repo /path/to/repo      # add hooks and the MCP server to one repository, then index it
laya-codex doctor --repo /path/to/repo    # check everything; prints a fix for each problem (--json available)
laya-codex index /path/to/repo            # incremental; re-run any time
laya-codex query "where is WAL replay implemented" --repo /path/to/repo
laya-codex status | laya-codex stop
laya-codex trace on|off|status|show|clear # record Claude Code <-> laya-codex exchanges for debugging
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
| `LAYA_CODEX_BUDGET_MS` | `1200` | Laya time budget per prompt: the model scores as many candidates as fit (lexical only if none) |
| `LAYA_CODEX_RENDER` | `compact` | `full` injects every span's code |
| `LAYA_CODEX_WEIGHT` / `LAYA_CODEX_STATE_TOKENS` / `LAYA_CODEX_K` / `LAYA_CODEX_P_THRESHOLD` | `0.5` / `128` / `24` / `0` | ranking knobs (daemon start) |
| `LAYA_CODEX_ADAPTIVE` | on | `0` = fixed compact injection; default skips code already sent or read in the session, and answers follow-ups about tests or callers with lists instead of code |
| `LAYA_CODEX_SCORE_TOP` | `16` | candidates the model scores (best first); `0` = all of them |
| `LAYA_CODEX_MOON_START_SECS` | `30` | how long a freshly started Moon may take to answer |
| `LAYA_CODEX_MOON_PORT` / `LAYA_CODEX_MOON_BIN` | `16379` / `moon` beside the real `laya-codex` binary, else in `../libexec` (Homebrew), else on `PATH` | Moon sidecar; a missing binary is reported with every path tried |
| `LAYA_CODEX_BIN` | unset | the `laya-codex` binary the Claude Code plugin should use |
| `LAYA_CODEX_TRACE` | unset (`laya-codex trace on` decides) | `1` = record hook and MCP exchanges in `$LAYA_CODEX_HOME/trace/trace.jsonl`, a path = record there, `0` = never |

`LAYA_CODEX_SCOPE`, `LAYA_CODEX_SCOPE_P`, `LAYA_CODEX_TAU_FULL`, `LAYA_CODEX_TAU_MAP` and `LAYA_CODEX_SIZE_BY_REPO` are no longer read; leaving them set changes nothing.

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
- **Model weights (optional):** `hf download tindang/laya-code --revision 25f97e5a2ec5f8cf7218a4f67504367d8832e1fe --local-dir ~/.cache/laya-codex/models/laya-code` (the revision `install.sh` pins; the repo's `main` branch holds the older model). Without them, laya-codex ranks by keywords alone.

```sh
cargo build --release -p laya-cli        # target/release/laya-codex (fat LTO, mimalloc, Metal on macOS)
cargo test --workspace --release
```

</details>

<details>
<summary><b>Reproducing the benchmark</b></summary>

Pinned commits and task sets are listed in [docs/RESULTS.md](docs/RESULTS.md) (v8). Before you
run:
- **Clone outside your home directory.** Claude Code reads `.claude/CLAUDE.md` in every folder
  above the repo, including your home, so `run_bench` refuses a clone with a CLAUDE.md above it.
- **Pin the model by id** (`--model claude-sonnet-5-5`, not the `sonnet` alias, which moves), and
  set the effort.

Each task's two prompts run in one live Claude session. For each repository:

```sh
python3 bench/run_bench.py tasks --repo <clone> --skip 40 --n 20 [--code-only] --out bench/tasks-v8/<repo>.jsonl
laya-codex index <clone>
python3 bench/run_bench.py run --repo <clone> --tasks bench/tasks-v8/<repo>.jsonl \
    --arms baseline,laya:laya-adaptive --model claude-sonnet-5-5 --effort medium --turns 2 \
    --max-total-usd 10 --out /tmp/bench/<repo>
```

Add `laya-lex:laya-lex` to `--arms` for the keyword-only comparison. Then pool the three runs and
build the headline and charts:

```sh
python3 bench/stats_pooled.py laya baseline /tmp/bench/moon /tmp/bench/httpx /tmp/bench/hono
python3 bench/headline.py --out /tmp/bench/headline.json --version mine --arm laya --lex none --with-output \
    --claude-model claude-sonnet-5-5 --effort medium --raw-root /tmp/bench \
    --run /tmp/bench/moon bench/tasks-v8/moon.jsonl --run /tmp/bench/httpx bench/tasks-v8/httpx.jsonl \
    --run /tmp/bench/hono bench/tasks-v8/hono.jsonl
python3 scripts/charts.py /tmp/bench/headline.json /tmp/bench/charts
```

Benchmark v13 cost $14.29 for 60 tasks × 2 arms on Sonnet 5.5.

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
