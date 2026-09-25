"""Paired bootstrap confidence intervals for treatment-vs-baseline changes.

`reading+injected tokens` -- tokens Claude reads plus tokens laya-codex injects, the VISION.md
token target -- is the primary token metric and prints first, with `code-reading tokens` beside
it for context (an injection that saves reads but costs more than it saves must show as a loss
here, not just as a smaller reads number).

    python3 bench/stats.py <run dir> <treatment arm> [baseline arm]
"""
import os
import random
import sys

from runs import load_runs


def reading(r):
    return (r.get("reading_tokens") or 0) + (r.get("injected_tokens") or 0)


METRICS = {
    "reading+injected tokens": reading,
    "code-reading tokens": lambda r: r.get("reading_tokens") or 0,
    "total input tokens": lambda r: r.get("total_input_tokens") or 0,
    "wall seconds": lambda r: r.get("wall_s") or 0,
    "turns": lambda r: r.get("num_turns") or 0,
    "cost usd": lambda r: r.get("cost_usd") or 0,
    # Output tokens drive wall time (wall ~ 5.2 + 10.8*output_ktok): a time proxy.
    "output tokens": lambda r: r.get("output_tokens") or 0,
}


def main(argv):
    out, arm = argv[1], argv[2]
    base = argv[3] if len(argv) > 3 else "baseline"
    # Repeated sessions of a task are averaged into one row (bench/runs.py), so tasks stay the unit.
    by = load_runs(os.path.join(out, "runs.jsonl"), arms=(arm, base))
    by = {a: by.get(a, {}) for a in (arm, base)}
    tasks = sorted(set(by[arm]) & set(by[base]))
    if not tasks:
        print("paired n=0, %s vs %s: no task finished by both arms" % (arm, base))
        return
    rng = random.Random(0)
    B = 10000
    print("paired n=%d, %s vs %s" % (len(tasks), arm, base))
    for name, f in METRICS.items():
        t = [f(by[arm][k]) for k in tasks]
        b = [f(by[base][k]) for k in tasks]
        sb = sum(b)
        point = sum(t) / sb - 1 if sb else float("nan")
        boots = []
        for _ in range(B):
            idx = [rng.randrange(len(tasks)) for _ in tasks]
            sbi = sum(b[i] for i in idx)
            boots.append(sum(t[i] for i in idx) / sbi - 1 if sbi else 0)
        boots.sort()
        lo, hi = boots[int(0.025 * B)], boots[int(0.975 * B)]
        wins = sum(x < y for x, y in zip(t, b))
        print("  %-24s %+6.1f%%  95%% CI [%+6.1f%%, %+6.1f%%]  lower in %d/%d tasks" % (
            name, 100 * point, 100 * lo, 100 * hi, wins, len(tasks)))
    for q in ("recall", "precision"):
        t = sum(by[arm][k].get(q, 0) for k in tasks) / len(tasks)
        b = sum(by[base][k].get(q, 0) for k in tasks) / len(tasks)
        print("  %-24s %.3f vs %.3f" % (q, t, b))


if __name__ == "__main__":
    main(sys.argv)
