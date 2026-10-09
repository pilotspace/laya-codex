"""Re-derive benchmark session costs from the raw transcripts.

    python3 bench/recost.py rewrite <run dir> [<run dir> ...]
    python3 bench/recost.py cold <treatment arm> <baseline arm> <run dir> [<run dir> ...]
    python3 bench/recost.py tokens <run dir> [<run dir> ...]

`rewrite`: every result event's total_cost_usd is the session's running total, in one live
session and across `claude -p --resume` alike, so a two-prompt session costs its last total. The
harness summed them and counted prompt 1 twice until 2026-09-30. `rewrite` sets `cost_usd` to the
last total, keeps the old sum as `cost_usd_summed`, and adds `prompt_cost_usd` and
`first_call_cache_read` (the cache read of the session's first API call). Needs `raw/`; idempotent.

`tokens`: until 2026-10-07 the harness counted code at 3.5 characters per token, about 1.6x
fewer tokens than the API bills. `tokens` re-estimates `reading_tokens` from raw/ and
`injected_tokens` and `prompt_injected_tokens` from hooklogs/ at runs.TOKENS_PER_CHAR, keeping
the old values as `<field>_at_3_5`. Idempotent.

`cold`: until the same date the harness gave every arm the same system prompt, so an arm whose
first request matched one another arm had just sent read that arm's prompt cache (v14: 51 of 51
tasks where the two laya-codex arms injected the same context). Only the first API call can match
(the conversations diverge after it). `cold` re-prices the part of that call's cache read above
the arm's usual system-prompt read (the median over sessions that ran first for their task, per
run dir) from a cache read to a cache write, then prints the paired, repo-stratified bootstrap of
bench/stats_pooled.py. Run it after `rewrite`.
"""
import json
import os
import random
import statistics as st
import sys

from run_bench import parse_stream, run_name
from runs import PRICES_PER_M, read_hook_log
import stats_pooled


def _events(path):
    with open(path) as f:
        return [json.loads(x) for x in f if x.strip()]


def session_cost(events):
    totals, first_read = [], None
    for e in events:
        if e.get("type") == "assistant" and first_read is None:
            first_read = (e.get("message", {}).get("usage") or {}).get("cache_read_input_tokens") or 0
        elif e.get("type") == "result" and e.get("total_cost_usd") is not None:
            totals.append(e["total_cost_usd"])
    steps = [round(b - a, 6) for a, b in zip([0.0] + totals, totals)]
    return {"cost_usd": totals[-1] if totals else 0.0, "prompt_cost_usd": steps, "first_call_cache_read": first_read or 0}


def _raw(run_dir, row):
    return os.path.join(run_dir, "raw", run_name(row["task_id"], row["arm"], row.get("rep") or 0) + ".jsonl")


def rewrite(run_dir):
    path = os.path.join(run_dir, "runs.jsonl")
    rows = _events(path)
    for row in rows:
        row.setdefault("cost_usd_summed", row["cost_usd"])
        row.update(session_cost(_events(_raw(run_dir, row))))
    with open(path, "w") as f:
        f.write("".join(json.dumps(r) + "\n" for r in rows))
    return rows


def retoken(run_dir):
    path = os.path.join(run_dir, "runs.jsonl")
    rows = _events(path)
    for row in rows:
        for k in ("reading_tokens", "injected_tokens", "prompt_injected_tokens"):
            row.setdefault(k + "_at_3_5", row.get(k))
        with open(_raw(run_dir, row)) as f:
            row["reading_tokens"] = parse_stream(f.readlines())["reading_tokens"]
        h = read_hook_log(os.path.join(run_dir, "hooklogs", run_name(row["task_id"], row["arm"], row.get("rep") or 0)
                                       + ".jsonl"))
        row["injected_tokens"], row["prompt_injected_tokens"] = h["injected_tokens"], h["prompt_injected_tokens"]
    with open(path, "w") as f:
        f.write("".join(json.dumps(r) + "\n" for r in rows))
    return rows


def cold_costs(rows):
    """{(task_id, arm): cost} with cross-arm cache reads re-priced; `rows` in run order, one repeat."""
    first_arm = {}
    for r in rows:
        first_arm.setdefault(r["task_id"], r["arm"])
    usual = {}
    for arm in {r["arm"] for r in rows}:
        mine = [r["first_call_cache_read"] for r in rows if r["arm"] == arm]
        ran_first = [r["first_call_cache_read"] for r in rows if r["arm"] == arm and first_arm[r["task_id"]] == arm]
        usual[arm] = st.median(ran_first) if ran_first else min(mine)
    extra = (PRICES_PER_M["cache_creation_input_tokens"] - PRICES_PER_M["cache_read_input_tokens"]) / 1e6
    return {(r["task_id"], r["arm"]): r["cost_usd"] + max(0, r["first_call_cache_read"] - usual[r["arm"]]) * extra
            for r in rows}


def cold(arm, base, dirs, b=stats_pooled.B):
    repos = {}
    for d in dirs:
        c = cold_costs(_events(os.path.join(d, "runs.jsonl")))
        tasks = sorted({t for t, a in c if a == arm} & {t for t, a in c if a == base})
        repos[os.path.basename(os.path.normpath(d))] = [({"c": c[(t, arm)]}, {"c": c[(t, base)]}) for t in tasks]
    f = lambda r: r["c"]
    rng = random.Random(0)
    idx = [{n: [rng.randrange(len(p)) for _ in p] for n, p in repos.items()} for _ in range(b)]
    cells = []
    for n, p in repos.items():
        lo, hi = stats_pooled.ci([stats_pooled.ratio([p[i] for i in ix[n]], f) for ix in idx])
        cells.append("%s %+.1f%% [%+.1f, %+.1f]" % (n, 100 * stats_pooled.ratio(p, f), 100 * lo, 100 * hi))
    allp = [x for p in repos.values() for x in p]
    lo, hi = stats_pooled.ci([stats_pooled.ratio([repos[n][i] for n in repos for i in ix[n]], f) for ix in idx])
    mean = lambda k: sum(f(x[k]) for x in allp) / len(allp)
    return ("%s vs %s, cost with cross-arm cache reads re-priced as writes (paired tasks: %d)\n"
            "  pooled %+.1f%% [%+.1f, %+.1f]; mean $%.4f vs $%.4f\n  %s\n" % (
                arm, base, len(allp), 100 * stats_pooled.ratio(allp, f), 100 * lo, 100 * hi, mean(0), mean(1),
                "; ".join(cells)))


def main(argv):
    if argv[:1] == ["rewrite"] and argv[1:]:
        for d in argv[1:]:
            print("%s: %d rows re-costed" % (d, len(rewrite(d))))
    elif argv[:1] == ["tokens"] and argv[1:]:
        for d in argv[1:]:
            print("%s: %d rows re-tokenized" % (d, len(retoken(d))))
    elif argv[:1] == ["cold"] and len(argv) > 3:
        sys.stdout.write(cold(argv[1], argv[2], argv[3:]))
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv[1:])
