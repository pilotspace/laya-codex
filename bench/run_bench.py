"""Paired Claude Code benchmark: baseline vs laya-codex.

Measures, per run: codebase-reading tokens (Read/Grep/Glob results + laya-injected context),
total input tokens, cost, wall-clock, turns, and answer accuracy (gold files from git history).

    python3 bench/run_bench.py tasks --repo <clone> --skip 40 --n 20 --out bench/tasks.jsonl
    python3 bench/run_bench.py run --repo <clone> --tasks bench/tasks.jsonl --arms baseline,laya \
        --out bench/results/<name> [--model sonnet] [--limit N] [--effort medium] [--rerun-unhealthy]
    python3 bench/run_bench.py report --out bench/results/<name>

Arms are `name[:template][@binary]` (bench/runs.py): `baseline`, `laya-adaptive`, or e.g.
`v030:laya-adaptive@/opt/v030/laya-codex` to compare two builds with the same hooks. An arm with its
own binary gets its own LAYA_CODEX_HOME and Moon port under <out>/homes/, so its daemon serves only it.
`--repeat N` runs every task N times per arm; the stats average the repeats of a task before pairing.

`--effort` (default "medium") is set as `CLAUDE_EFFORT` explicitly for every session, so a run is
pinned to the value on the command line rather than whatever the parent shell happened to export.
Each row also records `claude_version`, and `laya_version` (the arm's `laya-codex --version`, None
for baseline) so a run can be told apart from a different binary or CLI build after the fact.

Each row of a laya arm records `injection_ok`: whether every prompt of the session got a clean
laya-codex injection (see `injection_health` / `DAEMON_FAILURE_ACTIONS`). `--rerun-unhealthy` treats
rows with `injection_ok: false` as not done, so the next `run` retries just those sessions; the old,
unhealthy row is left in runs.jsonl (bench/runs.py's `load_runs` prefers the healthy rerun over it).
"""
import argparse
import json
import os
import random
import re
import subprocess
import sys
import time

from runs import load_runs, parse_arm, read_hook_log

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


MOON_PORT_BASE = 16500

# Skip actions crates/laya-cli/src/hook.rs returns only when the daemon call itself failed or was
# unreachable -- "query_failed" from the UserPromptSubmit handler (`user_prompt`), "daemon_unavailable"
# from the Read/Agent/compact hooks that also call the daemon. Distinct from ordinary skip reasons
# (e.g. "already_in_context", "skip_prompt", "no_spans") which are not injection failures.
DAEMON_FAILURE_ACTIONS = {"query_failed", "daemon_unavailable"}


def injection_health(prompt_actions, n_prompts):
    """False if a UserPromptSubmit hook entry hit a daemon failure (Moon disk guard etc, v8's
    `query_failed` incident) or fewer UserPromptSubmit entries were logged than prompts were sent
    (the session died, or hooks were not wired up, before a later prompt's hook could run)."""
    if any(a in DAEMON_FAILURE_ACTIONS for a in prompt_actions):
        return False
    return len(prompt_actions) >= n_prompts


def default_bin():
    return os.environ.get("LAYA_CODEX_BIN") or os.path.abspath(os.path.join(HERE, "..", "target", "release", "laya-codex"))


def _cli_version(cmd):
    """`<cmd> --version`, or None if the binary is missing or times out."""
    try:
        p = subprocess.run([cmd, "--version"], capture_output=True, text=True, timeout=30)
        return p.stdout.strip() or None
    except (OSError, subprocess.TimeoutExpired):
        return None


def arm_versions(specs, version_of=_cli_version):
    """{arm name: laya-codex --version}, None for baseline. Run once per distinct resolved binary
    path (cached), since several arms commonly share the default binary."""
    cache, versions = {}, {}
    for name, template, binary in specs:
        if template is None:
            versions[name] = None
            continue
        path = os.path.abspath(binary or default_bin())
        if path not in cache:
            cache[path] = version_of(path)
        versions[name] = cache[path]
    return versions


def done_set(res_path, rerun_unhealthy):
    """((arm, task_id, rep) already run, $ already spent) from an existing runs.jsonl. With
    `--rerun-unhealthy`, a row whose injection failed (`injection_ok: false`) does not count as
    done -- the plan reruns it -- but its cost still counts toward `--max-total-usd`, since it was
    already spent. The old row is left in place; runs.py's `load_runs` prefers the healthy rerun."""
    done, spent = set(), 0.0
    if not os.path.exists(res_path):
        return done, spent
    for r in map(json.loads, open(res_path)):
        spent += r.get("cost_usd") or 0
        if rerun_unhealthy and r.get("injection_ok") is False:
            continue
        done.add((r["arm"], r["task_id"], r.get("rep", 0)))
    return done, spent


def render_configs(out_dir, specs):
    """Materialize each arm's bench/config/laya-{settings,mcp}.<template>.json as
    <out>/config/laya-{settings,mcp}.<arm>.json with the arm's absolute laya-codex binary path."""
    dst = os.path.join(out_dir, "config")
    os.makedirs(dst, exist_ok=True)
    for name, template, binary in specs:
        if template is None:
            continue
        laya_bin = os.path.abspath(binary or default_bin())
        if not os.path.exists(laya_bin):
            sys.exit("laya-codex binary not found at %s (build with cargo build --release -p laya-cli or set LAYA_CODEX_BIN)" % laya_bin)
        if not os.path.exists(os.path.join(HERE, "config", "laya-settings.%s.json" % template)):
            sys.exit("arm %s: no bench/config/laya-settings.%s.json" % (name, template))
        for kind in ("settings", "mcp"):
            src = os.path.join(HERE, "config", "laya-%s.%s.json" % (kind, template))
            if os.path.exists(src):
                text = open(src).read().replace("@LAYA_CODEX_BIN@", laya_bin)
                open(os.path.join(dst, "laya-%s.%s.json" % (kind, name)), "w").write(text)
    return dst


def arm_env(out_dir, specs):
    """Extra environment per arm: an arm with its own binary gets its own home and Moon port."""
    env = {}
    for i, (name, _, binary) in enumerate(specs):
        env[name] = {}
        if binary:
            env[name] = {"LAYA_CODEX_HOME": os.path.join(os.path.abspath(out_dir), "homes", name),
                         "LAYA_CODEX_MOON_PORT": str(MOON_PORT_BASE + i)}
    return env


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


def run_name(task_id, arm, rep):
    """File stem of a session's raw transcript and hook log; repeat 0 keeps the pre-repeat name."""
    return "%s_%s" % (task_id, arm) + ("_r%d" % rep if rep else "")


def run_one(arm, task, args, cfg_dir, rep=0, extra_env=None, versions=None, claude_version=None):
    """One task = one Claude session of `args.turns` prompts (turn 2+ resume the same session)."""
    import uuid
    hook_log = os.path.join(args.out, "hooklogs", run_name(task["id"], arm, rep) + ".jsonl")
    os.makedirs(os.path.dirname(hook_log), exist_ok=True)
    if os.path.exists(hook_log):
        os.remove(hook_log)
    # LAYA_CODEX_MEMO=0: score every prompt cold, as a new prompt is in real use; otherwise whichever arm
    # runs a task first pays the model run and the others hit its cache.
    # CLAUDE_EFFORT is set explicitly (not just inherited) so every arm is pinned to --effort rather
    # than whatever the parent shell happens to export (v8 silently ran the whole benchmark at medium).
    env = dict(os.environ, LAYA_CODEX_HOOK_LOG=hook_log, LAYA_CODEX_MEMO=os.environ.get("LAYA_CODEX_MEMO", "0"),
               CLAUDE_EFFORT=args.effort, **(extra_env or {}))
    prompts = [PROMPT.format(task=task["task"])] + [FOLLOWUP] * (args.turns - 1)
    sid = str(uuid.uuid4())
    all_lines, wall, rcs = [], 0.0, []
    agg = {"reading_tokens": 0, "total_in": 0, "output": 0, "cost": 0.0, "turns": 0, "tool_calls": {}}
    answers = []
    prompt_output_tokens, prompt_answer_chars, prompt_turns, prompt_wall_s = [], [], [], []
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
        # Per-prompt breakdown (a timeout truncates the stream, so a later prompt's parse_stream sees
        # no "result" event: output tokens 0, turns 0, empty answer -- consistent with the aggregates).
        prompt_output_tokens.append(u.get("output_tokens") or 0)
        prompt_answer_chars.append(len(r["result"] or ""))
        prompt_turns.append(r["num_turns"] or 0)
        prompt_wall_s.append(round(w, 2))
    h = read_hook_log(hook_log)
    injection_ok = injection_health(h["prompt_actions"], len(prompts)) if arm != "baseline" else None
    if injection_ok is False:
        print("WARNING: unhealthy injection for %s (prompt_actions=%s, %d/%d prompts logged)" % (
            run_name(task["id"], arm, rep), h["prompt_actions"], len(h["prompt_actions"]), len(prompts)), flush=True)
    row = {"arm": arm, "task_id": task["id"], "rep": rep, "model": args.model, "effort": args.effort,
           "laya_version": (versions or {}).get(arm), "claude_version": claude_version,
           "wall_s": round(wall, 2), "rc": rcs,
           "total_input_tokens": agg["total_in"], "output_tokens": agg["output"], "reading_tokens": agg["reading_tokens"],
           "injected_tokens": h["injected_tokens"], "cost_usd": round(agg["cost"], 6), "num_turns": agg["turns"],
           "tool_calls": agg["tool_calls"], "hook_actions": h["hook_actions"], "prompts": len(prompts),
           # How each prompt was ranked (laya / laya-partial / lexical; None from builds that do not log it).
           "rank_modes": h["rank_modes"], "scored": h["scored"], "offered": h["offered"],
           "prompt_injected_tokens": h["prompt_injected_tokens"], "injection_ok": injection_ok,
           "prompt_output_tokens": prompt_output_tokens, "prompt_answer_chars": prompt_answer_chars,
           "prompt_turns": prompt_turns, "prompt_wall_s": prompt_wall_s}
    row.update(grade(answers[0], task["gold"]))
    if len(answers) > 1:
        named = [n for a in answers for n in grade(a, task["gold"])["named"]]
        row["recall_all_turns"] = len({g for g in task["gold"]
                                       if any(n == g or g.endswith("/" + n) or n.endswith(g) for n in named)}) / len(task["gold"])
        row["turn2"] = grade(answers[1], task["gold"])
    return row, all_lines


def make_plan(tasks, arms, repeat, seed=7):
    """(arm, task, rep) in run order: each repeat of a task runs every arm, in a random order per
    task and repeat, so machine and API drift hit all arms alike. Repeat 0 is the pre-repeat plan."""
    rng = random.Random(seed)
    return [(a, t, rep) for rep in range(repeat) for t in tasks for a in rng.sample(arms, len(arms))]


def run(args):
    tasks = [json.loads(l) for l in open(args.tasks)][: args.limit or None]
    specs = [parse_arm(a) for a in args.arms.split(",")]
    arms = [name for name, _, _ in specs]
    if len(set(arms)) != len(arms):
        sys.exit("duplicate arm names in --arms")
    os.makedirs(os.path.join(args.out, "raw"), exist_ok=True)
    res_path = os.path.join(args.out, "runs.jsonl")
    done, spent = done_set(res_path, args.rerun_unhealthy)
    cfg_dir = render_configs(args.out, specs)
    envs = arm_env(args.out, specs)
    versions = arm_versions(specs)
    claude_version = _cli_version("claude")
    # Restart each daemon so the first hook starts it with this run's environment (LAYA_CODEX_MEMO etc.).
    for name, template, binary in specs:
        if template is not None:
            subprocess.run([binary or default_bin(), "stop"], capture_output=True, env=dict(os.environ, **envs[name]))
    plan = make_plan(tasks, arms, args.repeat)
    for i, (arm, task, rep) in enumerate(plan):
        if (arm, task["id"], rep) in done:
            continue
        if args.max_total_usd and spent >= args.max_total_usd:
            print("stopping: spent $%.2f of the $%.2f cap (--max-total-usd); rerun to resume" % (spent, args.max_total_usd))
            break
        row, lines = run_one(arm, task, args, cfg_dir, rep, envs[arm], versions, claude_version)
        spent += row["cost_usd"] or 0
        with open(os.path.join(args.out, "raw", run_name(task["id"], arm, rep) + ".jsonl"), "w") as f:
            f.write("\n".join(lines))
        with open(res_path, "a") as f:
            f.write(json.dumps(row) + "\n")
        print("[%d/%d] %-10s %s r%d wall=%5.1fs read=%6d inj=%5d total_in=%7d recall=%.2f cost=%s rank=%s" % (
            i + 1, len(plan), arm, task["id"], rep, row["wall_s"], row["reading_tokens"], row["injected_tokens"],
            row["total_input_tokens"], row["recall"], row["cost_usd"], ",".join(str(m) for m in row["rank_modes"])),
            flush=True)
    report(args)


def report(args):
    by = load_runs(os.path.join(args.out, "runs.jsonl"))
    arms = sorted(by, key=lambda a: (a != "baseline", a))
    rows = [r for a in arms for r in by[a].values()]
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
    r.add_argument("--repeat", type=int, default=1, help="sessions per task and arm; the stats average them")
    r.add_argument("--max-total-usd", type=float, default=0.0,
                   help="stop starting sessions once the run (with resumed rows) has spent this much; 0 = no cap")
    r.add_argument("--effort", default="medium", help="CLAUDE_EFFORT, set explicitly for every session (not inherited)")
    r.add_argument("--rerun-unhealthy", action="store_true",
                   help="rows whose laya-codex injection failed (injection_ok: false) do not count as done")
    p = sub.add_parser("report")
    p.add_argument("--out", required=True)
    args = ap.parse_args()
    {"tasks": make_tasks, "run": run, "report": report}[args.cmd](args)


if __name__ == "__main__":
    sys.exit(main())
