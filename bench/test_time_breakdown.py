"""Tests for the session time breakdown: `python3 -m unittest bench/test_time_breakdown.py`."""
import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import time_breakdown as tb  # noqa: E402


def at(sec):
    return f"2026-09-30T10:00:{sec:06.3f}Z"


def assistant(sec, rid, *blocks):
    return {"type": "assistant", "timestamp": at(sec), "request_id": rid, "message": {"content": list(blocks)}}


def tool_use(uid, name="Grep"):
    return {"type": "tool_use", "id": uid, "name": name, "input": {}}


def tool_result(sec, uid):
    return {"type": "user", "timestamp": at(sec), "message": {"content": [{"type": "tool_result", "tool_use_id": uid}]}}


INIT = {"type": "system", "subtype": "init"}


def result(ms):
    return {"type": "result", "subtype": "success", "duration_ms": ms, "duration_api_ms": ms}


# One prompt: a Grep call ending at 3.0 s, its result at 3.5 s, then the answer ending at 9.0 s.
# Claude Code reports 10 s for the prompt, of which 0.5 s is the laya-codex hook.
ONE_PROMPT = [INIT, assistant(3.0, "r1", tool_use("t1")), tool_result(3.5, "t1"),
              assistant(9.0, "r2", {"type": "text", "text": "answer"}), result(10_000)]


class Prompts(unittest.TestCase):
    def test_splits_a_prompt_into_lookups_tools_hook_and_answer(self):
        [p] = tb.prompts(ONE_PROMPT, [0.5])
        self.assertAlmostEqual(p["answer_s"], 5.5)       # 3.5 -> 9.0
        self.assertAlmostEqual(p["tools_s"], 0.5)        # 3.0 -> 3.5
        self.assertAlmostEqual(p["hook_s"], 0.5)
        self.assertAlmostEqual(p["lookups_s"], 3.5)      # what is left of the 10 s
        self.assertEqual(p["lookup_calls"], 1)

    def test_the_parts_add_up_to_claude_codes_duration(self):
        [p] = tb.prompts(ONE_PROMPT, [0.5])
        self.assertAlmostEqual(p["answer_s"] + p["tools_s"] + p["hook_s"] + p["lookups_s"], 10.0)

    def test_events_of_one_request_are_one_call(self):
        # Claude Code emits one assistant event per content block; both share the request id.
        lines = [INIT, assistant(2.0, "r1", {"type": "thinking", "thinking": ""}), assistant(3.0, "r1", tool_use("t1")),
                 tool_result(3.5, "t1"), assistant(9.0, "r2", {"type": "text", "text": "a"}), result(10_000)]
        [p] = tb.prompts(lines, [])
        self.assertEqual(p["lookup_calls"], 1)
        self.assertAlmostEqual(p["answer_s"], 5.5)

    def test_a_prompt_answered_in_one_call_has_no_lookups(self):
        lines = [INIT, assistant(6.0, "r1", {"type": "text", "text": "a"}), result(7_000)]
        [p] = tb.prompts(lines, [0.5])
        self.assertEqual(p["lookup_calls"], 0)
        self.assertAlmostEqual(p["lookups_s"], 0.0)
        self.assertAlmostEqual(p["answer_s"], 6.5)

    def test_hook_times_follow_prompt_order(self):
        lines = ONE_PROMPT + [INIT, assistant(15.0, "r3", {"type": "text", "text": "b"}), result(4_000)]
        ps = tb.prompts(lines, [0.5, 0.25])
        self.assertEqual([p["hook_s"] for p in ps], [0.5, 0.25])
        self.assertAlmostEqual(ps[1]["answer_s"], 3.75)


class Sessions(unittest.TestCase):
    def test_overhead_is_wall_clock_outside_claude_codes_prompts(self):
        s = tb.session(tb.prompts(ONE_PROMPT, [0.5]), wall_s=10.8)
        self.assertAlmostEqual(s["overhead_s"], 0.8)
        self.assertAlmostEqual(sum(s[k] for k in tb.PARTS), 10.8)

    def test_summary_means_per_arm_and_reads_run_directories(self):
        with tempfile.TemporaryDirectory() as root:
            repo = os.path.join(root, "moon")
            os.makedirs(os.path.join(repo, "raw"))
            os.makedirs(os.path.join(repo, "hooklogs"))
            with open(os.path.join(repo, "runs.jsonl"), "w") as f:
                for arm, wall in (("baseline", 10.8), ("laya", 10.8)):
                    f.write(json.dumps({"task_id": "t", "arm": arm, "wall_s": wall}) + "\n")
            for arm in ("baseline", "laya"):
                with open(os.path.join(repo, "raw", f"t_{arm}.jsonl"), "w") as f:
                    f.write("\n".join(json.dumps(x) for x in ONE_PROMPT) + "\n")
            with open(os.path.join(repo, "hooklogs", "t_laya.jsonl"), "w") as f:
                f.write(json.dumps({"event": "SessionStart", "elapsed_ms": 0}) + "\n")
                f.write(json.dumps({"event": "UserPromptSubmit", "elapsed_ms": 500}) + "\n")
            out = tb.summarize(root, ["moon"])
        self.assertEqual(out["n_tasks"], 1)
        self.assertAlmostEqual(out["arms"]["baseline"]["hook_s"], 0.0)
        self.assertAlmostEqual(out["arms"]["baseline"]["lookups_s"], 4.0)
        self.assertAlmostEqual(out["arms"]["laya"]["hook_s"], 0.5)
        self.assertAlmostEqual(out["arms"]["laya"]["wall_s"], 10.8)
        self.assertIn("moon", out["per_repo"])


class Chart(unittest.TestCase):
    def test_chart_labels_bars_with_the_measured_wall_clock(self):
        summary = {"n_tasks": 60, "label": "benchmark v13", "arms": {
            "baseline": {"answer_s": 12.36, "lookups_s": 9.91, "hook_s": 0.0, "tools_s": 0.13, "overhead_s": 0.78,
                         "wall_s": 23.19, "lookup_calls": 3.9},
            "laya": {"answer_s": 12.52, "lookups_s": 4.07, "hook_s": 1.06, "tools_s": 0.29, "overhead_s": 0.81,
                     "wall_s": 18.74, "lookup_calls": 1.75}}}
        for theme in tb.charts.THEMES.values():
            doc = tb.chart(summary, theme)
            self.assertIn("23.2 s", doc)
            self.assertIn("18.7 s", doc)  # not 18.8, the sum of rounded parts
            self.assertNotIn("18.8 s", doc)


if __name__ == "__main__":
    unittest.main()
