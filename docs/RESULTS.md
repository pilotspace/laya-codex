# laya-codex — results (2026-09-23)

All numbers are reproducible from this repo; raw per-run rows are in `bench/results/`.
Hardware: Apple M4 Pro, 24 GB. Agent: Claude Code 2.1.280, model `sonnet`, isolated from user
settings/plugins/MCP (`--setting-sources project --strict-mcp-config`), tools Read/Grep/Glob.

## v8 — benchmark v2 (3 repos, 60 tasks)

**Bottom line.** The v7 result did not hold up over three repositories:
- With laya-codex, Claude Code reads **38% less code** (CI excludes zero), takes **21% fewer
  turns** and costs **10% less**.
- Wall-clock did not move: **+3.5%** (n.s.).
- Total input tokens did not move: −3.7% (n.s.).
- Answer quality did not drop. Turn-1 recall went up (+0.083, significant); recall over both
  prompts is +0.026 (n.s.).
- Neither numeric goal is met, pooled or on any single repo.
- The Laya model **did not beat lexical-only ranking**: `laya-adaptive` is 13% *slower* than
  `laya-lex` (significant).
  - *Caveat (added later):* the model probably fell back to keyword ranking on part of the
    `laya-adaptive` prompts, after using up its time budget. The run didn't record how many (see
    [the caveat below](#caveat-the-model-may-not-have-ranked-every-prompt)).
- On moon, the v7 task set gave wall-clock −17.4% in v7 and +13.8% now, with the same tasks, the
  same repo commit and the same injection mechanics. So a single 20-task run cannot pin the
  wall-clock effect.

### Design

| repo | language | pinned commit | indexed files / chunks | tasks |
|---|---|---|---|---|
| [pilotspace/moon](https://github.com/pilotspace/moon) | Rust | `8bba3ced49ab` (2026-05-26) | 783 / 8,842 | the v7 set (`bench/tasks-v8/moon.jsonl` = `bench/tasks.jsonl`) |
| [encode/httpx](https://github.com/encode/httpx) | Python | `b5addb64f016` (2026-02-23) | 92 / 982 | `bench/tasks-v8/httpx.jsonl` |
| [honojs/hono](https://github.com/honojs/hono) | TypeScript | `6cadf7537385` (2026-09-22) | 436 / 2,569 | `bench/tasks-v8/hono.jsonl` |

- **Tasks.** 20 per repo from git history with the v7 generator: `run_bench.py tasks --skip 40 --n 20`.
  - Moon keeps the v7 set, which the generator reproduces exactly at `8bba3ced`. That is the
    checkout v7 ran on.
  - httpx and hono add `--code-only`. It drops reverts, `ruff`/`mypy` fixes and commits that only
    touch test files; without it httpx produced "move test cases into test_url.py" tasks.
  - Applied to moon, `--code-only` changes nothing.
  - Gold = the source files the commit touched. For httpx and hono that includes test files, so
    turn-1 recall (which asks for the implementation) is stricter there. Recall over both prompts
    (the second prompt asks for tests) is the fairer quality measure.
- **Arms.** All arms use the same laya-codex 0.1.2 binary (then named `laya`) (`a38c8be`), the same Claude Code 2.1.280,
  `--model sonnet`, 2 prompts per session and tools Read/Grep/Glob. Order is interleaved at random
  per task.
  - `baseline`: stock Claude Code.
  - `laya-adaptive`: the v0.1.2 defaults.
  - `laya-lex`: the same hooks and MCP server with `LAYA_BUDGET_MS=0`. This is lexical-only
    ranking with identical rank-based sizing and rendering, so it isolates the Laya model.
- **Scoring.** Every prompt is scored cold (`LAYA_MEMO=0`), and the daemon restarts at each run
  start.
- **Stats.** `bench/stats_pooled.py` gives the ratio-of-sums change and a paired bootstrap 95% CI
  (10k resamples). The pooled figures resample tasks within each repo (stratified). A
  "repo-balanced" figure, the mean of the per-repo changes, is in the raw stats files.
- **Leakage.** None of the three repos is in the laya-code `TRAIN` list (`finetune/repos.py`), and
  moon is `HELDOUT`.
  - The training checkouts are not on the benchmark machine, so the root-commit overlap check was
    not re-run.
  - httpx and hono are popular public repos. Laya's base model and Sonnet may both have seen them
    in pre-training. For Sonnet that applies to every arm equally.
- **Raw data.** Rows are in `bench/results/claude-v8/<repo>/runs.jsonl`. Pins, binaries and spend
  are in `bench/results/claude-v8/meta.json`, and the machine-readable headline is
  `bench/results/headline-v8.json`.

### Headline: laya-adaptive (v0.1.2 defaults) vs stock Claude Code

Each cell is the change, the 95% CI, and the number of tasks where laya-codex was lower.

| metric | moon (Rust) | httpx (Python) | hono (TS) | **pooled, 60 tasks** |
|---|---|---|---|---|
| code-reading tokens | −37.1% [−55.1, −9.7] 12/20 | −31.5% [−45.2, −13.5] 15/20 | −46.5% [−60.3, −30.7] 18/20 | **−38.2% [−50.8, −21.8]** 45/60 |
| reading + injected tokens | −12.6% [−36.4, +23.3] | +69.5% [+35.3, +121.3] | +22.2% [+0.3, +48.9] | +7.1% [−13.7, +33.0] |
| total input tokens | −3.5% [−22.4, +21.0] | −3.0% [−19.3, +13.8] | −5.2% [−30.3, +33.1] | −3.7% [−16.9, +11.9] |
| wall-clock | +13.8% [−3.3, +31.9] 8/20 | −2.5% [−13.0, +11.3] 13/20 | −5.0% [−23.2, +21.3] 12/20 | **+3.5% [−6.2, +14.8]** 33/60 |
| turns | −10.7% [−23.2, +2.5] | −26.5% [−36.6, −15.1] | −31.8% [−41.9, −19.1] | **−20.9% [−27.7, −13.7]** 45/60 |
| cost | −16.8% [−28.4, −3.0] | +3.8% [−5.0, +13.2] | −5.2% [−21.7, +17.3] | **−9.9% [−18.7, −0.4]** 32/60 |
| answer recall, turn 1 | 0.950 vs 0.925 | 0.917 vs 0.717 | 0.829 vs 0.804 | 0.899 vs 0.815, **+0.083 [+0.028, +0.144]** |
| answer recall, both prompts | 0.975 vs 0.950 | 0.917 vs 0.867 | 0.988 vs 0.983 | 0.960 vs 0.933, +0.026 [−0.014, +0.075] |

Means per session (baseline / laya-adaptive / laya-lex):

| repo | wall-clock (s) | turns | cost ($) |
|---|---|---|---|
| moon | 60.8 / 69.1 / 52.4 | 18.8 / 16.8 / 13.0 | 0.463 / 0.385 / 0.339 |
| httpx | 42.4 / 41.4 / 35.7 | 12.7 / 9.3 / 8.4 | 0.171 / 0.177 / 0.162 |
| hono | 43.9 / 41.7 / 46.1 | 11.2 / 7.6 / 8.6 | 0.186 / 0.176 / 0.212 |

### Against the goals

| goal | moon | httpx | hono | pooled | verdict |
|---|---|---|---|---|---|
| −50% code-reading tokens | −37% | −32% | −47% | −38% [−51, −22] | **not met.** A significant cut, but the CI only just reaches −50%. Counting the injected context, reading is +7% (n.s.). |
| −30% task time | +14% (n.s.) | −3% (n.s.) | −5% (n.s.) | +3.5% [−6, +15] | **not met.** There is no measurable time effect. |
| no answer-quality loss | +0.03 | +0.20 | +0.03 | +0.083 turn 1 (sig.), +0.026 both prompts (n.s.) | **met.** Quality did not drop on any repo, and turn-1 recall rose. |

### Is it the Laya model? laya-adaptive vs laya-lex (same pipeline, lexical-only ranking)

| metric | moon | httpx | hono | pooled |
|---|---|---|---|---|
| code-reading tokens | +19.1% [+4.3, +35.2] | −1.7% [−20.2, +19.3] | −13.7% [−32.7, +9.8] | +7.9% [−3.1, +19.8] |
| total input tokens | +29.5% [+9.5, +55.5] | +15.6% [−2.3, +38.2] | −17.4% [−36.1, +2.2] | **+14.0% [+1.1, +29.1]** |
| wall-clock | +31.8% [+8.4, +60.5] | +16.0% [−0.2, +36.0] | −9.5% [−23.4, +5.6] | **+13.4% [+1.8, +27.0]** |
| turns | +28.8% [+12.5, +48.5] | +10.1% [−5.9, +29.7] | −11.6% [−23.0, +0.7] | **+12.0% [+2.9, +22.2]** |
| cost | +13.5% [−2.1, +33.1] | +9.3% [−5.2, +28.0] | −16.9% [−29.9, −2.2] | +3.5% [−5.9, +14.7] |
| recall, turn 1 | −0.008 | +0.100 | −0.025 | +0.022 [−0.025, +0.075] |

Compared with stock Claude Code, `laya-lex` did better than `laya-adaptive` on time, turns and
input:
- wall-clock −8.8% [−18.9, +2.5];
- turns −29.4% [−35.6, −22.8];
- total input −15.6% [−31.2, −0.1];
- cost −12.9% [−24.4, −1.5];
- code reading −42.7% [−55.5, −27.6];
- recall +0.061 on turn 1 and +0.004 over both prompts.

The model's scoring alone costs about 1.1 s per prompt, roughly 4% of a session. It does not
explain the whole +13%: the adaptive sessions also took more turns.

#### Caveat: the model may not have ranked every prompt

*Added 2026-09-24, after the run.*

**Why it could fall back.** The model's time budget was 1,200 ms (`LAYA_BUDGET_MS`). Scoring 24
candidates takes about 5,500 tokens/s on this machine. The benchmark wording made the model's
question long, so a scoring run took about as long as the whole budget.
- **All or nothing:** a run that missed the budget gave no model ranking. The prompt was ranked
  by keywords alone, after waiting out the budget.
- **Not recorded:** `runs.jsonl` records hook actions but not which ranking each prompt got. So
  the share of `laya-adaptive` prompts that the model actually ranked is **unknown**.

**How large the share could be.** I replayed the same 60 first prompts afterwards, with the same
wording, the same 1,200 ms budget and the same scoring code (v0.2.0 differs from v0.1.2 here only
in renamed variables).
- **Quiet machine:** the model ranked 53 of 60 prompts.
- **Busy machine:** it ranked 13 of 60.
- **Timing run with the budget lifted:** 51 of 60 scorings took longer than 1.2 s.

**What this means for the comparison:**
- `laya-adaptive` was partly a keyword-ranked arm that paid for the model's time. That may
  account for part of its +13% wall-clock against `laya-lex`.
- "The model did not beat lexical-only ranking" should read as "this run could not show it
  does", not "the model makes no difference".

**Fixes in progress** (open PRs at the time of writing):
- #10: the model's question no longer carries the prompt's instructions;
- #11: the model scores candidates best-first inside the budget instead of all or nothing. Under
  load it ranked 59 of 60 of these prompts instead of 0–2.

The next model-vs-keywords run should record the ranking mode of every prompt. The trace log
(`laya-codex trace`, #8) does this.

The forensics (`bench/results/claude-v8/forensics-*.md`) show that the re-ranker injects no more
gold files than BM25 does. The gold files it inlines:

| repo | laya-adaptive | laya-lex |
|---|---|---|
| moon | 19 | 20 |
| httpx | 28 | 28 |
| hono | 26 | 28 |

The Laya effect only favoured the model on hono, and flipped sign between repos. This matches the
v1/v2 ablations, where the Laya-vs-lexical sign also flipped. Over 60 tasks, the evidence now
points *against* paying for the model by default.

**Robustness of the +13% (added after review).** Per task, from the raw rows:
- the median per-task ratio is +13.6% and the geometric mean +15.3%, and `laya-adaptive` was
  faster on 21 of 60 tasks, so the gap is not produced by one outlier;
- dropping the 3 worst tasks (moon `14ca4f0c12`, moon `624822d46e`, httpx `a682f6f1c7`) leaves
  +6.9% [−3.6, +17.9], which is not significant; turns go from +12.0% to +6.9% [−2.3, +16.4];
- the model's own scoring (~1.1 s per prompt) is about 4 points of it; the rest is Claude taking
  more turns after a different injection.

**Decision (2026-09-23): the model stays on by default.** Choosing which code blocks replace
Claude's own Search and Read is what laya-codex is for, and offline the model ranks those blocks
far better than keywords (MRR 0.702 vs 0.480, calibrated). This run says that advantage does not
yet reach the end-to-end session. Closing that gap, with a larger model-vs-keywords run to confirm
it, is the top roadmap item. `LAYA_CODEX_NO_MODEL=1` gives lexical-only ranking.

Environment variables in this section use the v0.1.x names (`LAYA_BUDGET_MS`, `LAYA_MEMO`,
`LAYA_MOON_BIN`); since v0.2.0 they are `LAYA_CODEX_*`.

### Read accuracy (`bench/read_accuracy.py`, pooled over 60 tasks)

| arm | Read calls | read precision | gold code seen | wasted read tokens | first gold Read at turn |
|---|---|---|---|---|---|
| baseline | 4.02 | 0.595 | 0.817 | 2,005 | 5.91 |
| laya-adaptive | 3.48 | 0.624 | 0.906 | 1,187 | 3.35 |
| laya-lex | 3.48 | 0.604 | 0.907 | 1,127 | 3.35 |

Per repo (`bench/results/claude-v8/read-accuracy.md`):
- On moon, read precision rose from 0.37 to 0.56 and the first gold Read moved from turn 8.2 to 4.0.
  Both repeat v7.
- On httpx and hono, stock Claude is already precise (0.58 and 0.83) and finds gold by turn about 4.6.
  Laya's gains there are fewer wasted reads and more gold seen, not better precision.

### Why reading fell but time did not (forensics)

1. **The injection is fixed-size, and small repos do not need it.** Every prompt injects about
   3.4k tokens (p50 about 5.6k characters, none over the 9,500 cap, `mechanics.txt`).
   - Stock Claude reads only 3.3k tokens per session on httpx and 4.5k on hono.
   - So on those repos, reading plus injected tokens *rises* (+70%, +22%), and total input and
     cost stay flat.
   - The 38% code-reading cut is real, but on small repos laya-codex mostly swaps Reads for injected
     tokens.
2. **Time goes to the tail, not to finding code.** In every arm, 87–90% of moon wall-clock comes
   after the last new gold file is found. That time goes to verification greps, the second
   prompt, and writing the answers.
   - Laya moves the first gold Read earlier (turn 5.9 → 3.4), but that part is a small share of
     the session.
   - On moon, `laya-adaptive` sessions made *more* verification greps than baseline (6.2 vs 5.4
     `e_grep_locate` calls per task). `laya-lex` made fewer (3.4).
3. **The largest misses.**
   - moon `14ca4f0c12`: +60 s vs baseline, 28 vs 29 turns (lex: 14).
   - moon `624822d46e`: +56 s, 21 vs 17 turns.
   - hono `d982f637eb`: +84 s, where *both* laya-codex arms went long (17 and 19 turns vs 10).
   - These are long verification tails after the gold file was already in context, not retrieval
     misses.

### v7 did not replicate on moon

The v7 run and this run used the same 20 moon tasks, the same repo commit, the same index (783
files, 8,842 chunks) and matching mechanics:
- 80 injections, max 8,303 characters;
- 69 of 80 with "Definitions and uses", vs 70 of 80 in v7.

The ranking code is unchanged between v0.1.0 and v0.1.2. The results moved:

| metric | v7 | v8 |
|---|---|---|
| code reading | −50.1% | −37.1% |
| wall-clock | −17.4% [−31.5, −1.0] | +13.8% [−3.3, +31.9] |
| baseline session wall-clock | 72.1 s | 60.8 s |

The v7 wall-clock CI and this one overlap only near zero. Day-to-day variation in Sonnet's
behaviour and API latency is at least as large as the effect v7 reported. Wall-clock claims need
more than one 20-task run.

### Run log, cost and caveats

- **Pilot.**
  - 18 sessions (2 tasks × 3 arms × 3 repos) cost $5.19 and took 16.6 min.
  - That projected $52 and 2.8 h for the full run, well inside the $150 / 16 h gate.
  - The pilot rows are part of the final 180 (same protocol).
- **Full run.**
  - 180 valid sessions cost $45.45 and 144.5 min of session wall-clock. There were 0 non-zero
    exits and 0 timeouts.
  - Total spend was about $51 including the discarded sessions below.
- **Incident: disk full.** The machine's data volume was about 96% full. Moon's default guard
  (`--disk-free-min-pct 5`) paused writes, and every laya-codex query failed (`query_failed`) from moon
  session 21 on.
  - 15 moon laya-codex-arm sessions had run with no injection. They are archived in
    `moon/runs.dropped.jsonl` ($5.41) and were re-run once, all successfully.
  - For the re-run, Moon was restarted through an `LAYA_MOON_BIN` wrapper that adds
    `--disk-free-min-pct 1`, and its AOF was compacted from 4.1 GB to 63 MB. No later session
    lost its injection.
  - Side effect: on 8 moon tasks the laya-codex arms ran about an hour after their baseline, so those
    pairs were not interleaved in time.
  - Sensitivity: on those 8 tasks both laya-codex arms look worse than on the other 12. Adaptive/baseline
    wall ratio is 1.28 vs 1.06, and lex/baseline is 1.00 vs 0.80.
  - Excluding them does not change any verdict. The adaptive-vs-lex comparison is unaffected,
    since both arms were re-run together.
- **Scope of the benchmark.**
  - Localisation and explanation tasks only; no edit tasks.
  - One model (Sonnet).
  - n = 20 per repo: per-repo CIs are wide, and only the pooled rows have useful power.
- **Environment.** The nested `claude -p` sessions inherited the orchestrating session's
  environment, including `CLAUDE_EFFORT=medium`. v7 ran the same way, and it is the same for
  every arm.
- **Task filter.** `--code-only` is a new filter applied to the new repos. Its rules are listed in
  the Design section, and it does not change moon.

---

*Everything below is the single-repo (moon) history up to v7, kept as recorded.*

## 1. The question

Does giving Claude Code pre-ranked code spans (tree-sitter chunks → Moon BM25 → Laya re-rank,
injected by hooks) cut codebase-reading tokens by 50% and task time by 30% without hurting
answer quality?

## 2. Benchmark design

- **Repo**: a clean clone of `pilotspace/moon` (Rust, 783 source files, 8,842 chunks).
- **Tasks**: 20 held-out commits (`bench/tasks.jsonl`), each "find and explain the code for
  <commit subject>, end with `FILES: …`". Gold = files the commit touched. Not used for any
  tuning: tuning used a disjoint 60-commit dev set (`bench/dev_tasks.jsonl`); moon was
  excluded from Laya fine-tuning data.
- **Arms** (same tasks, interleaved, random order): `baseline` (stock Claude Code) vs laya-codex
  variants. Every run records tokens, wall-clock, turns, cost, and graded FILES recall/precision.
- **Stats**: paired bootstrap (10k resamples) 95% CI of the ratio of sums (`bench/stats.py`).

## 3a. Headline result: v7, the v0.1.0 defaults (two prompts per session)

These are the v0.1.0 defaults: adaptive injection (`laya-adaptive`). Tasks are the same 20; each
session has two prompts (localisation, then tests and call sites). Laya is scored **cold** in
every arm (`LAYA_MEMO=0`) and the model is warmed up at daemon start. Raw rows are in
`bench/results/claude-v7/`.

| metric (vs stock Claude Code) | v7 adaptive (default) | v7 laya-refs | 95% CI (adaptive) | tasks improved |
|---|---|---|---|---|
| code-reading tokens | **−50.1%** | −48.5% | [−61.6%, −33.0%] | 17/20 |
| reading + injected tokens | −27.9% | −25.8% | [−43.8%, −4.2%] | 13/20 |
| total input tokens | −26.8% | −28.5% | [−42.2%, −6.4%] | 14/20 |
| wall-clock | **−17.4%** | −16.6% (n.s.) | [−31.5%, −1.0%] | 13/20 |
| turns | −23.4% | −24.9% | [−34.1%, −10.5%] | 15/20 |
| cost | −27.2% | **−34.7%** | [−39.2%, −11.1%] | 13/20 |
| answer recall (turn 1) | 0.933 vs 0.975 | 0.933 | diff −0.042 [−0.108, 0.000] | |
| answer recall (both turns) | 0.933 vs 0.975 | 0.958 | diff −0.017 [−0.092, +0.058] | |

Read accuracy (`bench/read_accuracy.py`):

| arm | read precision | gold code seen | wasted read tokens | first gold Read at turn |
|---|---|---|---|---|
| baseline | 0.373 | 0.871 | 4,993 | 8.2 |
| **adaptive** | **0.546** | **0.933** | **2,118** | 4.0 |
| refs | 0.482 | 0.871 | 2,569 | 3.2 |

Mechanics (`bench/check_v7.py`):
- 80 prompt injections, max 8,303 characters, none over the 9,500 cap.
- 70 of 80 carried the "Definitions and uses" list.
- 6 whole-file Reads were narrowed to a region. Claude later read other ranges of those files,
  and never the whole file.

What moved from v6 to v7:
- Wall-clock went from −9% to −17%, code reading from −39% to −50%, and cost from −19% to
  −27%, even though v7 scores every prompt cold, which v6 did not.
- The forensics-driven changes were:
  - the character cap;
  - distinct-file inlining;
  - the definitions and uses list;
  - the Read plan;
  - the model warm-up.

Recall dipped by 2 tasks (−0.042, CI touching 0). In one of them (756db483ef) both laya arms
missed the third gold file, `warm_search.rs`. Treat that one as a possible systematic miss, not
proven noise.

## 3. Earlier headline: v3 (single prompt, compact injection + one-hop reference expansion)

`bench/results/claude-v3/stats-laya-refs.txt` — laya-refs vs baseline, paired n=20:

| metric | change | 95% CI | lower in |
|---|---|---|---|
| code-reading tokens (Read/Grep/Glob output) | **−44.7%** | [−59.4%, −26.0%] | 17/20 |
| reading + injected tokens | **−29.6%** | [−47.3%, −7.0%] | 14/20 |
| total input tokens (all turns) | **−42.0%** | [−55.0%, −24.4%] | 18/20 |
| wall-clock | **−24.6%** | [−41.8%, −4.0%] | 13/20 |
| turns | **−36.5%** | [−48.5%, −23.7%] | 17/20 |
| cost (USD) | **−28.6%** | [−38.9%, −16.5%] | 17/20 |
| answer recall / precision | **0.950 / 0.420** vs 0.938 / 0.337 | | |

Every change is significant (CI excludes 0), and answer recall is no longer below baseline.

### Read accuracy vs the stock Read tool (`bench/read_accuracy.py`, v3)

| arm | Read calls | read precision | read recall (gold code seen) | wasted read tokens | first gold Read (turn) | runs reading a gold file |
|---|---|---|---|---|---|---|
| baseline | 3.15 | 0.550 | 0.767 | 3,775 | 7.7 | 18/20 |
| laya-compact (no refs) | 3.15 | 0.556 | 0.771 | 1,891 | 4.4 | 17/20 |
| **laya-refs** | **2.70** | **0.657** | **0.871** | **1,740** | **4.5** | **20/20** |

laya-codex gets the agent to relevant code ~3 turns earlier and halves tokens wasted on
irrelevant files; the reference edges are what raise how much of the relevant code it sees.

## 3b. Multi-turn sessions — v6 (two prompts per session)

Each session asks the v3 localisation prompt, then a follow-up in the same session ("Now, for the
same change, identify the tests that cover this code and the main call sites that invoke it …").
The tasks are the same 20 and the runs are paired. Arms: `laya-refs` (v3 injection with the
retrieval fixes below) and `laya-adaptive` (the same, plus session delta: code already sent or
read is not re-sent, and the next-ranked spans take its place).

| metric (vs baseline) | laya-refs | laya-adaptive |
|---|---|---|
| code-reading tokens | **−33.7%** [−50.6, −15.7] | **−39.4%** [−52.6, −25.8] |
| reading + injected | −10.4% [−28.8, +9.4] | −17.4% [−33.3, +0.3] |
| total input tokens | −4.7% [−28.4, +25.3] | −17.1% [−34.5, +4.5] |
| wall-clock | −9.2% [−24.2, +9.8] | −9.6% [−23.5, +9.5] |
| turns | −12.2% [−25.9, +4.7] | **−19.7%** [−31.5, −6.1] |
| cost | **−21.5%** [−36.8, −3.8] | **−18.8%** [−32.4, −4.1] |
| answer recall (base 0.925) | 0.933 | 0.821 |
| read precision / gold seen (base 0.42 / 0.81) | 0.48 / 0.91 | 0.52 / 0.85 |
| first gold Read at turn (base 8.6) | 5.4 | 4.7 |

- **Reading, turns and cost fall significantly; wall-clock does not.** Across two prompts, time
  is dominated by turns, output and the final answer turn (9–10 s each), not by reading.
- **Adaptive's lower answer recall is run noise.** In v6 both laya arms got identical turn-1
  injections, yet they disagree on 5 tasks in both directions. The misses are wiring files
  (`mod.rs`, `main.rs`) that Claude sometimes leaves out of its FILES line.
- **Laya's scoring cache inflated some arms' speed in v3 and v6.** Whichever arm ran a task first
  paid the ~1.1 s model run, and later arms got cache hits. From v7 every arm scores cold
  (`LAYA_MEMO=0`), which matches real use.

### What changed between v3 and v6 (and why v4/v5 were stopped)

- **Retrieval defect (fixed, 1b0ba87, 2fbdd74).**
  - The benchmark's instruction wrapper ("find the source code…", "comma-separated repo-relative
    paths") beat the task words in the rarest-first BM25 term selection. Dev-set MRR fell from
    0.724 (bare task) to 0.394 (wrapped).
  - A prompt stoplist restores 0.541 on wrapped prompts with no loss on bare ones. A broader list
    scored 0.618 but dropped real code words ("fast-path").
  - Follow-ups are detected by explicit back-references ("same", "above", …). They are retrieved
    as the session topic plus any identifiers or paths they name.
  - v4 and v5 ran with the defect in place and were stopped (19/60 and 10/60 runs).
- **Confidence-sized context lost to rank.** Laya's P scale shifts with prompt wording. On
  wrapped prompts every P threshold covered fewer gold files with full code than the fused rank
  at equal code volume, so adaptive sizing is rank-based by default.
- **Scope classifier is effectively a no-op.** Zero-shot Laya can't separate function / file /
  module / cross-cutting scope (best macro-F1 0.28, bar 0.45; a bag-of-words baseline gets
  0.29–0.35). Even oracle scope barely changes what is loaded. It stays wired and gated but
  does nothing; the fine-tune was not run.
- **Fusion weight.** P-weight 0.5 beats 0.7 and Laya-only on the full pipeline (MRR 0.724 vs
  0.678 / 0.602), because the lexical rank is what demotes prose.

### Why Claude still reads after the injection (`docs/research/injection-forensics.md`)

- Claude re-reads only 3.6% of what laya inlined. Its remaining reading goes to:
  - gold files retrieval missed (27%);
  - other ranges of files in the map (21%);
  - within ±100 lines of an inlined chunk (14%).
- Whole-file Reads are 49% of the remaining Read tokens.
- Inlining more spans is net negative: it saves 126–312 tokens and 0 turns, and costs 1–3.7k.
- The largest removable time sink is Grepping for identifiers Claude was already shown: about
  one turn and 3.9 s per task.
- v1's "full injection" arm exceeded Claude Code's 10,000-character hook limit in 16 of 20
  injections, so Claude saw a preview and read the overflow file back.

**v7** (results in §3a) acted on those findings:
- hard 9,500-character cap on all hook output;
- first whole-file Read of a large file → best region plus an outline;
- a "Definitions and uses" list for task identifiers;
- inline the top 3 *distinct files*;
- cold scoring in every arm, with the model warmed up at load (the first Metal run took 10 s).

## 4. Against the goals

| goal | result (v7, v0.1.0 defaults; v3 single-prompt in brackets) | verdict |
|---|---|---|
| −50% codebase-reading tokens | −50.1% code-reading (CI −62%…−33%); −27.9% counting laya's injected context; −26.8% total input [v3: −44.7% / −29.6% / −42%] | **met on code reading (point estimate)**; not when counting the injection |
| −30% task time | −17.4% (CI −32%…−1%) over two prompts [v3: −24.6%, single prompt] | **not met** (significant improvement) |
| no quality drop | answer recall 0.933 vs 0.975, diff −0.042 (CI −0.108…0.000); read precision 0.55 vs 0.37 | **no significant loss**; point estimate −4 points |

## 5. What each component contributed (ablations)

| run | variant | wall | total input | answer recall |
|---|---|---|---|---|
| v1 | full injection (~3.1k tok) + Laya | −6% | −23% | 0.86 vs 0.91 |
| v1 | full injection, lexical only | −17% | −33% | 0.95 vs 0.91 |
| v2 | compact injection (~1.6k tok) + Laya | −21% | −32% | 0.875 vs 0.958 |
| v2 | compact, lexical only | −3.5% | −21% | 0.87 vs 0.96 |
| v3 | compact + Laya (same run as refs) | −14% | −30% | 0.933 vs 0.938 |
| v3 | **compact + Laya + reference expansion** | **−24.6%** | **−42%** | **0.950 vs 0.938** |

- **Injection size** was the first lever (−6% → −21% wall); **reference expansion** the second: in
  the same v3 run it moved wall −14% → −25% and total input −30% → −42% while fixing recall.
- **Laya vs lexical-only flipped between runs** (v1 favoured lexical, v2 Laya): at n=20 the
  model's marginal contribution is within run-to-run noise; its cost is ~0.8 s per prompt.
- **Read narrowing** fired once in 60 treated runs: Sonnet already reads with offset/limit.
- Offline retrieval (dev set, `bench/results/retrieval_dev.jsonl`): lexical MRR 0.644 → 0.700 with
  the prose-demotion prior; Laya re-rank 0.708–0.735; reference expansion raises recall of the
  injected map 0.70 → 0.74.

## 6. Engineering facts

- Index: 783 files → 8,842 chunks in 2.8 s; no-op re-index 75 ms (content-hash skip).
- Query (warm): lexical 13 ms p50; with Laya (24 candidates × 128 tokens, F16 Metal) ~0.77 s.
- Laya Rust port parity with the Python reference: CPU f32 logits |Δ| ≤ 7.3e-6; Metal f16 ≤ 9e-3.
- Hooks: UserPromptSubmit injection 16 ms client-side; every hook fails open.
- **Soak** (`bench/soak.py`, release binary, 15 min, 4 concurrent Claude-like sessions over 2
  repos, faults injected):
  - 151,326 hook calls with 0 failures (no non-zero exits, no invalid output).
  - A file changed under the index at 270 s and was restored at 540 s: handled.
  - The daemon was killed at 450 s: hooks failed open, and it autostarted within 30 s.
  - Latency p50 / p95 / max: prompt 22 / 86 / 1,240 ms, Read 7 / 19 / 230 ms, re-index on edit
    27 / 74 / 264 ms.
  - Daemon RSS stayed flat at 935–987 MB (about 843 MB is the F16 model), so no leak.
  - At about 48 prompts/s (far above human use) the single model scored about 2 prompts/s. The
    rest fell back to lexical ranking immediately instead of queueing, by design (busy flag):
    under overload quality degrades, latency does not.
- 320 tests green, `cargo clippy --workspace --all-targets -D warnings` and `cargo fmt --check`
  clean. The workspace also builds and tests on Linux (arm64 native, x86_64 emulated).

## 7. Honest caveats

- One repo (Rust), one model (Sonnet), one task type (localisation/explanation, no edits), n=20.
- References are name-based (no type resolution): common method names can link to unrelated
  definitions; ambiguous names (>3 definitions) are skipped.
- Cost did not fall in v2 because cached input is cheap and output tokens dominate price variance.

## 8. Next steps (ranked by expected value)

1. **Close the time gap (−17% → −30%).** Time is now dominated by the final answer turn and
   verification turns (forensics §6). The candidates are:
   - fewer verification turns, e.g. richer usage lists for identifiers the answer names;
   - making `laya_search` (MCP) the cheap default for follow-up lookups;
   - checking the `warm_search.rs`-style third-file misses on multi-file tasks.
2. **Residual prompt noise.** A few generic chunks (`hash_write.rs` HgetexMode,
   `shortest_path.rs`) recur across tasks via the answer-format tail ("paths", "files"), and
   per-repo IDF-aware term selection is the principled fix. The laya-typed-decisions comparison
   is done: not better for relevance (AUROC 0.539 vs laya-code 0.713).
3. **More evidence**: 60+ tasks across 3 repos/languages, plus edit tasks (SWE-bench-style);
   n=20 cannot separate Laya from lexical-only or adaptive from v3.
4. **Moon**: upstream OR queries and DEL de-indexing (see `crates/laya-store/MOON_NOTES.md`),
   then the embedded `moon-embed` store (architecture D2 v2).
