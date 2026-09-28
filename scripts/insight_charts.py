#!/usr/bin/env python3
"""Render the insight charts behind docs/RESULTS.md "At a glance" as dependency-free SVGs (light and dark).

    python3 scripts/insight_charts.py bench/results/claude-v10/nostall docs/assets --arm laya --run-label "benchmark v10" \
        --before bench/results/replay-2026-09-24/ab-rank.jsonl v030 v0.3.0 \
        --after bench/results/replay-2026-09-26-r1/replay.jsonl candidate v0.4.0

Writes <out>/insight-{time,turns,followup}-{light,dark}.svg:
- time: wall-clock against output tokens for every session of the run, with the least-squares line;
- turns: turns per session and output tokens per turn, stock Claude Code vs laya-codex, per repository;
- followup: characters injected into the second prompt of a session, one replay label against another.
Inputs: the run's runs.jsonl files (<run>/<repo>/runs.jsonl) and hook replays (bench/replay_hooks.py output).
"""
import argparse
import json
import math
import os

from charts import THEMES, svg, text

REPOS = ("moon", "httpx", "hono")


def rows_of(path):
    with open(path) as f:
        return [json.loads(line) for line in f if line.strip()]


def fit(xs, ys):
    """Least-squares line y = a + b x and its R²."""
    n = len(xs)
    mx, my = sum(xs) / n, sum(ys) / n
    b = sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / sum((x - mx) ** 2 for x in xs)
    a = my - b * mx
    ss = sum((y - a - b * x) ** 2 for x, y in zip(xs, ys))
    st = sum((y - my) ** 2 for y in ys)
    return a, b, 1 - ss / st


def legend(t, x, y, items):
    """Swatch + label pairs in one row; returns the body parts."""
    out = []
    for color, label in items:
        out.append(f'<rect x="{x}" y="{y - 9}" width="10" height="10" rx="2" fill="{color}"/>')
        out.append(text(x + 16, y, label, 12, t["fg"]))
        x += 26 + 7 * len(label)
    return out


def hgrid(t, x0, x1, y, label, anchor_x):
    return [f'<line x1="{x0:.1f}" y1="{y:.1f}" x2="{x1:.1f}" y2="{y:.1f}" stroke="{t["grid"]}" stroke-width="1"/>',
            text(anchor_x, y + 4, label, 11, t["muted"], "end")]


def paired_rows(run, arm):
    """Per repo, the rows of the tasks both baseline and `arm` ran (every arm's row for those tasks)."""
    out = {}
    for repo in REPOS:
        rows = rows_of(os.path.join(run, repo, "runs.jsonl"))
        paired = {r["task_id"] for r in rows if r["arm"] == arm} & {r["task_id"] for r in rows if r["arm"] == "baseline"}
        out[repo] = [r for r in rows if r["task_id"] in paired]
    return out


def time_chart(run, arm, run_label, t):
    """Scatter: each session's wall-clock against its output tokens. Stock and laya-codex sessions
    fall on the same line, so time is set by how much Claude writes, not by who found the code."""
    rows = [r for rs in paired_rows(run, arm).values() for r in rs]
    xs = [r["output_tokens"] / 1000 for r in rows]
    ys = [r["wall_s"] for r in rows]
    a, b, r2 = fit(xs, ys)
    a_label = f"{a:.1f}".replace("-", "−")
    w, h = 760, 400
    left, right, top, bottom = 64, 28, 96, 336
    xmax, ymax = 2 * math.ceil(max(xs) / 2), 30 * math.ceil(max(ys) / 30)
    sx = lambda v: left + v / xmax * (w - left - right)
    sy = lambda v: bottom - v / ymax * (bottom - top)
    body = [
        text(24, 32, "Session time follows how much Claude writes", 17, t["fg"], weight="600"),
        text(24, 54, f"Wall-clock vs output tokens · {len(rows)} {run_label} sessions (3 repos, 2 prompts each) · "
                     f"line: {a_label} s + {b:.1f} s per 1k tokens, R² {r2:.2f}", 12, t["muted"]),
    ]
    body += legend(t, left, 80, ((t["base"], "stock Claude Code"), (t["laya"], "laya-codex (both arms)")))
    for v in range(0, ymax + 1, 30):
        body += hgrid(t, left, w - right, sy(v), f"{v} s", left - 8)
    for v in range(0, xmax + 1, 2):
        body.append(text(sx(v), bottom + 18, f"{v}k", 11, t["muted"], "middle"))
    body.append(text((left + w - right) / 2, bottom + 40, "output tokens per session", 12, t["muted"], "middle"))
    # Stock first, laya-codex on top; a 2px surface ring keeps overlapping dots legible.
    for r in sorted(rows, key=lambda r: r["arm"] != "baseline"):
        x, y = r["output_tokens"] / 1000, r["wall_s"]
        color = t["base"] if r["arm"] == "baseline" else t["laya"]
        body.append(f'<circle cx="{sx(x):.1f}" cy="{sy(y):.1f}" r="4" fill="{color}" stroke="{t["bg"]}" stroke-width="2"/>')
    x1 = min(xmax, (ymax - a) / b)
    body.append(f'<line x1="{sx(0):.1f}" y1="{sy(a):.1f}" x2="{sx(x1):.1f}" y2="{sy(a + b * x1):.1f}" '
                f'stroke="{t["fg"]}" stroke-width="2" stroke-linecap="round"/>')
    # The annotation sits in the empty upper-left corner, away from the dots.
    body.append(text(sx(xmax * 0.03), sy(ymax * 0.97), f"Each 1,000 output tokens adds about {b:.1f} s,", 13, t["fg"], weight="600"))
    body.append(text(sx(xmax * 0.03), sy(ymax * 0.97) + 18, "whether or not laya-codex found the code", 13, t["fg"], weight="600"))
    return svg(w, h, body, t, f"Session wall-clock against output tokens, {run_label}")


def turns_chart(run, arm, run_label, t):
    """Two small multiples on their own scales: turns per session and output tokens per turn."""
    stats, n = {}, 0
    for repo, rows in paired_rows(run, arm).items():
        n += sum(1 for r in rows if r["arm"] == "baseline")
        for a in ("baseline", arm):
            rs = [r for r in rows if r["arm"] == a]
            turns = sum(r["num_turns"] for r in rs)
            stats[(repo, a)] = (turns / len(rs), sum(r["output_tokens"] for r in rs) / turns)
    w, h = 760, 330
    body = [
        text(24, 32, "Turns per session and output per turn", 17, t["fg"], weight="600"),
        text(24, 54, f"Means per session, stock Claude Code vs laya-codex · {run_label}, {n} paired tasks", 12, t["muted"]),
    ]
    body += legend(t, 24, 80, ((t["base"], "stock Claude Code"), (t["laya"], "laya-codex")))
    panels = (("Turns per session", 0, "{:.1f}", 20), ("Output tokens per turn", 1, "{:.0f}", 500))
    pw = (w - 48) / 2
    base_y, bar_max = 286, 150
    for p, (title, k, fmt, vmax) in enumerate(panels):
        x0 = 24 + p * pw
        body.append(text(x0 + 8, 112, title, 13, t["fg"], weight="600"))
        body.append(f'<line x1="{x0 + 8:.1f}" y1="{base_y}" x2="{x0 + pw - 16:.1f}" y2="{base_y}" stroke="{t["grid"]}" stroke-width="1"/>')
        gw = (pw - 24) / len(REPOS)
        for i, repo in enumerate(REPOS):
            gx = x0 + 8 + i * gw + gw / 2
            for j, a in enumerate(("baseline", arm)):
                v = stats[(repo, a)][k]
                bh = bar_max * v / vmax
                bx = gx - 26 + j * 28  # 24px bars, 4px surface gap
                color = t["base"] if a == "baseline" else t["laya"]
                body.append(f'<path d="M{bx:.1f},{base_y} v{-bh + 4:.1f} q0,-4 4,-4 h16 q4,0 4,4 v{bh - 4:.1f} z" fill="{color}"/>')
                body.append(text(bx + 12, base_y - bh - 6, fmt.format(v), 11, t["fg"], "middle", "600"))
            body.append(text(gx, base_y + 18, repo, 12, t["muted"], "middle"))
    return svg(w, h, body, t, "Turns per session and output tokens per turn, stock vs laya-codex")


def followup_chart(before, after, t):
    """Horizontal bars per repository: characters injected into the second prompt.

    before/after: (replay jsonl, replay label, name shown in the legend)."""
    (before_path, before_label, before_name), (after_path, after_label, after_name) = before, after
    before = [r for r in rows_of(before_path) if r["label"] == before_label and r["turn"] == 2]
    after = [r for r in rows_of(after_path) if r["label"] == after_label and r["turn"] == 2]
    mean = lambda rs, repo: sum(r["chars"] for r in rs if r["repo"] == repo) / sum(1 for r in rs if r["repo"] == repo)
    w, left, right, top, row_h = 760, 96, 150, 96, 64
    h = top + row_h * len(REPOS) + 44
    vmax = 5000
    sx = lambda v: left + v / vmax * (w - left - right)
    body = [
        text(24, 32, "The follow-up prompt now gets answers, not more code", 17, t["fg"], weight="600"),
        text(24, 54, f"Characters added to a session's second prompt (tests and call sites) · mean of "
                     f"{len(after) // len(REPOS)} tasks per repo · offline replay, no Claude", 12, t["muted"]),
    ]
    body += legend(t, left, 80, ((t["base"], f"{before_name}: two more code blocks"),
                                 (t["laya"], f"{after_name}: test pointers, call sites, locations")))
    for v in range(0, vmax + 1, 1000):
        x = sx(v)
        body.append(f'<line x1="{x:.1f}" y1="{top - 2}" x2="{x:.1f}" y2="{h - 38}" stroke="{t["grid"]}" stroke-width="1"/>')
        body.append(text(x, h - 20, f"{v:,}", 11, t["muted"], "middle"))
    for i, repo in enumerate(REPOS):
        y = top + i * row_h + 8
        body.append(text(left - 12, y + 22, repo, 13, t["fg"], "end"))
        b, a = mean(before, repo), mean(after, repo)
        for j, (v, color) in enumerate(((b, t["base"]), (a, t["laya"]))):
            by = y + j * 26  # 24px bars, 2px surface gap
            bw = sx(v) - left
            body.append(f'<path d="M{left},{by} h{bw - 4:.1f} q4,0 4,4 v16 q0,4 -4,4 h{-bw + 4:.1f} z" fill="{color}"/>')
            label = f"{v:,.0f}" + (f"  ({(a - b) / b:+.0%})".replace("-", "−") if j else "")
            body.append(text(left + bw + 8, by + 17, label, 12, t["fg"], weight="600" if j else "normal"))
    return svg(w, h, body, t, f"Characters injected into the second prompt, {before_name} vs {after_name}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("run", help="run dir holding <repo>/runs.jsonl")
    ap.add_argument("out")
    ap.add_argument("--arm", default="laya", help="the laya-codex arm to set against baseline")
    ap.add_argument("--run-label", required=True, help='how the charts name the run, e.g. "benchmark v10"')
    ap.add_argument("--before", nargs=3, required=True, metavar=("REPLAY", "LABEL", "NAME"))
    ap.add_argument("--after", nargs=3, required=True, metavar=("REPLAY", "LABEL", "NAME"))
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    charts = (("time", lambda t: time_chart(a.run, a.arm, a.run_label, t)),
              ("turns", lambda t: turns_chart(a.run, a.arm, a.run_label, t)),
              ("followup", lambda t: followup_chart(a.before, a.after, t)))
    for name, t in THEMES.items():
        for chart, fn in charts:
            path = os.path.join(a.out, f"insight-{chart}-{name}.svg")
            with open(path, "w") as f:
                f.write(fn(t))
            print(path)


if __name__ == "__main__":
    main()
