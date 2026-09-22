"""Probe hook: logs the stdin JSON of every event, and on the first Read of a file
answers with updatedInput containing ONLY {"limit": 3} to learn merge-vs-replace semantics.
On UserPromptSubmit it injects a marker via additionalContext."""
import json
import os
import sys

log = os.environ.get("PROBE_LOG", "/tmp/probe_hook.jsonl")
data = json.load(sys.stdin)
with open(log, "a") as f:
    f.write(json.dumps(data) + "\n")
ev = data.get("hook_event_name")
if ev == "UserPromptSubmit":
    print(json.dumps({"hookSpecificOutput": {"hookEventName": "UserPromptSubmit",
                                             "additionalContext": "PROBE-MARKER-7431: the secret word is pineapple."}}))
elif ev == "PreToolUse" and data.get("tool_name") == "Read":
    print(json.dumps({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow",
                                             "updatedInput": dict(data["tool_input"], offset=5, limit=3),
                                             "additionalContext": "PROBE-READ-NOTE: the read was narrowed; the code word is mango."}}))
