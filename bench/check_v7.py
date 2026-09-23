"""Spot-check v7 mechanics in a run dir: injection sizes vs the 9,500-char cap, presence of the
"Definitions and uses" section, and what Claude did after each narrowed Read (did it read the
same file again, and was that a whole-file re-read?).

    python3 bench/check_v7.py <run dir>
"""
import json
import os
import sys
from collections import defaultdict

out = sys.argv[1]
sizes, with_usages, narrowed, reread_same, reread_full = [], 0, 0, 0, 0
per_arm = defaultdict(lambda: {"inj": 0, "usages": 0})
for name in sorted(os.listdir(os.path.join(out, "raw"))):
    arm = name[:-6].split("_", 1)[1]
    if arm == "baseline":
        continue
    reads = []  # (path, has_range)
    narrowed_paths = set()
    for line in open(os.path.join(out, "raw", name)):
        try:
            e = json.loads(line)
        except ValueError:
            continue
        if e.get("type") == "system" and e.get("subtype") == "hook_response":
            try:
                hso = json.loads(e.get("output") or "{}").get("hookSpecificOutput", {})
            except ValueError:
                hso = {}
            ctx = hso.get("additionalContext") or ""
            if e.get("hook_event") == "UserPromptSubmit":
                sizes.append(len(ctx))
                per_arm[arm]["inj"] += 1
                if "Definitions and uses:" in ctx:
                    with_usages += 1
                    per_arm[arm]["usages"] += 1
            elif e.get("hook_event") == "PreToolUse" and hso.get("updatedInput") and "best match for the task" in ctx:
                narrowed += 1
                narrowed_paths.add(hso["updatedInput"].get("file_path"))
        if e.get("type") == "assistant":
            for c in e.get("message", {}).get("content", []) or []:
                if c.get("type") == "tool_use" and c["name"] == "Read":
                    i = c["input"]
                    reads.append((i.get("file_path"), "offset" in i or "limit" in i))
    for p in narrowed_paths:
        later = [r for r in reads if r[0] == p][1:]  # reads after the first (narrowed) one
        if later:
            reread_same += 1
            reread_full += any(not ranged for _, ranged in later)
sizes.sort()
print(f"prompt injections: {len(sizes)}, max {max(sizes)} chars, p50 {sizes[len(sizes)//2]}, over 9500: {sum(s > 9500 for s in sizes)}")
print(f"with 'Definitions and uses': {with_usages}/{len(sizes)}", {a: f"{v['usages']}/{v['inj']}" for a, v in per_arm.items()})
print(f"narrowed whole-file Reads: {narrowed}; file read again later: {reread_same}; of which whole-file again: {reread_full}")
