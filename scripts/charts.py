#!/usr/bin/env python3
"""Render the README benchmark charts as dependency-free SVGs (light and dark variants).

    python3 scripts/charts.py bench/results/headline-v7.json docs/assets

Writes <out>/benchmark-savings-{light,dark}.svg (paired % change vs stock Claude Code with the
95% bootstrap CI) and <out>/benchmark-reads-{light,dark}.svg (read behaviour, stock vs laya).
The input is the headline JSON that the benchmark writes (see bench/results/headline-v7.json).
"""
import json
import os
import sys
from xml.sax.saxutils import escape

THEMES = {
    "light": {"bg": "#ffffff", "fg": "#1f2328", "muted": "#59636e", "grid": "#d1d9e0",
              "laya": "#2da44e", "base": "#afb8c1", "ci": "#1f2328"},
    "dark": {"bg": "#0d1117", "fg": "#e6edf3", "muted": "#9198a1", "grid": "#3d444d",
             "laya": "#3fb950", "base": "#6e7681", "ci": "#e6edf3"},
}
FONT = "-apple-system,BlinkMacSystemFont,'Segoe UI',Helvetica,Arial,sans-serif"


def text(x, y, s, size=13, color="#000", anchor="start", weight="normal"):
    return (f'<text x="{x:.1f}" y="{y:.1f}" font-family="{FONT}" font-size="{size}" fill="{color}" '
            f'text-anchor="{anchor}" font-weight="{weight}">{escape(s)}</text>')


def svg(w, h, body, t, title):
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" '
            f'role="img" aria-label="{escape(title)}"><title>{escape(title)}</title>'
            f'<rect width="{w}" height="{h}" rx="8" fill="{t["bg"]}"/>{"".join(body)}</svg>\n')


def savings(d, t):
    """Horizontal bars: how much less each metric costs with laya, with the 95% CI."""
    rows = list(d["metrics"].values())
    w, left, right, top, row_h = 760, 200, 60, 74, 44
    h = top + row_h * len(rows) + 46
    span = max(abs(r["lo"]) for r in rows)
    scale = (w - left - right) / (span * 1.08)
    body = [
        text(24, 32, f"With laya, Claude Code spends less on every measure", 17, t["fg"], weight="600"),
        text(24, 54, f"Paired change vs stock Claude Code · {d['n_tasks']} tasks · {', '.join(d['repos'])} · "
                     f"{d['model']} · 95% bootstrap CI", 12, t["muted"]),
    ]
    for tick in range(0, int(span * 1.08) + 1, 10):
        x = left + tick * scale
        body.append(f'<line x1="{x:.1f}" y1="{top - 8}" x2="{x:.1f}" y2="{h - 40}" stroke="{t["grid"]}" stroke-width="1"/>')
        body.append(text(x, h - 22, f"−{tick}%" if tick else "0", 11, t["muted"], "middle"))
    for i, r in enumerate(rows):
        y = top + i * row_h
        cy = y + row_h / 2 - 4
        body.append(text(left - 14, cy + 5, r["label"], 14, t["fg"], "end"))
        bw = abs(r["pct"]) * scale
        body.append(f'<rect x="{left}" y="{cy - 12:.1f}" width="{bw:.1f}" height="24" rx="4" fill="{t["laya"]}"/>')
        x1, x2 = left + abs(r["hi"]) * scale, left + abs(r["lo"]) * scale
        body.append(f'<line x1="{x1:.1f}" y1="{cy:.1f}" x2="{x2:.1f}" y2="{cy:.1f}" stroke="{t["ci"]}" stroke-width="1.5"/>')
        for x in (x1, x2):
            body.append(f'<line x1="{x:.1f}" y1="{cy - 6:.1f}" x2="{x:.1f}" y2="{cy + 6:.1f}" stroke="{t["ci"]}" stroke-width="1.5"/>')
        body.append(text(x2 + 8, cy + 5, f"{r['pct']:+.1f}%".replace("-", "−"), 14, t["fg"], weight="600"))
    return svg(w, h, body, t, "laya-codex benchmark: savings vs stock Claude Code")


def reads(d, t):
    """Small multiples: stock Claude Code vs laya on read behaviour."""
    items = list(d["reads"].values())
    w, h = 760, 272
    pw = (w - 48) / len(items)
    body = [
        text(24, 32, "Claude reads the right code sooner", 17, t["fg"], weight="600"),
        text(24, 54, "Read behaviour per task, stock Claude Code vs with laya", 12, t["muted"]),
    ]
    base_y, bar_max = 226, 110
    for i, it in enumerate(items):
        x0 = 24 + i * pw
        body.append(text(x0 + pw / 2, 86, it["label"], 13, t["fg"], "middle", "600"))
        hint = "higher is better" if it["better"] == "higher" else "lower is better"
        body.append(text(x0 + pw / 2, 104, hint, 11, t["muted"], "middle"))
        top = max(it["baseline"], it["laya"])
        for j, (key, color, name) in enumerate((("baseline", t["base"], "stock"), ("laya", t["laya"], "laya"))):
            v = it[key]
            bh = bar_max * 0.72 * v / top
            bx = x0 + pw / 2 - 46 + j * 50
            body.append(f'<rect x="{bx:.1f}" y="{base_y - bh:.1f}" width="42" height="{bh:.1f}" rx="4" fill="{color}"/>')
            body.append(text(bx + 21, base_y - bh - 6, it["fmt"].format(v), 12, t["fg"], "middle", "600"))
            body.append(text(bx + 21, base_y + 18, name, 11, t["muted"], "middle"))
    return svg(w, h, body, t, "laya-codex benchmark: read behaviour")


def median(xs):
    s = sorted(xs)
    n = len(s)
    return s[n // 2] if n % 2 else (s[n // 2 - 1] + s[n // 2]) / 2


def journey(d, t):
    """Step lines: share of tasks with a correct file in Claude's context by each turn."""
    j = d["journey"]
    n = len(j["baseline"])
    w, h = 760, 360
    left, right, top, bottom = 64, 24, 92, 300
    max_turn = 20
    sx = lambda turn: left + turn * (w - left - right) / max_turn
    sy = lambda frac: bottom - frac * (bottom - top)
    body = [
        text(24, 32, "The journey to the right code", 17, t["fg"], weight="600"),
        text(24, 54, f"Share of the {n} tasks where a correct file is in Claude's context, by turn", 12, t["muted"]),
    ]
    for pct in (0, 25, 50, 75, 100):
        y = sy(pct / 100)
        body.append(f'<line x1="{left}" y1="{y:.1f}" x2="{w - right}" y2="{y:.1f}" stroke="{t["grid"]}" stroke-width="1"/>')
        body.append(text(left - 8, y + 4, f"{pct}%", 11, t["muted"], "end"))
    for turn in range(0, max_turn + 1, 2):
        body.append(text(sx(turn), bottom + 18, str(turn), 11, t["muted"], "middle"))
    body.append(text((left + w - right) / 2, bottom + 38, "Claude's turn (0 = before its first turn)", 12, t["muted"], "middle"))

    def step(turns, color, dash="", width=3):
        pts = []
        for turn in range(0, max_turn + 1):
            frac = sum(1 for x in turns if x is not None and x <= turn) / n
            if pts:
                pts.append((sx(turn), pts[-1][1]))
            pts.append((sx(turn), sy(frac)))
        path = " ".join(f"{x:.1f},{y:.1f}" for x, y in pts)
        d_attr = f' stroke-dasharray="{dash}"' if dash else ""
        return (f'<polyline points="{path}" fill="none" stroke="{color}" stroke-width="{width}" '
                f'stroke-linejoin="round"{d_attr}/>')

    body.append(step(j["baseline"], t["base"]))
    body.append(step(j["laya_read"], t["laya"], "6 5", 2))
    body.append(step(j["laya_seen"], t["laya"]))

    at0 = sum(1 for x in j["laya_seen"] if x == 0)
    body.append(f'<circle cx="{sx(0):.1f}" cy="{sy(at0 / n):.1f}" r="5" fill="{t["laya"]}"/>')
    cx, cy = sx(8.6), sy(0.52)
    body.append(f'<line x1="{sx(0) + 6:.1f}" y1="{sy(at0 / n) + 4:.1f}" x2="{cx - 4:.1f}" y2="{cy - 4:.1f}" stroke="{t["muted"]}" stroke-width="1"/>')
    body.append(text(cx, cy + 4, f"{at0} of {n} tasks: the right code arrives with the prompt", 12, t["fg"], weight="600"))
    first_b = min(j["baseline"])
    body.append(text(sx(first_b) + 8, sy(0) - 10, f"stock: first correct file at turn {first_b}", 12, t["muted"]))

    lx, ly = sx(8.6), sy(0.36)
    legend = (
        (t["laya"], "", 3, f"laya: correct code in context (median turn {median(j['laya_seen']):g})"),
        (t["laya"], "6 5", 2, f"laya: first correct Read (median {median(j['laya_read']):g})"),
        (t["base"], "", 3, f"stock Claude Code (median {median(j['baseline']):g})"),
    )
    for k, (color, dash, width, label) in enumerate(legend):
        y = ly + k * 20
        d_attr = f' stroke-dasharray="{dash}"' if dash else ""
        body.append(f'<line x1="{lx}" y1="{y}" x2="{lx + 26}" y2="{y}" stroke="{color}" stroke-width="{width}"{d_attr}/>')
        body.append(text(lx + 34, y + 4, label, 12, t["fg"]))
    return svg(w, h, body, t, "laya-codex benchmark: turns until Claude has the right code")


def main():
    src, out = sys.argv[1], sys.argv[2]
    d = json.load(open(src))
    os.makedirs(out, exist_ok=True)
    charts = [("savings", savings), ("reads", reads)]
    if "journey" in d:
        charts.append(("journey", journey))
    for name, t in THEMES.items():
        for chart, fn in charts:
            path = os.path.join(out, f"benchmark-{chart}-{name}.svg")
            open(path, "w").write(fn(d, t))
            print(path)


if __name__ == "__main__":
    main()
