"""Soak test: concurrent Claude-like hook traffic against the laya-codex daemon, with injected faults.

Workers loop over sessions:
  SessionStart(startup) → UserPromptSubmit → Read (whole) → Read (again) → PostToolUse(Edit)
  → follow-up prompt → SessionStart(compact)
across the given repos. Faults, at fractions of the duration:
- 30%: append a line to an indexed file (stale index), restored at 60%;
- 50%: `laya-codex stop` (the daemon dies; hooks must fail open, and the daemon must autostart).

Reports per-event latency p50/p95/max, hook failures (non-zero exit or invalid JSON output; a
fail-open empty output is fine), fail-open counts, and the daemon's RSS over time.

    python3 bench/soak.py <laya-codex binary> <duration s> <repo> [<repo> ...]
"""
import json
import os
import random
import subprocess
import sys
import threading
import time
from collections import defaultdict

LAYA, DURATION, REPOS = sys.argv[1], float(sys.argv[2]), sys.argv[3:]
HOME = os.path.expanduser("~/.cache/laya-codex")
PROMPTS = [
    "Where is the WAL replayed on startup and how are corrupted segments handled?",
    "How does the daemon decide which code spans to inject for a prompt?",
    "fix(shard): gate unused graph-merge params under graph feature",
    "Which function renders the compact injection and how is it capped?",
    "Explain how sessions track spans already sent to the agent.",
    "Where are hook requests parsed and dispatched?",
]
FOLLOW_UP = "Now, for the same change, identify the tests that cover this code and the main call sites."
lat = defaultdict(list)
stats = defaultdict(int)
lock = threading.Lock()
stop_at = time.time() + DURATION


def files_of(repo):
    out = []
    for root, dirs, fs in os.walk(repo):
        dirs[:] = [d for d in dirs if not d.startswith(".") and d not in ("node_modules", "tmp") and not d.startswith("target")]
        out += [os.path.join(root, f) for f in fs if f.endswith((".rs", ".py", ".ts"))]
    return out


FILES = {r: files_of(r) for r in REPOS}


def hook(repo, payload, label):
    t0 = time.time()
    try:
        p = subprocess.run([LAYA, "hook"], input=json.dumps(payload), capture_output=True, text=True, cwd=repo, timeout=20)
        ms = (time.time() - t0) * 1000
        ok = p.returncode == 0
        if ok and p.stdout.strip():
            json.loads(p.stdout)
        with lock:
            lat[label].append(ms)
            stats["calls"] += 1
            stats["empty_output" if not p.stdout.strip() else "output"] += 1
            if not ok:
                stats["nonzero_exit"] += 1
    except (subprocess.TimeoutExpired, ValueError) as e:
        with lock:
            stats["failures"] += 1
            stats[type(e).__name__] += 1


def size(f):
    try:
        return os.path.getsize(f)
    except OSError:
        return 0


def worker(wid):
    rng = random.Random(wid)
    n = 0
    while time.time() < stop_at:
        try:
            one_session(rng, wid, n)
        except Exception as e:  # harness problem, not a laya failure: count it and keep going
            with lock:
                stats["harness_errors"] += 1
                stats["harness_" + type(e).__name__] += 1
        n += 1


def one_session(rng, wid, n):
    repo = rng.choice(REPOS)
    base = {"session_id": f"soak-{wid}-{n}-{time.time():.0f}", "cwd": repo}
    big = [f for f in FILES[repo] if size(f) > 20_000] or FILES[repo]
    path = rng.choice(big)
    # Unique prompt text per session: every prompt is scored cold, as in real use.
    prompt = f"{rng.choice(PROMPTS)} (ticket {wid}-{n})"
    hook(repo, {**base, "hook_event_name": "SessionStart", "source": "startup"}, "SessionStart")
    hook(repo, {**base, "hook_event_name": "UserPromptSubmit", "prompt": prompt}, "UserPromptSubmit")
    for _ in range(2):
        hook(repo, {**base, "hook_event_name": "PreToolUse", "tool_name": "Read", "tool_input": {"file_path": path}}, "PreToolUse:Read")
    hook(repo, {**base, "hook_event_name": "PostToolUse", "tool_name": "Edit", "tool_input": {"file_path": path}}, "PostToolUse:Edit")
    hook(repo, {**base, "hook_event_name": "UserPromptSubmit", "prompt": FOLLOW_UP}, "UserPromptSubmit")
    hook(repo, {**base, "hook_event_name": "SessionStart", "source": "compact"}, "SessionStart")


def daemon_rss_mb():
    try:
        pid = open(os.path.join(HOME, "daemon.pid")).read().strip()
        out = subprocess.run(["ps", "-o", "rss=", "-p", pid], capture_output=True, text=True).stdout.strip()
        return int(out) / 1024 if out else None
    except OSError:
        return None


def faults():
    victim = next(f for f in FILES[REPOS[0]] if size(f) > 20_000)
    original = open(victim, "rb").read()
    events = []
    try:
        time.sleep(DURATION * 0.3)
        with open(victim, "ab") as f:
            f.write(b"\n// soak: stale-index probe\n")
        events.append(("stale_file", time.time()))
        time.sleep(DURATION * 0.2)
        subprocess.run([LAYA, "stop"], capture_output=True)
        events.append(("daemon_killed", time.time()))
        time.sleep(DURATION * 0.1)
    finally:
        open(victim, "wb").write(original)  # always restore the file
        events.append(("file_restored", time.time()))
    print("faults:", [(e, round(t - start, 1)) for e, t in events], flush=True)


start = time.time()
threads = [threading.Thread(target=worker, args=(i,)) for i in range(4)] + [threading.Thread(target=faults)]
for t in threads:
    t.start()
rss = []
while any(t.is_alive() for t in threads[:4]):
    rss.append((round(time.time() - start), daemon_rss_mb()))
    time.sleep(30)
for t in threads:
    t.join()

print("stats:", dict(stats))
for label, v in sorted(lat.items()):
    v.sort()
    print(f"{label:18s} n={len(v):5d} p50={v[len(v)//2]:7.0f}ms p95={v[int(len(v)*0.95)]:7.0f}ms max={v[-1]:7.0f}ms")
print("daemon RSS MB over time:", rss)
