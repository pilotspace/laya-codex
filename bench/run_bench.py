"""Paired Claude Code benchmark: baseline vs laya-codex.

Measures, per run: codebase-reading tokens (Read/Grep/Glob results + laya-injected context),
total input tokens, cost, wall-clock, turns, and answer accuracy (gold files from git history).

    python3 bench/run_bench.py tasks --repo <clone> --skip 40 --n 20 --out bench/tasks.jsonl
    python3 bench/run_bench.py run --repo <clone> --tasks bench/tasks.jsonl --arms baseline,laya \
        --out bench/results/<name> [--model sonnet] [--limit N]
    python3 bench/run_bench.py report --out bench/results/<name>
"""
import argparse
import json
import os
import random
import re
import subprocess
import sys
import time

SRC_EXT = (".rs", ".py", ".ts", ".tsx", ".js", ".go", ".java", ".c", ".h", ".cc", ".cpp", ".hpp", ".rb", ".php", ".kt", ".swift", ".cs")
HERE = os.path.dirname(os.path.abspath(__file__))

PROMPT = ("In this repository, find the source code that implements or would need to change for the following "
          "change, and briefly explain how it works:\n\n\"{task}\"\n\n"
          "Be efficient: read only what you need. End your answer with one line exactly of the form\n"
          "FILES: <comma-separated repo-relative paths of the most relevant source files>")


def git(repo, *args):
    return subprocess.run(["git", "-C", repo, *args], capture_output=True, text=True, check=True).stdout


def is_test_path(path):
    """Test file by the usual Rust / Python / TS conventions (tests/ dir, test_*.py, *_test.*, *.test.*, *.spec.*)."""
    base = path.rsplit("/", 1)[-1]
    return bool(re.search(r"(^|/)(tests?|__tests__)/", path) or base.startswith("test_")
                or re.search(r"(_test|\.test|\.spec)\.[a-z]+$", base))


def make_tasks(args):
    tracked = set(git(args.repo, "ls-files").splitlines())
    log = git(args.repo, "log", "--no-merges", "-n", "2000", "--name-only", "--format=@@%H%x09%s")
    seen, tasks = 0, []
    for block in log.split("@@")[1:]:
        head, *files = [l for l in block.splitlines() if l.strip()]
        sha, subj = head.split("\t", 1)
        gold = sorted({f for f in files if f in tracked and f.endswith(SRC_EXT) and "/vendor/" not in f})
        subj = re.sub(r"\(#\d+\)$", "", subj).strip()
        if not (1 <= len(gold) <= 4 and len(subj) >= 25 and not subj.lower().startswith(("merge", "chore(release", "bump"))):
            continue
        seen += 1
        if seen <= args.skip:  # the first `skip` qualifying commits were used by the spike; keep them out
            continue
        # keep feature/behaviour descriptions; test-only, lint and formatting commits are not "find the code" tasks
        if subj.lower().startswith(("test(", "style", "docs")) or re.search(r"clippy|lint|fmt|typo|RED\b", subj, re.I):
            continue
        # --code-only (v8, repos whose tests are separate source files): drop reverts, ruff/mypy fixes (linters
        # the v7 regex does not name) and commits that touch only tests -- "moving test cases" is not a "find the
        # code" task. Off by default; the v7 moon task set is unchanged with the flag on (docs/RESULTS.md v8).
        if getattr(args, "code_only", False) and (subj.lower().startswith("revert") or re.search(r"\bruff\b|\bmypy\b", subj, re.I)
                                                  or all(is_test_path(g) for g in gold)):
            continue
        tasks.append({"id": sha[:10], "task": subj, "gold": gold})
        if len(tasks) >= args.n:
            break
    with open(args.out, "w") as f:
        for t in tasks:
            f.write(json.dumps(t) + "\n")
    print("wrote %d tasks to %s" % (len(tasks), args.out))


def render_configs(out_dir):
    """Materialize bench/config/*.json templates with the absolute laya-codex binary path."""
    laya_bin = os.environ.get("LAYA_CODEX_BIN") or os.path.abspath(os.path.join(HERE, "..", "target", "release", "laya-codex"))
    if not os.path.exists(laya_bin):
        sys.exit("laya-codex binary not found at %s (build with cargo build --release -p laya-cli or set LAYA_CODEX_BIN)" % laya_bin)
    dst = os.path.join(out_dir, "config")
    os.makedirs(dst, exist_ok=True)
    for name in os.listdir(os.path.join(HERE, "config")):
        src = open(os.path.join(HERE, "config", name)).read().replace("@LAYA_CODEX_BIN@", laya_bin)
        open(os.path.join(dst, name), "w").write(src)
    return dst


def arm_flags(arm, cfg_dir):
    # --tools also takes effect for MCP tools only through --mcp-config; list them explicitly.
    base = ["--setting-sources", "project", "--strict-mcp-config", "--permission-mode", "bypassPermissions"]
    if arm == "baseline":
        return base + ["--tools", "Read,Grep,Glob"]
    flags = base + ["--tools", "Read,Grep,Glob", "--settings", os.path.join(cfg_dir, "laya-settings.%s.json" % arm)]
    mcp = os.path.join(cfg_dir, "laya-mcp.%s.json" % arm)
    if os.path.exists(mcp):
        flags += ["--mcp-config", mcp]
    return flags


def read_hook_log(path):
    out = {"injected_tokens": 0, "hook_actions": {}}
    if not os.path.exists(path):
        return out
    for line in open(path):
        try:
            e = json.loads(line)
        except ValueError:
            continue
        out["injected_tokens"] += tok_estimate("x" * int(e.get("injected_chars") or 0))
        a = e.get("action", "?")
        out["hook_actions"][a] = out["hook_actions"].get(a, 0) + 1
    return out


def tok_estimate(text):
    return int(len(text) / 3.5)


def parse_stream(lines):
    """Extract usage and reading cost from `claude -p --output-format stream-json --verbose` output."""
    out = {"reading_tokens": 0, "injected_tokens": 0, "tool_calls": {}, "read_bytes": 0, "result": "", "usage": {},
           "cost_usd": None, "num_turns": None, "api_ms": None, "is_error": None, "hook_events": 0}
    tool_names = {}
    for line in lines:
        try:
            e = json.loads(line)
        except ValueError:
            continue
        t = e.get("type")
        if t == "assistant":
            for c in e.get("message", {}).get("content", []) or []:
                if c.get("type") == "tool_use":
                    tool_names[c["id"]] = c["name"]
                    out["tool_calls"][c["name"]] = out["tool_calls"].get(c["name"], 0) + 1
        elif t == "user":
            for c in e.get("message", {}).get("content", []) or []:
                if isinstance(c, dict) and c.get("type") == "tool_result":
                    content = c.get("content")
                    text = content if isinstance(content, str) else json.dumps(content)
                    name = tool_names.get(c.get("tool_use_id"), "?")
                    if name in ("Read", "Grep", "Glob") or name.startswith("mcp__laya"):
                        out["reading_tokens"] += tok_estimate(text)
                        out["read_bytes"] += len(text)
        elif t == "system" and "hook" in str(e.get("subtype", "")):
            out["hook_events"] += 1
        elif t == "result":
            out["result"] = e.get("result", "") or ""
            out["usage"] = e.get("usage", {}) or {}
            out["cost_usd"] = e.get("total_cost_usd")
            out["num_turns"] = e.get("num_turns")
            out["api_ms"] = e.get("duration_api_ms")
            out["is_error"] = e.get("is_error")
    return out


def grade(answer, gold):
    m = re.findall(r"FILES:\s*(.+)", answer)
    named = [p.strip().strip("`").lstrip("./") for p in (m[-1].split(",") if m else []) if p.strip()]
    hit = [g for g in gold if any(n == g or g.endswith("/" + n) or n.endswith(g) for n in named)]
    recall = len(hit) / len(gold)
    precision = (len({n for n in named if any(n == g or g.endswith("/" + n) or n.endswith(g) for g in gold)}) / len(named)) if named else 0.0
    return {"named": named, "recall": recall, "precision": precision, "hit_any": float(bool(hit))}


FOLLOWUP = ("Now, for the same change, identify the tests that cover this code and the main call sites that invoke it. "
            "Be efficient: read only what you need. End your answer with one line exactly of the form\n"
            "FILES: <comma-separated repo-relative paths of the most relevant source files>")


def _claude(prompt, arm, args, cfg_dir, env, session_flags):
    cmd = ["claude", "-p", prompt, "--model", args.model, "--output-format", "stream-json", "--verbose",
           "--include-hook-events", "--max-budget-usd", str(args.max_usd)] + session_flags + arm_flags(arm, cfg_dir)
    t0 = time.time()
    try:
        p = subprocess.run(cmd, cwd=args.repo, capture_output=True, text=True, timeout=args.timeout, env=env)
        lines, rc = p.stdout.splitlines(), p.returncode
    except subprocess.TimeoutExpired as ex:
        out = ex.stdout or ""
        lines, rc = (out.decode(errors="ignore") if isinstance(out, bytes) else out).splitlines(), "timeout"
    return lines, rc, time.time() - t0


def run_one(arm, task, args, cfg_dir):
    """One task = one Claude session of `args.turns` prompts (turn 2+ resume the same session)."""
    import uuid
    hook_log = os.path.join(args.out, "hooklogs", "%s_%s.jsonl" % (task["id"], arm))
    os.makedirs(os.path.dirname(hook_log), exist_ok=True)
    if os.path.exists(hook_log):
        os.remove(hook_log)
    # LAYA_CODEX_MEMO=0: score every prompt cold, as a new prompt is in real use; otherwise whichever arm
    # runs a task first pays the model run and the others hit its cache.
    env = dict(os.environ, LAYA_CODEX_HOOK_LOG=hook_log, LAYA_CODEX_MEMO=os.environ.get("LAYA_CODEX_MEMO", "0"))
    prompts = [PROMPT.format(task=task["task"])] + [FOLLOWUP] * (args.turns - 1)
    sid = str(uuid.uuid4())
    all_lines, wall, rcs = [], 0.0, []
    agg = {"reading_tokens": 0, "total_in": 0, "output": 0, "cost": 0.0, "turns": 0, "tool_calls": {}}
    answers = []
    for k, prompt in enumerate(prompts):
        flags = (["--no-session-persistence"] if args.turns == 1 else ["--session-id", sid]) if k == 0 else ["--resume", sid]
        lines, rc, w = _claude(prompt, arm, args, cfg_dir, env, flags)
        all_lines += lines
        wall += w
        rcs.append(rc)
        r = parse_stream(lines)
        u = r["usage"]
        agg["reading_tokens"] += r["reading_tokens"]
        agg["total_in"] += (u.get("input_tokens") or 0) + (u.get("cache_creation_input_tokens") or 0) + (u.get("cache_read_input_tokens") or 0)
        agg["output"] += u.get("output_tokens") or 0
        agg["cost"] += r["cost_usd"] or 0
        agg["turns"] += r["num_turns"] or 0
        for t, n in r["tool_calls"].items():
            agg["tool_calls"][t] = agg["tool_calls"].get(t, 0) + n
        answers.append(r["result"])
    h = read_hook_log(hook_log)
    row = {"arm": arm, "task_id": task["id"], "wall_s": round(wall, 2), "rc": rcs, "total_input_tokens": agg["total_in"],
           "output_tokens": agg["output"], "reading_tokens": agg["reading_tokens"], "injected_tokens": h["injected_tokens"],
           "cost_usd": round(agg["cost"], 6), "num_turns": agg["turns"], "tool_calls": agg["tool_calls"],
           "hook_actions": h["hook_actions"], "prompts": len(prompts)}
    row.update(grade(answers[0], task["gold"]))
    if len(answers) > 1:
        named = [n for a in answers for n in grade(a, task["gold"])["named"]]
        row["recall_all_turns"] = len({g for g in task["gold"]
                                       if any(n == g or g.endswith("/" + n) or n.endswith(g) for n in named)}) / len(task["gold"])
        row["turn2"] = grade(answers[1], task["gold"])
    return row, all_lines


def run(args):
    tasks = [json.loads(l) for l in open(args.tasks)][: args.limit or None]
    arms = args.arms.split(",")
    os.makedirs(os.path.join(args.out, "raw"), exist_ok=True)
    res_path = os.path.join(args.out, "runs.jsonl")
    done = set()
    if os.path.exists(res_path):
        done = {(r["arm"], r["task_id"]) for r in map(json.loads, open(res_path))}
    cfg_dir = render_configs(args.out)
    # Restart the daemon so the first hook starts it with this run's environment (LAYA_CODEX_MEMO etc.).
    laya_bin = os.environ.get("LAYA_CODEX_BIN") or os.path.abspath(os.path.join(HERE, "..", "target", "release", "laya-codex"))
    subprocess.run([laya_bin, "stop"], capture_output=True)
    rng = random.Random(7)
    plan = [(a, t) for t in tasks for a in rng.sample(arms, len(arms))]  # interleave arms per task, random order
    for i, (arm, task) in enumerate(plan):
        if (arm, task["id"]) in done:
            continue
        row, lines = run_one(arm, task, args, cfg_dir)
        with open(os.path.join(args.out, "raw", "%s_%s.jsonl" % (task["id"], arm)), "w") as f:
            f.write("\n".join(lines))
        with open(res_path, "a") as f:
            f.write(json.dumps(row) + "\n")
        print("[%d/%d] %-10s %s wall=%5.1fs read=%6d inj=%5d total_in=%7d recall=%.2f cost=%s" % (
            i + 1, len(plan), arm, task["id"], row["wall_s"], row["reading_tokens"], row["injected_tokens"],
            row["total_input_tokens"], row["recall"], row["cost_usd"]), flush=True)
    report(args)


def report(args):
    rows = [json.loads(l) for l in open(os.path.join(args.out, "runs.jsonl"))]
    arms = sorted({r["arm"] for r in rows}, key=lambda a: (a != "baseline", a))
    by = {a: {r["task_id"]: r for r in rows if r["arm"] == a} for a in arms}
    common = set.intersection(*[set(v) for v in by.values()])
    keys = ["reading_tokens", "injected_tokens", "total_input_tokens", "output_tokens", "wall_s", "num_turns", "cost_usd",
            "recall", "precision", "hit_any"] + (["recall_all_turns"] if all("recall_all_turns" in r for r in rows) else [])

    def mean(a, k):
        vals = [by[a][t][k] or 0 for t in common]
        return sum(vals) / max(1, len(vals))

    def median_ratio(a, k):
        rs = sorted((by[a][t][k] or 0) / by["baseline"][t][k] for t in common if by["baseline"][t][k])
        return rs[len(rs) // 2] if rs else float("nan")

    summary = {"n_tasks_paired": len(common), "arms": {}}
    lines = ["| metric | " + " | ".join(arms) + " |", "|---" * (len(arms) + 1) + "|"]
    for k in keys:
        lines.append("| %s | %s |" % (k, " | ".join("%.3f" % mean(a, k) if k in ("recall", "precision", "hit_any", "cost_usd", "recall_all_turns")
                                                    else "%.1f" % mean(a, k) for a in arms)))
    for a in arms:
        summary["arms"][a] = {k: mean(a, k) for k in keys}
        if a != "baseline":
            read_cost = lambda arm, t: (by[arm][t]["reading_tokens"] or 0) + (by[arm][t]["injected_tokens"] or 0)
            tot_b = sum(read_cost("baseline", t) for t in common)
            tot_a = sum(read_cost(a, t) for t in common)
            summary["arms"][a]["reading_cost_change_pct"] = round(100 * (tot_a - tot_b) / max(1, tot_b), 1)
            wb = sum(by["baseline"][t]["wall_s"] for t in common)
            wa = sum(by[a][t]["wall_s"] for t in common)
            summary["arms"][a]["wall_change_pct"] = round(100 * (wa - wb) / max(1e-9, wb), 1)
            summary["arms"][a]["median_wall_ratio"] = round(median_ratio(a, "wall_s"), 3)
            summary["arms"][a]["median_total_input_ratio"] = round(median_ratio(a, "total_input_tokens"), 3)
            lines.append("| **%s vs baseline** | reading+injected %+.1f%% · wall %+.1f%% · median wall ratio %.2f · median total-input ratio %.2f |" % (
                a, summary["arms"][a]["reading_cost_change_pct"], summary["arms"][a]["wall_change_pct"],
                summary["arms"][a]["median_wall_ratio"], summary["arms"][a]["median_total_input_ratio"]))
    md = "\n".join(lines)
    print(md)
    json.dump(summary, open(os.path.join(args.out, "summary.json"), "w"), indent=1)
    open(os.path.join(args.out, "summary.md"), "w").write(md + "\n")


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    t = sub.add_parser("tasks")
    t.add_argument("--repo", required=True)
    t.add_argument("--skip", type=int, default=40)
    t.add_argument("--n", type=int, default=20)
    t.add_argument("--out", required=True)
    t.add_argument("--code-only", action="store_true", help="drop reverts and test-only commits (v8 repos)")
    r = sub.add_parser("run")
    r.add_argument("--repo", required=True)
    r.add_argument("--tasks", required=True)
    r.add_argument("--arms", default="baseline,laya")
    r.add_argument("--out", required=True)
    r.add_argument("--model", default="sonnet")
    r.add_argument("--limit", type=int, default=0)
    r.add_argument("--timeout", type=int, default=900)
    r.add_argument("--max-usd", type=float, default=2.0)
    r.add_argument("--turns", type=int, default=1, help="prompts per session (2 = localisation + follow-up)")
    p = sub.add_parser("report")
    p.add_argument("--out", required=True)
    args = ap.parse_args()
    {"tasks": make_tasks, "run": run, "report": report}[args.cmd](args)


if __name__ == "__main__":
    sys.exit(main())
