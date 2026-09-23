"""Tune adaptive sizing thresholds offline on dumped dev-set query results.

Replays laya-rank `size_context` selection (scope caps + tau_full/tau_map + min_full/min_map;
see crates/laya-rank/src/sizing.rs) over a threshold grid and compares with the fixed compact
format (top 3 spans with full code, 10 in the ranked map). Per config:
- R_full: share of gold files that got full code
- R_seen: share of gold files named in the map (full + map)
- full_lines: lines of inlined code (the token cost driver)
The "oracle-scope" row sizes with the scope implied by the gold files (1 file -> file,
2-3 files in one directory -> module, else cross) to bound what a perfect classifier adds.

    python3 bench/size_sweep.py bench/results/dev-dumps/code-w0.5-s128-k24-t0.jsonl
"""
import json
import os
import sys

CAPS = {  # scope -> (map_spans, full_spans)
    "function": (5, 1), "file": (8, 2), "module": (10, 3), "cross": (12, 3), None: (10, 3),
}
MIN_FULL, MIN_MAP = 1, 3


def select(spans, scope, tau_full, tau_map):
    map_cap, full_cap = CAPS[scope]
    ps = [s.get("p_relevant") for s in spans]
    if any(p is None for p in ps):  # lexical: top full_cap full, rest to the map
        return spans[:full_cap], spans[full_cap:map_cap]
    full = [i for i, p in enumerate(ps) if p >= tau_full][:full_cap]
    if len(full) < MIN_FULL:
        full = list(range(min(MIN_FULL, len(spans), full_cap)))
    budget = map_cap - len(full)
    rest = [i for i in range(len(spans)) if i not in full]
    mp = [i for i in rest if ps[i] >= tau_map][:budget]
    target = min(MIN_MAP - len(full), budget)
    if len(full) + len(mp) < MIN_MAP and len(mp) < target:
        mp = rest[:target]
    return [spans[i] for i in full], [spans[i] for i in mp]


def oracle_scope(gold):
    if len(gold) == 1:
        return "file"
    dirs = {os.path.dirname(g) for g in gold}
    return "module" if len(gold) <= 3 and len(dirs) == 1 else "cross"


def evaluate(rows, pick):
    rf = rs = lines = nfull = 0.0
    for r in rows:
        gold = set(r["gold"])
        full, mp = pick(r)
        full_files = {s["path"] for s in full}
        seen = full_files | {s["path"] for s in mp}
        rf += len(full_files & gold) / len(gold)
        rs += len(seen & gold) / len(gold)
        lines += sum(s["end_line"] - s["start_line"] + 1 for s in full)
        nfull += len(full)
    n = len(rows)
    return {"R_full": round(rf / n, 3), "R_seen": round(rs / n, 3), "full_lines": round(lines / n, 1), "full_spans": round(nfull / n, 2)}


def main():
    rows = [json.loads(l) for l in open(sys.argv[1])]
    rows = [r for r in rows if r["result"]["spans"]]
    lexical = sum(any(s.get("p_relevant") is None for s in r["result"]["spans"]) for r in rows)
    print(f"{sys.argv[1]}: n={len(rows)} (lexical fallback {lexical})")
    out = [("fixed compact (top3 full, 10 map)", evaluate(rows, lambda r: (r["result"]["spans"][:3], r["result"]["spans"][3:10])))]
    for tf in (0.30, 0.35, 0.40, 0.45, 0.50):
        for tm in (0.10, 0.20, 0.30):
            out.append((f"tau_full={tf:.2f} tau_map={tm:.2f}", evaluate(rows, lambda r, tf=tf, tm=tm: select(r["result"]["spans"], None, tf, tm))))
    out.append(("oracle-scope, tau 0.40/0.20", evaluate(rows, lambda r: select(r["result"]["spans"], oracle_scope(r["gold"]), 0.40, 0.20))))
    print("| config | R_full | R_seen | full lines | full spans |")
    print("|---|---|---|---|---|")
    for name, m in out:
        print(f"| {name} | {m['R_full']} | {m['R_seen']} | {m['full_lines']} | {m['full_spans']} |")


if __name__ == "__main__":
    main()
