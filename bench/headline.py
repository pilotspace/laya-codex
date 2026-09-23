"""Machine-readable headline for README charts (bench/results/headline-v8.json).

    python3 bench/headline.py --out bench/results/headline-v8.json \
        --run <run dir> <tasks.jsonl> [--run <run dir> <tasks.jsonl> ...] [--raw-root <dir with <repo>/raw>]

pct = laya-adaptive vs baseline (ratio of sums - 1, in %), 95% CI from the paired bootstrap of
bench/stats_pooled.py (pooled = stratified by repo). laya_vs_lex = laya-adaptive vs laya-lex.
Read accuracy needs the raw transcripts (not committed); pass --raw-root when they are available.
"""
import argparse
import json
import os
import random
from collections import defaultdict

import read_accuracy
import stats_pooled as sp

METRICS = {
    "reading_tokens": sp.METRICS["code-reading tokens"],
    "reading_plus_injected": sp.METRICS["reading+injected tokens"],
    "total_input": sp.METRICS["total input tokens"],
    "wall_clock": sp.METRICS["wall seconds"],
    "turns": sp.METRICS["turns"],
    "cost": sp.METRICS["cost usd"],
}


def compare(runs, arm, base, keys, B=10000):
    repos = {name: sp.load(d, arm, base)[0] for name, d in runs}
    rng = random.Random(0)
    idx = [{n: [rng.randrange(len(p)) for _ in p] for n, p in repos.items()} for _ in range(B)]

    def cell(pairs_of, f):
        lo, hi = sp.ci([sp.ratio(pairs_of(ix), f) for ix in idx])
        return {"pct": round(100 * sp.ratio(pairs_of(None), f), 1), "lo": round(100 * lo, 1), "hi": round(100 * hi, 1)}

    pooled = {}
    per_repo = {n: {} for n in repos}
    for k in keys:
        f = METRICS[k]
        pooled[k] = cell(lambda ix: [x for n in repos for x in (repos[n] if ix is None else [repos[n][i] for i in ix[n]])], f)
        for n in repos:
            per_repo[n][k] = cell(lambda ix, n=n: repos[n] if ix is None else [repos[n][i] for i in ix[n]], f)
    q = {}
    for label, f in (("answer_recall", sp.QUALITY["answer recall (turn 1)"]),
                     ("answer_recall_both_turns", sp.QUALITY["answer recall (both turns)"])):
        allp = [x for p in repos.values() for x in p]
        lo, hi = sp.ci([sp.mean_diff([repos[n][i] for n in repos for i in ix[n]], f) for ix in idx])
        q[label] = {base: round(sum(f(b) for _, b in allp) / len(allp), 3), arm: round(sum(f(t) for t, _ in allp) / len(allp), 3),
                    "diff": round(sp.mean_diff(allp, f), 3), "lo": round(lo, 3), "hi": round(hi, 3)}
    return pooled, per_repo, q, sum(len(p) for p in repos.values())


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--run", nargs=2, action="append", metavar=("RUN_DIR", "TASKS"), required=True)
    ap.add_argument("--raw-root", help="dir holding <repo>/raw transcripts (for read accuracy)")
    a = ap.parse_args()
    runs = [(os.path.basename(os.path.normpath(d)), d) for d, _ in a.run]
    keys = list(METRICS)
    pooled, per_repo, q, n = compare(runs, "laya-adaptive", "baseline", keys)
    lex, lex_repo, lex_q, _ = compare(runs, "laya-adaptive", "laya-lex", keys)
    lexb, _, lexb_q, _ = compare(runs, "laya-lex", "baseline", keys)
    out = {"version": "v8", "n_tasks": n, "repos": [r for r, _ in runs], "model": "sonnet", "arm": "laya-adaptive",
           "metrics": pooled, "per_repo": per_repo,
           "answer_recall": {"baseline": q["answer_recall"]["baseline"], "laya": q["answer_recall"]["laya-adaptive"],
                             "diff": q["answer_recall"]["diff"], "lo": q["answer_recall"]["lo"], "hi": q["answer_recall"]["hi"]},
           "answer_recall_both_turns": {"baseline": q["answer_recall_both_turns"]["baseline"],
                                        "laya": q["answer_recall_both_turns"]["laya-adaptive"],
                                        "diff": q["answer_recall_both_turns"]["diff"],
                                        "lo": q["answer_recall_both_turns"]["lo"], "hi": q["answer_recall_both_turns"]["hi"]},
           "laya_vs_lex": {"note": "laya-adaptive vs laya-lex (same hooks, LAYA_BUDGET_MS=0: lexical-only ranking)",
                           "pooled": {k: lex[k] for k in ("reading_tokens", "total_input", "wall_clock", "cost")},
                           "per_repo": {r: {k: v[k] for k in ("reading_tokens", "wall_clock")} for r, v in lex_repo.items()},
                           "answer_recall": lex_q["answer_recall"]},
           "lex_vs_baseline": {"pooled": {k: lexb[k] for k in ("reading_tokens", "total_input", "wall_clock", "cost")},
                               "answer_recall": lexb_q["answer_recall"]}}
    if a.raw_root:
        pooled_rows = defaultdict(list)
        for (name, _), (_, tasks) in zip(runs, a.run):
            read_accuracy.collect(os.path.join(a.raw_root, name), tasks, pooled_rows)
        def mean(rs, k):
            v = [r[k] for r in rs if r[k] is not None]
            return round(sum(v) / max(1, len(v)), 3)
        out["read_precision"] = {"baseline": mean(pooled_rows["baseline"], "read_precision"),
                                 "laya": mean(pooled_rows["laya-adaptive"], "read_precision"),
                                 "lex": mean(pooled_rows["laya-lex"], "read_precision")}
        out["first_gold_read_turn"] = {"baseline": round(mean(pooled_rows["baseline"], "first_gold_read_turn"), 2),
                                       "laya": round(mean(pooled_rows["laya-adaptive"], "first_gold_read_turn"), 2),
                                       "lex": round(mean(pooled_rows["laya-lex"], "first_gold_read_turn"), 2)}
    json.dump(out, open(a.out, "w"), indent=1)
    print(json.dumps(out, indent=1))


if __name__ == "__main__":
    main()
