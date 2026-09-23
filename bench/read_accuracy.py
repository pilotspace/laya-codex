"""Read accuracy per arm: did the agent read the *right* code?

For every run (raw stream-json transcript + task gold files):
- read_precision: fraction of Read calls whose file is a gold file
- read_recall: fraction of gold files whose code the agent saw (a Read of it, or laya injecting
  its full code in the UserPromptSubmit context)
- wasted_read_tokens: tokens returned by Reads of non-gold files
- first_gold_read_turn: assistant turn index of the first Read hitting a gold file (lower = faster)

    python3 bench/read_accuracy.py <run dir> bench/tasks.jsonl
"""
import json
import os
import re
import sys
from collections import defaultdict

out, tasks_path = sys.argv[1], sys.argv[2]
gold = {json.loads(l)["id"]: set(json.loads(l)["gold"]) for l in open(tasks_path)}


def rel(path, gold_set):
    for g in gold_set:
        if path.endswith("/" + g) or path == g:
            return g
    return None


rows = defaultdict(list)
for name in sorted(os.listdir(os.path.join(out, "raw"))):
    task_id, arm = name[:-6].split("_", 1)
    g = gold.get(task_id)
    if not g:
        continue
    reads, names, turn, first_hit = [], {}, 0, None
    seen = set()
    wasted = 0
    for line in open(os.path.join(out, "raw", name)):
        try:
            e = json.loads(line)
        except ValueError:
            continue
        if e.get("type") == "system" and e.get("subtype") == "hook_response" and e.get("hook_event") == "UserPromptSubmit":
            ctx = e.get("output") or ""
            for m in re.finditer(r"### ([^\s:]+):\d+-\d+", ctx):  # spans injected with full code
                hit = rel(m.group(1), g)
                if hit:
                    seen.add(hit)
        if e.get("type") == "assistant":
            turn += 1
            for c in e.get("message", {}).get("content", []) or []:
                if c.get("type") == "tool_use" and c["name"] == "Read":
                    p = c["input"].get("file_path", "")
                    names[c["id"]] = p
                    hit = rel(p, g)
                    reads.append(hit is not None)
                    if hit:
                        seen.add(hit)
                        first_hit = turn if first_hit is None else first_hit
        if e.get("type") == "user":
            for c in e.get("message", {}).get("content", []) or []:
                if isinstance(c, dict) and c.get("type") == "tool_result" and c.get("tool_use_id") in names:
                    if not rel(names[c["tool_use_id"]], g):
                        body = c.get("content")
                        wasted += int(len(body if isinstance(body, str) else json.dumps(body)) / 3.5)
    rows[arm].append({
        "read_precision": sum(reads) / len(reads) if reads else None,
        "read_recall": len(seen) / len(g),
        "wasted_read_tokens": wasted,
        "reads": len(reads),
        "first_gold_read_turn": first_hit,
    })

print("| arm | n | Read calls | read precision | read recall (saw gold code) | wasted read tokens | first gold Read at turn |")
print("|---|---|---|---|---|---|---|")
for arm, rs in sorted(rows.items(), key=lambda x: (x[0] != "baseline", x[0])):
    prec = [r["read_precision"] for r in rs if r["read_precision"] is not None]
    first = [r["first_gold_read_turn"] for r in rs if r["first_gold_read_turn"] is not None]
    print("| %s | %d | %.2f | %.3f | %.3f | %.0f | %.2f (%d/%d runs Read a gold file) |" % (
        arm, len(rs), sum(r["reads"] for r in rs) / len(rs), sum(prec) / max(1, len(prec)),
        sum(r["read_recall"] for r in rs) / len(rs), sum(r["wasted_read_tokens"] for r in rs) / len(rs),
        sum(first) / max(1, len(first)), len(first), len(rs)))
