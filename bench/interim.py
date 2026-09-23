"""Interim per-arm means over tasks completed by every arm (safe to run while a benchmark is going).

    python3 bench/interim.py <run dir>
"""
import json
import os
import sys
from collections import defaultdict

rows = [json.loads(l) for l in open(os.path.join(sys.argv[1], "runs.jsonl"))]
by_task = defaultdict(dict)
for r in rows:
    by_task[r["task_id"]][r["arm"]] = r
arms = sorted({r["arm"] for r in rows}, key=lambda a: (a != "baseline", a))
done = [t for t, d in by_task.items() if len(d) == len(arms)]
keys = ["wall_s", "reading_tokens", "injected_tokens", "total_input_tokens", "num_turns", "cost_usd", "recall", "recall_all_turns"]
print(f"tasks complete in all arms: {len(done)}")
print("| arm | " + " | ".join(keys) + " |")
print("|---|" + "---|" * len(keys))
for a in arms:
    vals = []
    for k in keys:
        xs = [by_task[t][a].get(k) for t in done if isinstance(by_task[t][a].get(k), (int, float))]
        vals.append("%.3g" % (sum(xs) / len(xs)) if xs else "-")
    print(f"| {a} | " + " | ".join(vals) + " |")
print("\nturn 2 answer (recall / precision) and hook actions:")
for a in arms:
    xs = [by_task[t][a].get("turn2") or {} for t in done]
    acts = defaultdict(int)
    for t in done:
        for k, v in (by_task[t][a].get("hook_actions") or {}).items():
            acts[k] += v
    print(a, round(sum(x.get("recall", 0) for x in xs) / len(xs), 3), round(sum(x.get("precision", 0) for x in xs) / len(xs), 3), dict(acts))
