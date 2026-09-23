"""Render a benchmark transcript (claude -p stream-json with hook events) as a readable timeline:
laya's injected context per prompt, hook decisions, Claude's tool calls and the final answers.

    python3 bench/show_session.py <raw/TASK_ARM.jsonl> [--full]
"""
import json
import sys

path = sys.argv[1]
full = "--full" in sys.argv


def hook_context(output):
    try:
        return json.loads(output)["hookSpecificOutput"].get("additionalContext", "")
    except (ValueError, KeyError, TypeError):
        return output or ""


def short(s, n=160):
    s = " ".join(str(s).split())
    return s if len(s) <= n else s[: n - 1] + "…"


prompt_no = 0
for line in open(path):
    try:
        e = json.loads(line)
    except ValueError:
        continue
    t = e.get("type")
    if t == "system" and e.get("subtype") == "hook_response":
        ev = e.get("hook_event")
        ctx = hook_context(e.get("output"))
        if ev == "UserPromptSubmit":
            prompt_no += 1
            print(f"\n{'=' * 100}\n[laya → UserPromptSubmit #{prompt_no}] injected {len(ctx)} chars (~{int(len(ctx) / 3.5)} tokens)\n{'-' * 100}")
            lines = ctx.splitlines()
            if not full:  # the ranked map in full, code blocks trimmed to 6 lines each
                out, in_code, kept = [], False, 0
                for l in lines:
                    if l.startswith("```"):
                        in_code, kept = not in_code, 0
                        out.append(l)
                    elif in_code:
                        kept += 1
                        if kept <= 6:
                            out.append(l)
                        elif kept == 7:
                            out.append("    … (code trimmed for display)")
                    else:
                        out.append(l)
                lines = out
            print("\n".join(lines))
            print("-" * 100)
        elif ev == "PreToolUse" and ctx:
            print(f"   [laya → PreToolUse] {short(ctx, 220)}")
    elif t == "user" and isinstance(e.get("message", {}).get("content"), str):
        print(f"\n>>> USER PROMPT: {short(e['message']['content'], 300)}")
    elif t == "assistant":
        for c in e.get("message", {}).get("content", []) or []:
            if c.get("type") == "tool_use":
                i = c["input"]
                arg = i.get("file_path") or i.get("pattern") or i.get("query") or json.dumps(i)
                extra = f" offset={i['offset']} limit={i['limit']}" if "offset" in i or "limit" in i else ""
                print(f"   Claude → {c['name']}({short(arg, 120)}){extra}")
    elif t == "result":
        print(f"\n<<< CLAUDE ANSWER (turns={e.get('num_turns')}, {e.get('duration_ms', 0) / 1000:.1f}s, ${e.get('total_cost_usd', 0):.3f}):")
        res = e.get("result", "")
        print(res if full else "\n".join(res.splitlines()[-12:]))
