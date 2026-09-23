"""Per-task reciprocal-rank changes between two dumped dev-set runs (eval_retrieval --dump).

    python3 bench/diff_dumps.py <before.jsonl> <after.jsonl>
"""
import json
import sys


def rr(row):
    files = [s["path"] for s in row["result"]["spans"]][:10]
    return next((1 / (i + 1) for i, f in enumerate(files) if f in set(row["gold"])), 0.0)


before = {json.loads(l)["task"]: json.loads(l) for l in open(sys.argv[1])}
after = {json.loads(l)["task"]: json.loads(l) for l in open(sys.argv[2])}
for task in before:
    if task in after and abs(rr(before[task]) - rr(after[task])) > 1e-9:
        print("%+.2f  %.2f -> %.2f  %s" % (rr(after[task]) - rr(before[task]), rr(before[task]), rr(after[task]), task[:110]))
