"""Per-arm breakdown of hook actions and tool calls from a benchmark run directory."""
import json
import os
import sys
from collections import Counter, defaultdict

out = sys.argv[1]
rows = [json.loads(l) for l in open(os.path.join(out, "runs.jsonl"))]
actions, tools, n = defaultdict(Counter), defaultdict(Counter), Counter()
for r in rows:
    n[r["arm"]] += 1
    actions[r["arm"]].update(r.get("hook_actions") or {})
    tools[r["arm"]].update(r.get("tool_calls") or {})
for arm in sorted(n):
    print("%s (n=%d)" % (arm, n[arm]))
    print("  tool calls/run:", {k: round(v / n[arm], 2) for k, v in tools[arm].most_common()})
    if actions[arm]:
        print("  hook actions/run:", {k: round(v / n[arm], 2) for k, v in actions[arm].most_common()})
