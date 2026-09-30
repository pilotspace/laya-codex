"""The cost floor of a benchmark session: what a session pays whatever laya-codex does.

    python3 bench/cost_floor.py --root <run root with <repo>/{runs.jsonl,raw}>

The first API call of a session writes Claude Code's own context (system prompt, tools, the
prompt) to the cache; Claude writes its answers; the follow-up writes the first answer and the
new prompt; each prompt re-reads the conversation at least once. With zero lookups and zero
injection that is the floor; adding laya-codex's injection gives the floor with today's
injection. Prices are list prices per million tokens (Claude Code uses the one-hour cache).
The raw transcripts are not committed.
"""
import argparse
import json
import os
import statistics as st

PRICES = {"cache_write": 6.0, "cache_read": 0.30, "output": 15.0}
CHARS_PER_TOKEN = 3.5


def session(lines):
    s = {"first_write": None, "first_read": 0, "cache_write": 0, "cache_read": 0, "output": 0, "reported_usd": 0.0}
    for d in lines:
        if d.get("type") == "assistant" and s["first_write"] is None:
            u = d["message"].get("usage") or {}
            s["first_write"] = (u.get("cache_creation_input_tokens") or 0) + (u.get("input_tokens") or 0)
            s["first_read"] = u.get("cache_read_input_tokens") or 0
        elif d.get("type") == "result":
            u = d.get("usage") or {}
            s["cache_write"] += (u.get("cache_creation_input_tokens") or 0) + (u.get("input_tokens") or 0)
            s["cache_read"] += u.get("cache_read_input_tokens") or 0
            s["output"] += u.get("output_tokens") or 0
            s["reported_usd"] += d.get("total_cost_usd") or 0
    return s


def usd(tokens):
    return sum(tokens.get(k, 0) * p / 1e6 for k, p in PRICES.items())


def floor(first_write, answer_tokens, followup_write, min_reads):
    return usd({"cache_write": first_write + followup_write, "output": answer_tokens, "cache_read": min_reads})


def _jsonl(path):
    with open(path) as f:
        return [json.loads(x) for x in f if x.strip()]


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--root", required=True)
    ap.add_argument("--repos", default="moon,httpx,hono")
    a = ap.parse_args()
    by_arm = {}
    for repo in a.repos.split(","):
        for r in _jsonl(os.path.join(a.root, repo, "runs.jsonl")):
            s = session(_jsonl(os.path.join(a.root, repo, "raw", f"{r['task_id']}_{r['arm']}.jsonl")))
            s["answer_tokens"] = sum(r["prompt_answer_chars"]) / CHARS_PER_TOKEN
            s["answer1_tokens"] = r["prompt_answer_chars"][0] / CHARS_PER_TOKEN
            s["followup_injected"] = (r.get("prompt_injected_tokens") or [0, 0])[1]
            by_arm.setdefault(r["arm"], []).append(s)
    m = {arm: {k: st.mean(s[k] for s in rows) for k in rows[0]} for arm, rows in by_arm.items()}
    for arm, v in m.items():
        print(f"{arm:9s} reported ${v['reported_usd']:.4f}; first call writes {v['first_write']:.0f}, reads "
              f"{v['first_read']:.0f}; session writes {v['cache_write']:.0f}, reads {v['cache_read']:.0f}, "
              f"output {v['output']:.0f}; answers ~{v['answer_tokens']:.0f} tokens")
    b, lay = m["baseline"], m["laya"]
    followup = b["answer1_tokens"] + 100          # the first answer and the follow-up prompt
    reads = 2 * b["first_read"] + b["first_write"]  # each prompt re-reads the conversation once
    zero = floor(b["first_write"], b["answer_tokens"], followup, reads)
    with_inj = floor(lay["first_write"], b["answer_tokens"], followup + lay["followup_injected"], reads)
    goal = b["reported_usd"] / 2
    print(f"floor, zero lookups and no injection: ${zero:.4f} ({100 * (zero / b['reported_usd'] - 1):+.0f}% vs stock)")
    print(f"floor, zero lookups with today's injection: ${with_inj:.4f} ({100 * (with_inj / b['reported_usd'] - 1):+.0f}%)")
    print(f"the -50% goal: ${goal:.4f}")


if __name__ == "__main__":
    main()
