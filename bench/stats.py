"""Paired bootstrap confidence intervals for treatment-vs-baseline changes.

    python3 bench/stats.py <run dir> <treatment arm> [baseline arm]
"""
import json
import os
import random
import sys

from runs import load_runs

out, arm = sys.argv[1], sys.argv[2]
base = sys.argv[3] if len(sys.argv) > 3 else "baseline"
# Repeated sessions of a task are averaged into one row (bench/runs.py), so tasks stay the unit.
by = load_runs(os.path.join(out, "runs.jsonl"), arms=(arm, base))
by = {a: by.get(a, {}) for a in (arm, base)}
tasks = sorted(set(by[arm]) & set(by[base]))


def reading(r):
    return (r["reading_tokens"] or 0) + (r["injected_tokens"] or 0)


METRICS = {
    "reading+injected tokens": reading,
    "code-reading tokens": lambda r: r["reading_tokens"] or 0,
    "total input tokens": lambda r: r["total_input_tokens"] or 0,
    "wall seconds": lambda r: r["wall_s"],
    "turns": lambda r: r["num_turns"] or 0,
    "cost usd": lambda r: r["cost_usd"] or 0,
}
rng = random.Random(0)
B = 10000
print("paired n=%d, %s vs %s" % (len(tasks), arm, base))
for name, f in METRICS.items():
    t = [f(by[arm][k]) for k in tasks]
    b = [f(by[base][k]) for k in tasks]
    point = sum(t) / sum(b) - 1
    boots = []
    for _ in range(B):
        idx = [rng.randrange(len(tasks)) for _ in tasks]
        sb = sum(b[i] for i in idx)
        boots.append(sum(t[i] for i in idx) / sb - 1 if sb else 0)
    boots.sort()
    lo, hi = boots[int(0.025 * B)], boots[int(0.975 * B)]
    wins = sum(x < y for x, y in zip(t, b))
    print("  %-24s %+6.1f%%  95%% CI [%+6.1f%%, %+6.1f%%]  lower in %d/%d tasks" % (name, 100 * point, 100 * lo, 100 * hi, wins, len(tasks)))
for q in ("recall", "precision"):
    t = sum(by[arm][k][q] for k in tasks) / len(tasks)
    b = sum(by[base][k][q] for k in tasks) / len(tasks)
    print("  %-24s %.3f vs %.3f" % (q, t, b))
