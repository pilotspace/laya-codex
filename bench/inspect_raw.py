"""Print system/hook events and tool calls from a raw stream-json run file (debugging aid)."""
import json
import sys

for line in open(sys.argv[1]):
    try:
        e = json.loads(line)
    except ValueError:
        continue
    t, st = e.get("type"), e.get("subtype")
    if t == "system" and st == "init":
        print("init mcp=%s plugins=%s tools=%s" % (e.get("mcp_servers"), e.get("plugins"), e.get("tools")))
    elif t == "system":
        print("system", st, json.dumps({k: v for k, v in e.items() if k not in ("type", "subtype", "session_id", "uuid")})[:600])
    elif t == "assistant":
        for c in e.get("message", {}).get("content", []) or []:
            if c.get("type") == "tool_use":
                print("tool_use", c["name"], json.dumps(c.get("input"))[:160])
