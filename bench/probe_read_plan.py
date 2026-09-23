"""Probe the large-file Read plan end to end through `laya hook`: submit a prompt in a fresh
session, then send PreToolUse Read events (whole file, then the same file again) and print what
Claude Code would receive.

    python3 bench/probe_read_plan.py <laya binary> <repo> <file relative to repo> "<prompt>"
"""
import json
import os
import subprocess
import sys
import time

laya, repo, rel, prompt = sys.argv[1:5]
session = "probe-%d" % time.time()


def hook(payload):
    p = subprocess.run([laya, "hook"], input=json.dumps(payload), capture_output=True, text=True, cwd=repo, timeout=30)
    return json.loads(p.stdout) if p.stdout.strip() else {}


base = {"session_id": session, "cwd": repo}
ctx = hook({**base, "hook_event_name": "UserPromptSubmit", "prompt": prompt})
print("prompt injection chars:", len(ctx.get("hookSpecificOutput", {}).get("additionalContext", "")))
path = os.path.join(repo, rel)
for attempt in (1, 2):
    out = hook({**base, "hook_event_name": "PreToolUse", "tool_name": "Read", "tool_input": {"file_path": path}})
    hso = out.get("hookSpecificOutput", {})
    print(f"--- Read #{attempt} of {rel} ({sum(1 for _ in open(path))} lines)")
    print("updatedInput:", json.dumps(hso.get("updatedInput")))
    print("additionalContext:", (hso.get("additionalContext") or "")[:1500])
