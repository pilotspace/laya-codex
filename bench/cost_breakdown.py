"""Where the money goes: per-arm billing components from raw benchmark transcripts.

Sums the `result` events' usage per run (one per prompt) and prices them at the list prices in
bench/runs.py, next to the reported cost (the last result's total_cost_usd, a running total).
Also shows how much of the bill the tool results (reading) and laya's injection can explain.

    python3 bench/cost_breakdown.py <run dir> [<run dir> ...]
"""
import json
import os
import sys
from collections import defaultdict

from runs import PRICES_PER_M as PRICE

for out in sys.argv[1:]:
    runs = {}
    for line in open(os.path.join(out, "runs.jsonl")):
        r = json.loads(line)
        runs[(r["task_id"], r["arm"])] = r
    # only tasks finished by every arm, so arms are compared on the same tasks
    arms = sorted({a for _, a in runs}, key=lambda a: (a != "baseline", a))
    tasks = [t for t in {t for t, _ in runs} if all((t, a) in runs for a in arms)]
    agg = defaultdict(lambda: defaultdict(float))
    for t in tasks:
        for a in arms:
            usage = defaultdict(int)
            reported = 0.0
            for line in open(os.path.join(out, "raw", f"{t}_{a}.jsonl")):
                try:
                    e = json.loads(line)
                except ValueError:
                    continue
                if e.get("type") == "result":
                    for k in PRICE:
                        usage[k] += (e.get("usage") or {}).get(k) or 0
                    reported = e.get("total_cost_usd") or reported
            g = agg[a]
            for k in PRICE:
                g[k] += usage[k] / len(tasks)
                g["$" + k] += usage[k] * PRICE[k] / 1e6 / len(tasks)
            g["reported"] += reported / len(tasks)
            g["turns"] += (runs[(t, a)].get("num_turns") or 0) / len(tasks)
            g["reading"] += (runs[(t, a)].get("reading_tokens") or 0) / len(tasks)
            g["injected"] += (runs[(t, a)].get("injected_tokens") or 0) / len(tasks)
    print(f"\n{out}  (tasks finished by all arms: {len(tasks)}; means per task)")
    print("| arm | turns | cache read tok ($) | cache write tok ($) | uncached in ($) | output tok ($) | priced $ | reported $ | reading tok | injected tok |")
    print("|---|---|---|---|---|---|---|---|---|---|")
    for a in arms:
        g = agg[a]
        priced = sum(g["$" + k] for k in PRICE)
        print("| %s | %.1f | %.0fk ($%.3f) | %.0fk ($%.3f) | $%.3f | %.1fk ($%.3f) | $%.3f | $%.3f | %.1fk | %.1fk |" % (
            a, g["turns"], g["cache_read_input_tokens"] / 1e3, g["$cache_read_input_tokens"],
            g["cache_creation_input_tokens"] / 1e3, g["$cache_creation_input_tokens"], g["$input_tokens"],
            g["output_tokens"] / 1e3, g["$output_tokens"], priced, g["reported"], g["reading"] / 1e3, g["injected"] / 1e3))
