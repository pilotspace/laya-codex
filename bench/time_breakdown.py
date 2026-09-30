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
The raw transcripts are not committed; the committed JSON is what this writes.
"""
import argparse
import json
import os
import statistics as st
import sys
from collections import defaultdict
from datetime import datetime

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "scripts"))
import charts  # noqa: E402

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
            out.append({"duration_s": dur, "answer_s": answer, "tools_s": cur["tools_s"], "hook_s": hook,
                        "lookups_s": dur - answer - cur["tools_s"] - hook, "lookup_calls": max(len(calls) - 1, 0)})
            cur = None
    return out


def session(ps, wall_s):
    s = {k: sum(p[k] for p in ps) for k in ("answer_s", "lookups_s", "hook_s", "tools_s", "lookup_calls")}
    s["overhead_s"] = wall_s - sum(p["duration_s"] for p in ps)
    s["wall_s"] = wall_s
    return s


def _jsonl(path):
    with open(path) as f:
        return [json.loads(line) for line in f if line.strip()]


def _hooks(path):
    if not os.path.exists(path):
        return []
    return [h["elapsed_ms"] / 1000 for h in _jsonl(path) if h.get("event") == "UserPromptSubmit"]


KEYS = PARTS + ["wall_s", "lookup_calls"]


def _means(rows):
    return {k: round(st.mean(r[k] for r in rows), 3) for k in KEYS}


def summarize(root, repos):
    by_arm, by_repo, by_prompt = defaultdict(list), defaultdict(lambda: defaultdict(list)), defaultdict(list)
    tasks = set()
    for repo in repos:
        d = os.path.join(root, repo)
        for r in _jsonl(os.path.join(d, "runs.jsonl")):
            name = f"{r['task_id']}_{r['arm']}.jsonl"
            ps = prompts(_jsonl(os.path.join(d, "raw", name)), _hooks(os.path.join(d, "hooklogs", name)))
            s = session(ps, r["wall_s"])
            by_arm[r["arm"]].append(s)
            by_repo[repo][r["arm"]].append(s)
            for i, p in enumerate(ps):
                by_prompt[(r["arm"], i + 1)].append(p)
            tasks.add((repo, r["task_id"]))
    return {
        "n_tasks": len(tasks),
        "arms": {a: _means(v) for a, v in by_arm.items()},
        "per_repo": {repo: {a: _means(v) for a, v in arms.items()} for repo, arms in by_repo.items()},
        "per_prompt": {f"{a} prompt {i}": {k: round(st.mean(p[k] for p in v), 3)
                                           for k in ("answer_s", "lookups_s", "hook_s", "tools_s", "lookup_calls")}
                       for (a, i), v in sorted(by_prompt.items())},
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
    if a.svg_out:
        for name, t in charts.THEMES.items():
            path = os.path.join(a.svg_out, f"insight-time-breakdown-{name}.svg")
            with open(path, "w") as f:
                f.write(chart(summary, t))
            print(path)


if __name__ == "__main__":
    main()
