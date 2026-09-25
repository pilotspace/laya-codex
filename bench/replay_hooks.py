"""Replay benchmark sessions through the real `laya-codex hook`, without Claude.

Each task becomes one hook session with the benchmark's two prompts (`run_bench.PROMPT`, then
`run_bench.FOLLOWUP`), so the adaptive session delta and follow-up handling run exactly as in a
benchmark session. Per prompt it records what the injection would cost and what it contains:

- injected characters and the files whose code was inlined (`### path:a-b` blocks), in rank order;
- the files the ranked map and the related lists name, beyond what was inlined;
- gold coverage: gold files inlined, gold files inlined among the first two blocks (Laya's job is
  to rank the right code first, not just eventually include it), and gold files named anywhere;
- the rank mode, scored/offered/candidate counts, and the hook's own latency, from the hook log.

This is the reranker gate the plan names: run it before and after a ranking change and compare the
`summary` table -- gold inlined must not fall, for no more injected chars.

    python3 bench/replay_hooks.py --bin target/release/laya-codex --home /tmp/lc-home \\
        --moon-port 16530 --repo <clone> --tasks bench/tasks-v8/httpx.jsonl --out replay.jsonl \\
        [--env LAYA_CODEX_WEIGHT=1] [--label model-only]
    python3 bench/replay_hooks.py summary replay-a.jsonl replay-b.jsonl

Use a scratch `--home` and `--moon-port` per binary/config: the daemon and Moon belong to the
home, and a shared one would serve every binary from whichever started first. Every path here is a
caller-supplied argument; nothing is specific to one machine or one run (see bench/replay_decide.sh,
which drives this module across ranking arms and repos with no hard-coded paths of its own).
"""
import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from run_bench import FOLLOWUP, PROMPT, is_test_path  # noqa: E402

INLINED = re.compile(r"^### (\S+?):(\d+)-(\d+)", re.M)
NAMED = re.compile(r"([\w./@-]+\.[A-Za-z]{1,5}):\d+")
MAP_LINE = re.compile(r"^\d+\. (\S+) — ", re.M)


def gold_hit(path, gold):
    return [g for g in gold if path == g or g.endswith("/" + path) or path.endswith(g)]


def parse(ctx, gold):
    """What one hook response cost and contained. `inlined` is in the order the blocks appear in
    the injection, i.e. rank order: the ranker's best guess comes first."""
    inlined = list(dict.fromkeys(m.group(1) for m in INLINED.finditer(ctx)))
    named = set(inlined) | {m.group(1) for m in NAMED.finditer(ctx)} | {m.group(1) for m in MAP_LINE.finditer(ctx)}
    g_in = sorted({g for p in inlined for g in gold_hit(p, gold)})
    g_named = sorted({g for p in named for g in gold_hit(p, gold)})
    g_top2 = sorted({g for p in inlined[:2] for g in gold_hit(p, gold)})
    return {"chars": len(ctx), "inlined": inlined, "gold_inlined": g_in, "gold_named": g_named, "gold_top2": g_top2}


def run(args):
    env = dict(os.environ, LAYA_CODEX_HOME=args.home, LAYA_CODEX_MOON_PORT=str(args.moon_port),
               LAYA_CODEX_ADAPTIVE="1", LAYA_CODEX_RENDER="compact", LAYA_CODEX_MEMO="0")
    if args.model_dir:
        env["LAYA_CODEX_MODEL_DIR"] = args.model_dir
    if args.moon_bin:
        env["LAYA_CODEX_MOON_BIN"] = args.moon_bin
    for kv in args.env:
        k, _, v = kv.partition("=")
        env[k] = v
    os.makedirs(args.home, exist_ok=True)
    tasks = [json.loads(l) for l in open(args.tasks) if l.strip()][: args.limit or None]
    # A daemon left running by another build or setting would serve this replay: stop it first.
    subprocess.run([args.bin, "stop"], env=env, capture_output=True, timeout=60)
    subprocess.run([args.bin, "index", args.repo], env=env, capture_output=True, timeout=900)
    # Warm the model: Metal compiles its kernels on the first run (~10 s).
    subprocess.run([args.bin, "query", "warm up the model", "--repo", args.repo], env=env,
                   capture_output=True, timeout=300)
    log = tempfile.NamedTemporaryFile(prefix="hooklog-", suffix=".jsonl", delete=False).name
    env["LAYA_CODEX_HOOK_LOG"] = log
    with open(args.out, "a") as out:
        for t in tasks:
            sid = f"replay-{args.label}-{t['id']}-{int(time.time() * 1000)}"
            base = {"session_id": sid, "cwd": args.repo, "hook_event_name": "UserPromptSubmit"}
            for turn, prompt in enumerate([PROMPT.format(task=t["task"]), FOLLOWUP], 1):
                before = os.path.getsize(log) if os.path.exists(log) else 0
                t0 = time.time()
                p = subprocess.run([args.bin, "hook"], input=json.dumps({**base, "prompt": prompt}),
                                   capture_output=True, text=True, cwd=args.repo, env=env, timeout=60)
                dt = time.time() - t0
                ctx = ""
                if p.stdout.strip():
                    ctx = json.loads(p.stdout).get("hookSpecificOutput", {}).get("additionalContext", "")
                with open(log) as f:
                    f.seek(before)
                    lines = [json.loads(l) for l in f if l.strip()]
                entry = lines[-1] if lines else {}
                row = {"label": args.label, "repo": os.path.basename(os.path.normpath(args.repo)),
                       "task_id": t["id"], "turn": turn, "gold": t["gold"], "hook_s": round(dt, 3),
                       "action": entry.get("action"), "rank_mode": entry.get("rank_mode"),
                       "scored": entry.get("scored"), "offered": entry.get("offered"),
                       "candidates": entry.get("candidates"), **parse(ctx, t["gold"])}
                out.write(json.dumps(row) + "\n")
                out.flush()
    os.unlink(log)


def percentile(values, p):
    """Nearest-rank percentile (0 <= p <= 1) of a non-empty list."""
    ordered = sorted(values)
    idx = min(len(ordered) - 1, int(len(ordered) * p))
    return ordered[idx]


def summary(paths):
    rows = [json.loads(l) for p in paths for l in open(p) if l.strip()]
    keys = sorted({(r["label"], r["repo"]) for r in rows})
    print("| label | repo | turn | prompts | chars mean | code blocks | gold inlined | gold in top 2 | "
          "gold named | test gold named (turn 2) | tasks with gold inlined | hook s p50 | hook s p95 | "
          "rank modes |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for label, repo in keys:
        for turn in (1, 2):
            rs = [r for r in rows if r["label"] == label and r["repo"] == repo and r["turn"] == turn]
            if not rs:
                continue
            n = len(rs)
            chars = sum(r["chars"] for r in rs) / n
            blocks = sum(len(r["inlined"]) for r in rs) / n
            g_in = sum(len(r["gold_inlined"]) for r in rs)
            g_top2 = sum(len(r.get("gold_top2") or []) for r in rs)  # older replays lack this field
            g_named = sum(len(r["gold_named"]) for r in rs)
            g_total = sum(len(r["gold"]) for r in rs)
            tests = sum(len([g for g in r["gold_named"] if is_test_path(g)]) for r in rs)
            tests_total = sum(len([g for g in r["gold"] if is_test_path(g)]) for r in rs)
            tasks_in = sum(1 for r in rs if r["gold_inlined"])
            hook_times = [r["hook_s"] for r in rs]
            p50, p95 = percentile(hook_times, 0.50), percentile(hook_times, 0.95)
            modes = {}
            for r in rs:
                k = r["rank_mode"] or r["action"] or "?"
                modes[k] = modes.get(k, 0) + 1
            test_cell = f"{tests}/{tests_total}" if turn == 2 else "–"
            print(f"| {label} | {repo} | {turn} | {n} | {chars:,.0f} | {blocks:.2f} | {g_in}/{g_total} | "
                  f"{g_top2}/{g_total} | {g_named}/{g_total} | {test_cell} | {tasks_in}/{n} | {p50:.2f} | "
                  f"{p95:.2f} | " + ", ".join(f"{k} {v}" for k, v in sorted(modes.items())) + " |")


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "summary":
        return summary(sys.argv[2:])
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--home", required=True)
    ap.add_argument("--moon-port", type=int, required=True)
    ap.add_argument("--repo", required=True)
    ap.add_argument("--tasks", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--label", default="replay")
    ap.add_argument("--model-dir")
    ap.add_argument("--moon-bin")
    ap.add_argument("--env", action="append", default=[], help="KEY=VALUE for the hook and daemon")
    ap.add_argument("--limit", type=int, default=0)
    run(ap.parse_args())


if __name__ == "__main__":
    main()
