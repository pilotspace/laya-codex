"""Smoke-test adaptive injection end to end: prompts in one session through `laya hook`, then a
compact reset. Shows which spans got full code, which were skipped as already sent, and timing.

    python3 bench/smoke_adaptive.py <repo> [prompt ...]
"""
import json
import os
import subprocess
import sys
import time

LAYA = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "target", "release", "laya")
REPO = sys.argv[1]
PROMPTS = sys.argv[2:] or [
    "Where is the WAL replayed on startup and how are corrupted segments handled?",
    "Where is the WAL replayed on startup and how are corrupted segments handled? Also show the tests.",
]
ENV = dict(os.environ, LAYA_ADAPTIVE="1", LAYA_RENDER="compact")


def hook(payload):
    t0 = time.time()
    p = subprocess.run([LAYA, "hook"], input=json.dumps(payload), capture_output=True, text=True, cwd=REPO, env=ENV, timeout=30)
    out = json.loads(p.stdout) if p.stdout.strip() else {}
    return out.get("hookSpecificOutput", {}).get("additionalContext", ""), time.time() - t0


def show(label, ctx, dt):
    lines = ctx.splitlines()
    heads = [l for l in lines if l.startswith("### ")]
    print(f"--- {label}: {dt:.2f}s, {len(ctx)} chars, full-code spans {len(heads)}")
    for l in heads + [l[:220] for l in lines if l.startswith("Already provided")]:
        print("   ", l)


subprocess.run([LAYA, "query", "warmup wal", "--repo", REPO], capture_output=True, env=ENV, timeout=180)
base = {"session_id": "smoke-%d" % time.time(), "cwd": REPO}
for i, prompt in enumerate(PROMPTS):
    show(f"prompt {i + 1}", *hook({**base, "hook_event_name": "UserPromptSubmit", "prompt": prompt}))
hook({**base, "hook_event_name": "SessionStart", "source": "compact"})
show("prompt 1 after compact", *hook({**base, "hook_event_name": "UserPromptSubmit", "prompt": PROMPTS[0]}))
