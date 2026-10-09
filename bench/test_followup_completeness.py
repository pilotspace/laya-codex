"""Tests for the follow-up completeness replay: `python3 -m unittest bench/test_followup_completeness.py`."""
import json
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import followup_completeness as fc  # noqa: E402

INJ1 = """<!-- laya-codex -->

Ranked locations:
1. src/store.rs — 1416-1432 fn enforce_budget; 1054-1088 impl VectorStore
2. src/budget.rs — 10-40 struct MmapBudget

### src/store.rs:1416-1432 — fn enforce_budget
```rust
fn enforce_budget(b: &mut MmapBudget) { helper_evict(b) }
fn helper_evict(b: &mut MmapBudget) {}
```
"""


def hook(text):
    return {"type": "system", "subtype": "hook_response", "hook_event": "UserPromptSubmit",
            "stdout": json.dumps({"hookSpecificOutput": {"additionalContext": text}})}


def call(rid, *tools):
    return {"type": "assistant", "request_id": rid, "timestamp": "2026-09-30T10:00:00Z",
            "message": {"content": [{"type": "tool_use", "id": i, "name": n, "input": inp} for i, n, inp in tools]}}


INIT = {"type": "system", "subtype": "init"}
SESSION = [
    hook(INJ1), INIT, {"type": "result", "result": "The check is `enforce_budget` in `VectorStore` (see `src/store.rs`)."},
    hook("Ranked locations:\n"), INIT,
    call("r1", ("t1", "mcp__laya-codex__search", {"query": "enforce_budget|MmapBudget"})),
    call("r2", ("t2", "Read", {"file_path": "/x/src/store.rs", "offset": 1, "limit": 9}),
         ("t3", "Grep", {"pattern": "fn test_budget"})),
    {"type": "result", "result": "Tests: ..."},
]


class Parsing(unittest.TestCase):
    def test_reads_injection_answer_tools_and_calls_per_prompt(self):
        p1, p2 = fc.parse_session(SESSION)
        self.assertIn("Ranked locations", p1["inj_text"])
        self.assertIn("enforce_budget", p1["answer"])
        self.assertEqual([t["name"] for t in p2["tools"].values()], ["mcp__laya-codex__search", "Read", "Grep"])
        self.assertEqual(p2["calls"], [["t1"], ["t2", "t3"]])

    def test_records_every_api_call_with_its_usage_and_the_prompt_usage(self):
        u1 = {"input_tokens": 2, "cache_creation_input_tokens": 300, "cache_read_input_tokens": 9000,
              "output_tokens": 5}
        u2 = dict(u1, output_tokens=40)
        fin = dict(u1, cache_creation_input_tokens=700, output_tokens=90)
        lines = [INIT,
                 dict(call("r1", ("t1", "Grep", {"pattern": "x"})), message={"content": [], "usage": u1}),
                 dict(call("r1", ("t1", "Grep", {"pattern": "x"})), message={
                     "content": [{"type": "tool_use", "id": "t1", "name": "Grep", "input": {}}], "usage": u2}),
                 {"type": "assistant", "request_id": "r2", "message": {"content": [{"type": "text", "text": "ok"}],
                                                                      "usage": fin}},
                 {"type": "result", "result": "ok",
                  "usage": {"output_tokens": 130, "iterations": [{"output_tokens": 95}]}}]
        (p,) = fc.parse_session(lines)
        self.assertEqual(p["api"], [{"rid": "r1", "usage": u1, "usage_last": u2, "tools": 1},
                                    {"rid": "r2", "usage": fin, "usage_last": fin, "tools": 0}])
        self.assertEqual(p["result_usage"]["output_tokens"], 130)
        self.assertEqual(p["calls"], [["t1"]])


class Rules(unittest.TestCase):
    def test_ranked_symbols_come_from_the_location_headers(self):
        p1, _ = fc.parse_session(SESSION)
        self.assertEqual(fc.ranked_symbols(p1), ["enforce_budget", "VectorStore", "MmapBudget"])

    def test_answer_names_skip_file_paths(self):
        p1, _ = fc.parse_session(SESSION)
        self.assertEqual(fc.answer_names(p1), ["enforce_budget", "VectorStore"])

    def test_inlined_definitions(self):
        p1, _ = fc.parse_session(SESSION)
        self.assertEqual(fc.inlined_definitions(p1), ["enforce_budget", "helper_evict"])


class Lookups(unittest.TestCase):
    def test_search_names_split_on_bars(self):
        self.assertEqual(fc.lookup_names({"name": "mcp__laya-codex__search", "input": {"query": "a_b|Foo::bar"}}),
                         ("search-name", {"a_b", "bar"}))

    def test_grep_names_drop_language_keywords(self):
        kind, names = fc.lookup_names({"name": "Grep", "input": {"pattern": r"fn test_budget\("}})
        self.assertEqual((kind, names), ("grep", {"test_budget"}))

    def test_read_is_never_a_name_lookup(self):
        self.assertEqual(fc.lookup_names({"name": "Read", "input": {"file_path": "x"}}), ("read", set()))


def fake_scan(rows_by_name):
    return lambda name: rows_by_name.get(name, [])


class Coverage(unittest.TestCase):
    def test_a_name_is_complete_only_within_the_line_cap_and_the_budget(self):
        scan = fake_scan({"a": [("x.rs", 1, "a")], "b": [("x.rs", i, "b") for i in range(100)], "c": [("y.rs", 2, "c")]})
        done, used = fc.complete_names(["a", "b", "c"], scan, cap=60, budget=10_000)
        self.assertEqual(done, {"a", "c"})
        done, _ = fc.complete_names(["a", "c"], scan, cap=60, budget=len(fc.render("a", scan("a"))))
        self.assertEqual(done, {"a"})

    def test_a_call_is_removable_only_if_all_its_tools_are_covered(self):
        _, p2 = fc.parse_session(SESSION)
        r = fc.evaluate(p2, {"enforce_budget", "MmapBudget", "test_budget"})
        self.assertEqual(r["calls_removable"], 1)        # r1; r2 also Reads
        self.assertFalse(r["all_covered"])               # the Read is never covered
        self.assertEqual(r["tools"], 3)

    def test_misses_are_classified(self):
        _, p2 = fc.parse_session(SESSION)
        r = fc.evaluate(p2, {"enforce_budget"}, mentioned={"enforce_budget", "MmapBudget"}, too_long={"MmapBudget"})
        self.assertEqual(r["misses"]["search-name: name has too many uses"], 1)
        self.assertEqual(r["misses"]["Read (wants code)"], 1)
        self.assertEqual(r["misses"]["grep: name not mentioned in prompt 1"], 1)


if __name__ == "__main__":
    unittest.main()
