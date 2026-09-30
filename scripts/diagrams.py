#!/usr/bin/env python3
"""Render the animated README diagrams as dependency-free SVGs (light and dark variants).

    python3 scripts/diagrams.py docs/assets

Writes <out>/diagram-{session,pipeline,rerank}-{light,dark}.svg in the style of charts.py.
Animation is CSS only, because GitHub shows README SVGs through <img>, where scripts never run.
Every element is drawn in its final state and keyframes supply the hidden start, so a viewer with
reduced motion (or no CSS animation) sees the complete picture.
"""
import os
import sys
from xml.sax.saxutils import escape

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from charts import THEMES, text  # noqa: E402

# Means per session from benchmark v13 (docs/RESULTS.md): 60 paired tasks, claude-sonnet-5-5.
SESSION = {
    "stock": {"wall_s": 23.2, "cost": 0.128, "tool_calls": 5.9, "calls": ["Grep", "Grep", "Read", "Grep", "Grep", "Read"],
              "right_file": "right file at turn 4 (median); never in 19 of 60"},
    "laya": {"wall_s": 18.7, "cost": 0.110, "tool_calls": 2.3, "calls": ["search", "Grep"],
             "right_file": "right file before turn 1 in 52 of 60"},
}
CYCLE_S = 12.0      # one loop of the session race
GROW_S = 8.0        # time the longer (stock) bar takes to grow; both bars grow at the same speed
START_S = 0.6


def pct(seconds, cycle):
    return 100 * seconds / cycle


def fade_in(name, at, cycle):
    """Keyframes: hidden until `at` seconds, visible, then fade out just before the loop restarts."""
    a = pct(at, cycle)
    return (f"@keyframes {name}{{0%,{a:.2f}%{{opacity:0}}{min(a + 2, 93):.2f}%,94%{{opacity:1}}98%,100%{{opacity:0}}}}"
            f".{name}{{animation:{name} {cycle}s linear infinite}}")


def grow(name, start, end, cycle):
    """Keyframes: a bar grows left to right between `start` and `end` seconds."""
    s, e = pct(start, cycle), pct(end, cycle)
    return (f"@keyframes {name}{{0%,{s:.2f}%{{transform:scaleX(0);opacity:1}}{e:.2f}%,94%{{transform:scaleX(1);opacity:1}}"
            f"98%,100%{{transform:scaleX(1);opacity:0}}}}"
            f".{name}{{transform-box:fill-box;transform-origin:left center;animation:{name} {cycle}s linear infinite}}")


def doc(w, h, body, css, t, title):
    style = (f"<style>{''.join(css)}"
             "@media (prefers-reduced-motion: reduce){*{animation:none !important}}</style>")
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" '
            f'role="img" aria-label="{escape(title)}"><title>{escape(title)}</title>{style}'
            f'<rect width="{w}" height="{h}" rx="8" fill="{t["bg"]}"/>{"".join(body)}</svg>\n')


def rect(x, y, w, h, fill, rx=4, cls="", extra=""):
    c = f' class="{cls}"' if cls else ""
    return f'<rect{c} x="{x:.1f}" y="{y:.1f}" width="{w:.1f}" height="{h:.1f}" rx="{rx}" fill="{fill}"{extra}/>'


def group(cls, items):
    return f'<g class="{cls}">{"".join(items)}</g>'


# --- 1. the session race ----------------------------------------------------------------------

AXIS_X0, AXIS_X1 = 200, 648


def session_lanes():
    """Geometry and timing of each lane. Bars share one scale in pixels and in seconds."""
    longest = max(v["wall_s"] for v in SESSION.values())
    px = (AXIS_X1 - AXIS_X0) / longest
    lanes = {}
    for key, s in SESSION.items():
        n = len(s["calls"])
        at = [s["wall_s"] * (i + 1) / (n + 1.4) for i in range(n)]
        lanes[key] = {"bar_w": s["wall_s"] * px, "grow_s": GROW_S * s["wall_s"] / longest,
                      "calls": [(c, AXIS_X0 + a * px, START_S + GROW_S * a / longest) for c, a in zip(s["calls"], at)]}
    return lanes


def session(t):
    w, h = 760, 290
    lanes = session_lanes()
    body = [
        text(24, 32, "One task, two sessions", 17, t["fg"], weight="600"),
        text(24, 54, "Means per session · benchmark v13 · 60 paired tasks · claude-sonnet-5-5 · "
                     "bar length = wall-clock time", 12, t["muted"]),
    ]
    css = []
    rows = (("stock", "stock Claude Code", t["base"], 122), ("laya", "with laya-codex", t["laya"], 226))
    for key, label, color, y in rows:
        s, lane = SESSION[key], lanes[key]
        body.append(text(24, y + 5, label, 14, t["fg"], weight="600"))
        body.append(text(24, y + 23, f"{s['tool_calls']} tool calls", 12, t["muted"]))
        body.append(rect(AXIS_X0, y - 9, AXIS_X1 - AXIS_X0, 18, t["grid"], 4, extra=' fill-opacity="0.35"'))
        css.append(grow(f"g{key}", START_S, START_S + lane["grow_s"], CYCLE_S))
        body.append(rect(AXIS_X0, y - 9, lane["bar_w"], 18, color, 4, f"g{key}"))
        if key == "laya":
            # The code arrives with the prompt: the right file is in context at the very start.
            css.append(fade_in("playa", START_S * 0.5, CYCLE_S))
            body.append(group("playa", [
                f'<circle cx="{AXIS_X0:.1f}" cy="{y:.1f}" r="7" fill="{t["bg"]}" stroke="{t["laya"]}" stroke-width="3"/>',
                rect(AXIS_X0 - 8, y + 16, 160, 22, t["laya"], 11, extra=' fill-opacity="0.16"'),
                text(AXIS_X0 + 72, y + 31, "prompt + ranked code", 12, t["fg"], "middle", "600"),
                text(AXIS_X0 + 162, y + 31, s["right_file"], 12, t["laya"], "start", "600"),
            ]))
        for i, (call, x, at) in enumerate(lane["calls"]):
            name = f"c{key}{i}"
            css.append(fade_in(name, at, CYCLE_S))
            items = [rect(x - 24, y - 40, 48, 22, t["bg"], 11, extra=f' stroke="{color}" stroke-width="1.5"'),
                     text(x, y - 25, call, 12, t["fg"], "middle"),
                     f'<line x1="{x:.1f}" y1="{y - 18:.1f}" x2="{x:.1f}" y2="{y - 9:.1f}" stroke="{color}" stroke-width="1.5"/>']
            if key == "stock" and i == 3:
                items += [f'<circle cx="{x:.1f}" cy="{y:.1f}" r="7" fill="{t["bg"]}" stroke="{t["laya"]}" stroke-width="3"/>',
                          text(x, y + 30, s["right_file"], 12, t["muted"], "middle")]
            body.append(group(name, items))
        end_x = AXIS_X0 + lane["bar_w"]
        name = f"e{key}"
        css.append(fade_in(name, START_S + lane["grow_s"], CYCLE_S))
        items = [text(AXIS_X1 + 14, y - 1, f"{s['wall_s']} s", 15, t["fg"], "start", "600"),
                 text(AXIS_X1 + 14, y + 16, f"${s['cost']:.3f}", 12, t["muted"])]
        longest = max(v["wall_s"] for v in SESSION.values())
        if s["wall_s"] < longest:
            saved = 100 * (s["wall_s"] / longest - 1)
            items.append(text((end_x + AXIS_X1) / 2, y + 4, f"{saved:.0f}%".replace("-", "−"), 12, t["fg"], "middle", "600"))
        body.append(group(name, items))
    return doc(w, h, body, css, t, "One task, two sessions: stock Claude Code 23.2 s, $0.128, 5.9 tool calls, right file at "
               "turn 4; with laya-codex 18.7 s, $0.110, 2.3 tool calls, right file before turn 1 in 52 of 60 tasks")


# --- 2. the pipeline --------------------------------------------------------------------------

def box(x, y, w, h, lines, t, accent=False):
    stroke = t["laya"] if accent else t["grid"]
    out = [rect(x, y, w, h, t["bg"], 8, extra=f' stroke="{stroke}" stroke-width="{2 if accent else 1.5}"')]
    if accent:
        out.append(rect(x, y, w, h, t["laya"], 8, extra=' fill-opacity="0.10"'))
    y0 = y + h / 2 - (len(lines) - 1) * 8 + 4
    for i, line in enumerate(lines):
        out.append(text(x + w / 2, y0 + i * 16, line, 13 if i == 0 else 11,
                        t["fg"] if i == 0 else t["muted"], "middle", "600" if i == 0 else "normal"))
    return "".join(out)


def flow(path, t, color=None, dashed=True):
    c = color or t["muted"]
    cls = ' class="flow"' if dashed else ""
    return (f'<path{cls} d="{path}" fill="none" stroke="{c}" stroke-width="1.8" stroke-dasharray="6 5" '
            f'stroke-linecap="round" marker-end="url(#arrow)"/>')


def pipeline(t):
    w, h = 760, 330
    bw, bh, gap, x0 = 136, 58, 15, 12
    xs = [x0 + i * (bw + gap) for i in range(5)]
    ya, yb = 96, 206
    css = ["@keyframes march{to{stroke-dashoffset:-22}}.flow{animation:march 1.1s linear infinite}"]
    body = [
        f'<defs><marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" '
        f'orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="{t["muted"]}"/></marker></defs>',
        text(24, 32, "How laya-codex picks the code", 17, t["fg"], weight="600"),
        text(24, 54, "Everything runs on your machine · the Laya re-ranker decides the order", 12, t["muted"]),
        text(x0, ya - 12, "Once, then on every edit", 12, t["muted"]),
        text(x0, yb - 12, "On every prompt", 12, t["muted"]),
    ]
    body += [
        box(xs[0], ya, bw, bh, ["Your repository", "any git repo"], t),
        box(xs[1] + bw / 2 + gap / 2, ya, bw + 20, bh, ["tree-sitter chunks", "10–50 lines · 14 languages"], t),
        box(xs[3] - 10, ya, bw, bh, ["Moon", "local keyword index"], t),
    ]
    body.append(flow(f"M{xs[0] + bw:.0f},{ya + bh / 2:.0f} H{xs[1] + bw / 2 + gap / 2 - 2:.0f}", t))
    body.append(flow(f"M{xs[1] + bw * 1.5 + gap / 2 + 20:.0f},{ya + bh / 2:.0f} H{xs[3] - 12:.0f}", t))
    stages = [
        (["Your prompt", "the task, as typed"], False),
        (["Keyword search", "BM25 + symbols + paths", "24 candidates"], False),
        (["Laya re-ranker", "top 16 · about 0.5 s", "is it relevant?"], True),
        (["Prompt + code", "up to 9,500 chars", "top 2 files + map"], True),
        (["Claude Code", "starts on the task"], False),
    ]
    for i, (lines, accent) in enumerate(stages):
        body.append(box(xs[i], yb, bw, bh + 12, lines, t, accent))
        if i:
            body.append(flow(f"M{xs[i - 1] + bw:.0f},{yb + (bh + 12) / 2:.0f} H{xs[i] - 2:.0f}", t))
        name = f"s{i}"
        css.append(f"@keyframes {name}{{0%,{i * 16:.0f}%{{opacity:0}}{i * 16 + 6:.0f}%{{opacity:1}}"
                   f"{i * 16 + 18:.0f}%,100%{{opacity:0}}}}.{name}{{opacity:0;animation:{name} 6s ease-in-out infinite}}")
        body.append(rect(xs[i] - 3, yb - 3, bw + 6, bh + 18, "none", 10,
                         name, f' stroke="{t["laya"]}" stroke-width="3"'))
    moon_cx = xs[3] - 10 + bw / 2
    body.append(flow(f"M{moon_cx:.0f},{ya + bh:.0f} V{(ya + bh + yb) / 2:.0f} H{xs[1] + bw / 2:.0f} V{yb - 2:.0f}", t))
    loop_y = yb + bh + 12 + 34
    body.append(flow(f"M{xs[4] + bw / 2:.0f},{yb + bh + 12:.0f} V{loop_y} H{xs[1] + bw / 2:.0f} V{yb + bh + 16:.0f}", t, t["laya"]))
    body.append(text((xs[1] + xs[4] + bw) / 2, loop_y - 7,
                     "MCP search: Claude's own lookups (where is X, who calls X, which tests)", 12, t["fg"], "middle"))
    return doc(w, h, body, css, t, "How laya-codex picks the code: your repository is split by tree-sitter into 10–50-line "
               "chunks in Moon, a local keyword index; on every prompt keyword search finds 24 candidates, the Laya "
               "re-ranker scores the top 16 in about 0.5 s, and up to 9,500 characters of code reach Claude Code with "
               "the prompt; Claude's own lookups go through MCP search")


# --- 3. keywords find, Laya orders -------------------------------------------------------------

PITCH = 20
BAR_X = 250


def rerank_bars():
    return [
        {"label": "Whole repository", "note": "every chunk · not to scale", "n": None, "w": 24 * PITCH},
        {"label": "Keyword search", "note": "24 candidates", "n": 24, "w": 24 * PITCH},
        {"label": "Laya re-ranker", "note": "scores the top 16 · about 0.5 s", "n": 16, "w": 16 * PITCH},
        {"label": "Inlined in the prompt", "note": "top 2 files' code + a map", "n": 2, "w": 2 * PITCH},
    ]


def rerank(t):
    w, h = 760, 332
    cycle = 9.0
    body = [
        text(24, 32, "Keywords find the candidates, Laya orders them", 17, t["fg"], weight="600"),
        text(24, 54, "Per prompt · one square = one candidate block", 12, t["muted"]),
    ]
    css = []
    for r, bar in enumerate(rerank_bars()):
        y = 80 + r * 54
        body.append(text(24, y + 12, bar["label"], 14, t["fg"], weight="600"))
        body.append(text(24, y + 29, bar["note"], 12, t["muted"]))
        name = f"r{r}"
        start = 0.4 + r * 1.6
        if bar["n"] is None:
            css.append(grow(name, start, start + 1.0, cycle))
            body.append(rect(BAR_X, y + 4, bar["w"], 18, t["base"], 4, name, ' fill-opacity="0.5"'))
            continue
        color = t["laya"] if r >= 2 else t["base"]
        for i in range(bar["n"]):
            sq = f"{name}q{i}"
            css.append(fade_in(sq, start + i * 0.04, cycle))
            body.append(rect(BAR_X + i * PITCH, y + 4, PITCH - 4, 18, color, 3, sq))
    css.append(fade_in("blend", 0.4 + 4 * 1.6, cycle))
    body.append(group("blend", [
        rect(24, 284, w - 48, 32, t["laya"], 8, extra=' fill-opacity="0.12"'),
        text(w / 2, 305, "final score = 0.5 × keyword rank + 0.5 × model probability", 14, t["fg"], "middle", "600"),
    ]))
    return doc(w, h, body, css, t, "Keywords find the candidates, Laya orders them: keyword search narrows the repository "
               "to 24 candidate blocks, the Laya re-ranker scores the top 16 in about 0.5 s, and the code of the top 2 "
               "files is inlined; final score = 0.5 × keyword rank + 0.5 × model probability")


DIAGRAMS = [("session", session), ("pipeline", pipeline), ("rerank", rerank)]


def write_all(out):
    os.makedirs(out, exist_ok=True)
    for theme, t in THEMES.items():
        for name, fn in DIAGRAMS:
            path = os.path.join(out, f"diagram-{name}-{theme}.svg")
            with open(path, "w") as f:
                f.write(fn(t))
            print(path)


if __name__ == "__main__":
    write_all(sys.argv[1] if len(sys.argv) > 1 else "docs/assets")
