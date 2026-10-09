"""Where a benchmark session's wall-clock goes: answers, lookups, the hook, tools and overhead.

    python3 bench/time_breakdown.py --root <run root with <repo>/{runs.jsonl,raw,hooklogs}> \
        --out bench/results/time-v13.json [--svg-out docs/assets] [--label "benchmark v13"]

Per prompt, Claude Code's own `duration_ms` (result event) is split with the raw stream-json
timestamps and the laya-codex hook log:
- answer: the prompt's last model call, from the event before it to its last content block;
- tools: tool_use -> tool_result, for every tool call;
- hook: the UserPromptSubmit hook's elapsed time (laya-codex only);
- lookups: the rest, i.e. the model calls that ended in a tool call.
Per session, overhead is the harness wall-clock outside those prompts (process start, init).

Lookup time (`lookup_s`) is what finding the code costs a session: the hook plus the lookup round
trips plus tool execution; `lookup_calls` counts those round trips. Every arm is paired with the
baseline (or the first arm) over the tasks both ran, with paired, repo-stratified bootstrap 95%
CIs for the mean difference and for the change of the ratio of sums (`paired` in the output).

Claude Code's result event also reports `ttft_stream_ms`. It is timed from the prompt's
submission, so it includes the UserPromptSubmit hook: it is not the API's time to first token.

Hook-log lines that do not parse (v14 has one spliced by two concurrent appends) are skipped and
counted in `unparsable_hook_lines`. A repeat (`rep` > 0) reads its own `_r<rep>` transcript.
The raw transcripts are not committed; the committed JSON is what this writes.
"""
import argparse
import json
import os
import random
import statistics as st
import sys
from collections import defaultdict
from datetime import datetime

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "scripts"))
import charts  # noqa: E402
from run_bench import run_name  # noqa: E402
from stats_pooled import ci, mean_diff, ratio  # noqa: E402

PARTS = ["answer_s", "lookups_s", "hook_s", "tools_s", "overhead_s"]


def _ts(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp()


def prompts(lines, hook_s):
    """Split each prompt of one stream-json session. `hook_s`: hook seconds per prompt, in order."""
    out, cur = [], None
    for d in lines:
        t = d.get("type")
        if t == "system" and d.get("subtype") == "init":
            cur = {"calls": {}, "order": [], "last": None, "pending": {}, "tools_s": 0.0}
        elif cur is None:
            continue
        elif t == "assistant":
            now = _ts(d["timestamp"])
            rid = d.get("request_id") or d["message"].get("id")
            if rid not in cur["calls"]:
                cur["calls"][rid] = {"start": cur["last"], "end": now}
                cur["order"].append(rid)
            cur["calls"][rid]["end"] = now
            for b in d["message"].get("content") or []:
                if b.get("type") == "tool_use":
                    cur["pending"][b["id"]] = now
            cur["last"] = now
        elif t == "user":
            now = _ts(d["timestamp"])
            content = d["message"].get("content")
            for b in content if isinstance(content, list) else []:
                if b.get("type") == "tool_result" and b.get("tool_use_id") in cur["pending"]:
                    cur["tools_s"] += now - cur["pending"].pop(b["tool_use_id"])
            cur["last"] = now
        elif t == "result":
            i = len(out)
            dur = d["duration_ms"] / 1000
            hook = hook_s[i] if i < len(hook_s) else 0.0
            calls = [cur["calls"][r] for r in cur["order"]]
            last = calls[-1] if calls else None
            if last and last["start"] is not None and len(calls) > 1:
                answer = last["end"] - last["start"]
            else:
                answer = dur - cur["tools_s"] - hook
            lookups = dur - answer - cur["tools_s"] - hook
            out.append({"duration_s": dur, "answer_s": answer, "tools_s": cur["tools_s"], "hook_s": hook,
                        "lookups_s": lookups, "lookup_calls": max(len(calls) - 1, 0),
                        "lookup_s": hook + lookups + cur["tools_s"]})
            cur = None
    return out


def session(ps, wall_s):
    s = {k: sum(p[k] for p in ps) for k in PROMPT_KEYS}
    s["overhead_s"] = wall_s - sum(p["duration_s"] for p in ps)
    s["wall_s"] = wall_s
    return s


def _jsonl(path):
    with open(path) as f:
        return [json.loads(line) for line in f if line.strip()]


def _hooks(path):
    """(UserPromptSubmit hook seconds in order, number of lines that did not parse)."""
    if not os.path.exists(path):
        return [], 0
    times, bad = [], 0
    with open(path) as f:
        for line in f:
            if not line.strip():
                continue
            try:
                h = json.loads(line)
            except ValueError:
                bad += 1
                continue
            if isinstance(h, dict) and h.get("event") == "UserPromptSubmit":
                times.append(h["elapsed_ms"] / 1000)
    return times, bad


PROMPT_KEYS = ["answer_s", "lookups_s", "hook_s", "tools_s", "lookup_calls", "lookup_s"]
KEYS = PARTS + ["wall_s", "lookup_calls", "lookup_s"]
LOOKUP = ["lookup_s", "lookup_calls"]
BOOT = 10000


def _means(rows):
    return {k: round(st.mean(r[k] for r in rows), 3) for k in KEYS}


def paired(sessions, arm, base, boot=BOOT, seed=0):
    """Lookup time and calls of `arm` vs `base` over the tasks both ran. `sessions` is
    {(repo, task): {arm: [session, ...]}}; a task's repeats are averaged first, so tasks stay the
    unit. Per metric: both means, the mean difference and the change of the ratio of sums, each
    with a 95% CI from a paired bootstrap that resamples tasks within each repo (one set of
    resamples for both metrics, as bench/stats_pooled.py)."""
    def avg(rows):
        return {k: st.mean(r[k] for r in rows) for k in LOOKUP}

    by_repo = defaultdict(list)
    for (repo, _), arms in sorted(sessions.items()):
        if arms.get(arm) and arms.get(base):
            by_repo[repo].append((avg(arms[arm]), avg(arms[base])))
    pairs = [p for ps in by_repo.values() for p in ps]
    out = {"n_tasks": len(pairs)}
    if not pairs:
        return out
    rng = random.Random(seed)
    samples = [[ps[rng.randrange(len(ps))] for ps in by_repo.values() for _ in ps] for _ in range(boot)]
    for k in LOOKUP:
        def f(r, k=k):
            return r[k]
        out[k] = {base: round(st.mean(b[k] for _, b in pairs), 4), arm: round(st.mean(t[k] for t, _ in pairs), 4),
                  "diff": round(mean_diff(pairs, f), 4),
                  "diff_ci": [round(x, 4) for x in ci([mean_diff(p, f) for p in samples])],
                  "change": round(ratio(pairs, f), 4),
                  "change_ci": [round(x, 4) for x in ci([ratio(p, f) for p in samples])]}
    return out


def summarize(root, repos):
    by_arm, by_repo, by_prompt = defaultdict(list), defaultdict(lambda: defaultdict(list)), defaultdict(list)
    by_task = defaultdict(lambda: defaultdict(list))
    tasks, bad_lines = set(), 0
    for repo in repos:
        d = os.path.join(root, repo)
        # A session run again (e.g. --rerun-unhealthy) left one row per attempt; its files on
        # disk are the last attempt's, so the last row counts.
        rows = {(r["arm"], r["task_id"], r.get("rep", 0)): r for r in _jsonl(os.path.join(d, "runs.jsonl"))}
        for (arm, task, rep), r in rows.items():
            name = run_name(task, arm, rep) + ".jsonl"
            hooks, bad = _hooks(os.path.join(d, "hooklogs", name))
            bad_lines += bad
            ps = prompts(_jsonl(os.path.join(d, "raw", name)), hooks)
            s = session(ps, r["wall_s"])
            by_arm[arm].append(s)
            by_repo[repo][arm].append(s)
            by_task[(repo, task)][arm].append(s)
            for i, p in enumerate(ps):
                by_prompt[(arm, i + 1)].append(p)
            tasks.add((repo, task))
    ref = "baseline" if "baseline" in by_arm else min(by_arm, default=None)
    return {
        "n_tasks": len(tasks),
        "unparsable_hook_lines": bad_lines,
        "arms": {a: _means(v) for a, v in by_arm.items()},
        "per_repo": {repo: {a: _means(v) for a, v in arms.items()} for repo, arms in by_repo.items()},
        "per_prompt": {f"{a} prompt {i}": {k: round(st.mean(p[k] for p in v), 3) for k in PROMPT_KEYS}
                       for (a, i), v in sorted(by_prompt.items())},
        "paired": {f"{a} vs {ref}": paired(by_task, a, ref) for a in sorted(by_arm) if a != ref},
    }


SEGMENTS = [("answer_s", "Final answers"), ("lookups_s", "Lookup turns"), ("hook_s", "Ranking hook"),
            ("tools_s", "Tools"), ("overhead_s", "Claude Code overhead")]


def chart(summary, t):
    """Stacked bars per arm; each bar is labelled with its measured wall-clock, not the sum of parts."""
    colors = {"answer_s": t["fg"], "lookups_s": t["base"], "hook_s": "#bf8700", "tools_s": t["grid"],
              "overhead_s": t["muted"]}
    arms = [("baseline", "stock Claude Code"), ("laya", "with laya-codex")]
    w, h = 760, 300
    x0, x1 = 176, 690
    top = max(summary["arms"][a]["wall_s"] for a, _ in arms)
    span = 4 * (int(top / 4) + 1)
    px = (x1 - x0) / span
    body = [
        charts.text(24, 32, "Where a session's time goes", 17, t["fg"], weight="600"),
        charts.text(24, 54, f"Seconds per session · {summary.get('label', 'benchmark')} · "
                            f"{summary['n_tasks']} paired tasks", 12, t["muted"]),
    ]
    for s in range(0, span + 1, 4):
        x = x0 + s * px
        body.append(f'<line x1="{x:.1f}" y1="78" x2="{x:.1f}" y2="206" stroke="{t["grid"]}" stroke-width="1"/>')
        body.append(charts.text(x, 222, f"{s} s", 11, t["muted"], "middle"))
    for r, (arm, label) in enumerate(arms):
        m = summary["arms"][arm]
        y = 96 + r * 58
        body.append(charts.text(x0 - 12, y + 16, label, 14, t["fg"], "end", "600"))
        body.append(charts.text(x0 - 12, y + 33, f"{m['lookup_calls']:.2f} lookup calls", 11, t["muted"], "end"))
        x = x0
        for key, _ in SEGMENTS:
            v = max(m[key], 0.0)
            if not v:
                continue
            color = t["laya"] if (key == "lookups_s" and arm == "laya") else colors[key]
            body.append(f'<rect x="{x:.1f}" y="{y:.1f}" width="{v * px:.1f}" height="28" fill="{color}"/>')
            if v * px > 44:
                body.append(charts.text(x + v * px / 2, y + 19, f"{v:.1f} s", 12, t["bg"], "middle", "600"))
            x += v * px
        body.append(charts.text(x + 8, y + 19, f"{m['wall_s']:.1f} s", 13, t["fg"], "start", "600"))
    lx = 24
    for key, name in SEGMENTS:
        body.append(f'<rect x="{lx:.1f}" y="250" width="12" height="12" rx="2" fill="{colors[key]}"/>')
        body.append(charts.text(lx + 18, 260, name, 12, t["fg"]))
        lx += 26 + len(name) * 6.8
    return charts.svg(w, h, body, t, "Where a benchmark session's time goes, stock Claude Code vs laya-codex")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--root", required=True)
    ap.add_argument("--repos", default="moon,httpx,hono")
    ap.add_argument("--out", required=True)
    ap.add_argument("--svg-out")
    ap.add_argument("--label", default="benchmark")
    a = ap.parse_args()
    summary = summarize(a.root, a.repos.split(","))
    summary["label"] = a.label
    with open(a.out, "w") as f:
        json.dump(summary, f, indent=1)
        f.write("\n")
    print(json.dumps(summary["arms"], indent=1))
    if summary["unparsable_hook_lines"]:
        print("skipped %d unparsable hook-log lines" % summary["unparsable_hook_lines"])
    for name, p in summary["paired"].items():
        for k in LOOKUP:
            if k in p:
                m = p[k]
                print("%s, %s: diff %+.3f [%+.3f, %+.3f], change %+.1f%% [%+.1f, %+.1f] (n=%d tasks)" % (
                    name, k, m["diff"], *m["diff_ci"], 100 * m["change"], *[100 * x for x in m["change_ci"]],
                    p["n_tasks"]))
    if a.svg_out:
        for name, t in charts.THEMES.items():
            path = os.path.join(a.svg_out, f"insight-time-breakdown-{name}.svg")
            with open(path, "w") as f:
                f.write(chart(summary, t))
            print(path)


if __name__ == "__main__":
    main()
