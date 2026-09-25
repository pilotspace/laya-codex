"""Unit tests for the measurement tooling: token metric, tool-call ledger, offline replay
parsing, and pilot mode.

    python3 -m pytest bench/test_bench.py
"""
import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))


def write_jsonl(rows):
    f = tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False)
    f.write("\n".join(json.dumps(r) for r in rows) + "\n")
    f.close()
    return f.name


def run_row(arm, task, **kw):
    r = {"arm": arm, "task_id": task, "rc": [0], "wall_s": 10.0, "reading_tokens": 100,
         "injected_tokens": 0, "total_input_tokens": 1000, "output_tokens": 50, "num_turns": 4,
         "cost_usd": 0.1, "recall": 1.0, "precision": 0.5, "hit_any": 1.0, "tool_calls": {}, "hook_actions": {}}
    r.update(kw)
    return r


class TokenMetricIsPrimary(unittest.TestCase):
    """VISION.md: the token target is tokens read plus tokens injected -- it must lead every
    report, with tokens-read shown beside it, not the other way round."""

    def test_stats_metric_order_leads_with_reading_plus_injected(self):
        import stats
        keys = list(stats.METRICS)
        self.assertEqual(keys[0], "reading+injected tokens")
        self.assertEqual(keys[1], "code-reading tokens")

    def test_stats_reading_tolerates_missing_fields(self):
        import stats
        self.assertEqual(stats.METRICS["reading+injected tokens"]({}), 0)
        self.assertEqual(stats.METRICS["reading+injected tokens"]({"reading_tokens": None, "injected_tokens": 5}), 5)

    def test_stats_pooled_metric_order_leads_with_reading_plus_injected(self):
        import stats_pooled
        keys = list(stats_pooled.METRICS)
        self.assertEqual(keys[0], "reading+injected tokens")
        self.assertEqual(keys[1], "code-reading tokens")

    def test_stats_pooled_reading_tolerates_missing_fields(self):
        import stats_pooled
        self.assertEqual(stats_pooled.METRICS["reading+injected tokens"]({}), 0)

    def test_headline_metric_order_leads_with_reading_plus_injected(self):
        import headline
        keys = list(headline.METRICS)
        self.assertEqual(keys[0], "reading_plus_injected")
        self.assertEqual(keys[1], "reading_tokens")
        self.assertEqual(headline.LABELS["reading_plus_injected"], "Reading + injected")

    def test_headline_chart_leads_with_reading_plus_injected(self):
        import headline
        self.assertEqual(headline.CHART_METRICS[0], "reading_plus_injected")
        self.assertIn("reading_tokens", headline.CHART_METRICS)

    def test_headline_title_does_not_assert_a_direction_the_data_may_not_show(self):
        import headline
        self.assertNotIn("reads less code", headline.CHART_TITLE.lower())


class StatsCliIsImportSafe(unittest.TestCase):
    """stats.py must be importable (for METRICS) without running its CLI as a side effect."""

    def test_stats_runs_only_from_main(self):
        import stats
        self.assertTrue(hasattr(stats, "main"))


class ToolCallLedger(unittest.TestCase):
    def test_calls_per_session_split_by_named_tool_and_other(self):
        import ledger
        rows = [
            run_row("baseline", "t1", tool_calls={"Grep": 5, "Read": 3}),
            run_row("baseline", "t2", tool_calls={"Grep": 6, "Read": 2, "Bash": 1}),
            run_row("branch", "t1", tool_calls={"Grep": 3, "Read": 2, "mcp__laya-codex__search": 1}),
        ]
        t = ledger.tool_ledger(rows)
        self.assertAlmostEqual(t["baseline"]["Grep"], 5.5)
        self.assertAlmostEqual(t["baseline"]["Read"], 2.5)
        self.assertAlmostEqual(t["baseline"]["other"], 0.5)  # Bash, unnamed
        self.assertAlmostEqual(t["baseline"]["total"], 8.5)
        self.assertAlmostEqual(t["branch"]["mcp__laya-codex__search"], 1.0)
        self.assertAlmostEqual(t["branch"]["other"], 0.0)

    def test_missing_tool_calls_field_is_zero_not_an_error(self):
        import ledger
        rows = [run_row("baseline", "t1")]
        del rows[0]["tool_calls"]
        t = ledger.tool_ledger(rows)
        self.assertEqual(t["baseline"]["total"], 0)

    def test_empty_rows_yields_empty_ledger_no_zero_division(self):
        import ledger
        self.assertEqual(ledger.tool_ledger([]), {})

    def test_hook_actions_per_session(self):
        import ledger
        rows = [
            run_row("branch", "t1", hook_actions={"inject": 2, "already_ranged": 1}),
            run_row("branch", "t2", hook_actions={"inject": 1}),
        ]
        h = ledger.hook_action_ledger(rows)
        self.assertAlmostEqual(h["branch"]["inject"], 1.5)
        self.assertAlmostEqual(h["branch"]["already_ranged"], 0.5)

    def test_missing_hook_actions_field_is_empty_not_an_error(self):
        import ledger
        rows = [run_row("baseline", "t1")]
        del rows[0]["hook_actions"]
        h = ledger.hook_action_ledger(rows)
        self.assertEqual(h["baseline"], {})

    def test_load_rows_pools_several_run_dirs(self):
        import ledger
        d1 = tempfile.mkdtemp()
        d2 = tempfile.mkdtemp()
        open(os.path.join(d1, "runs.jsonl"), "w").write(json.dumps(run_row("baseline", "t1")) + "\n")
        open(os.path.join(d2, "runs.jsonl"), "w").write(json.dumps(run_row("baseline", "t2")) + "\n")
        rows = ledger.load_rows([d1, d2])
        self.assertEqual({r["task_id"] for r in rows}, {"t1", "t2"})

    def test_v9_ledger_reproduces_the_committed_numbers(self):
        """bench/results/claude-v9: baseline 8.45 tool calls/session (Grep 5.20), branch 5.10
        (Grep 2.85, search 0.02) -- the numbers VISION.md and the team's verified facts record."""
        import ledger
        here = os.path.dirname(os.path.abspath(__file__))
        dirs = [os.path.join(here, "results", "claude-v9", r) for r in ("moon", "httpx", "hono")]
        rows = ledger.load_rows(dirs)
        t = ledger.tool_ledger(rows)
        self.assertAlmostEqual(t["baseline"]["total"], 8.45, places=2)
        self.assertAlmostEqual(t["baseline"]["Grep"], 5.20, places=2)
        self.assertAlmostEqual(t["branch"]["total"], 5.10, places=2)
        self.assertAlmostEqual(t["branch"]["Grep"], 2.85, places=2)
        self.assertAlmostEqual(t["branch"]["mcp__laya-codex__search"], 0.02, places=2)


class ReplayParse(unittest.TestCase):
    def test_inlined_blocks_and_gold_coverage(self):
        import replay_hooks
        ctx = ("1. src/foo.rs — does the thing\n"
               "### src/foo.rs:10-20\ncode about src/bar.py:5\n"
               "### src/baz.ts:1-5\nmore code\n")
        gold = ["src/foo.rs", "src/qux.rs"]
        row = replay_hooks.parse(ctx, gold)
        self.assertEqual(row["inlined"], ["src/foo.rs", "src/baz.ts"])
        self.assertEqual(row["gold_inlined"], ["src/foo.rs"])
        self.assertEqual(row["gold_top2"], ["src/foo.rs"])
        self.assertIn("src/foo.rs", row["gold_named"])

    def test_gold_named_but_not_in_top_two_is_excluded_from_top2(self):
        import replay_hooks
        ctx = "### a.py:1-2\nx\n### b.py:1-2\nx\n### gold.py:1-2\nx\n"
        row = replay_hooks.parse(ctx, ["gold.py"])
        self.assertEqual(row["gold_inlined"], ["gold.py"])
        self.assertEqual(row["gold_top2"], [])

    def test_no_injection_is_empty_not_an_error(self):
        import replay_hooks
        row = replay_hooks.parse("", ["gold.py"])
        self.assertEqual(row["inlined"], [])
        self.assertEqual(row["gold_inlined"], [])
        self.assertEqual(row["gold_top2"], [])

    def test_percentile_p50_and_p95(self):
        import replay_hooks
        values = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0]
        self.assertAlmostEqual(replay_hooks.percentile(values, 0.50), 0.6)
        self.assertAlmostEqual(replay_hooks.percentile(values, 0.95), 1.0)

    def test_percentile_of_one_value(self):
        import replay_hooks
        self.assertEqual(replay_hooks.percentile([0.42], 0.95), 0.42)

    def test_summary_prints_gold_top2_and_p95_columns(self):
        import contextlib
        import io
        import replay_hooks
        path = write_jsonl([
            {"label": "blend", "repo": "httpx", "task_id": "t1", "turn": 1, "gold": ["a.py"],
             "hook_s": 0.5, "action": "inject", "rank_mode": "laya", "scored": 16, "offered": 16,
             "candidates": 24, "chars": 100, "inlined": ["a.py"], "gold_inlined": ["a.py"],
             "gold_named": ["a.py"], "gold_top2": ["a.py"]},
        ])
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            replay_hooks.summary([path])
        out = buf.getvalue()
        self.assertIn("gold in top 2", out)
        self.assertIn("hook s p95", out)
        self.assertIn("1/1", out)

    def test_summary_tolerates_rows_without_gold_top2_from_older_replays(self):
        import contextlib
        import io
        import replay_hooks
        path = write_jsonl([
            {"label": "old", "repo": "httpx", "task_id": "t1", "turn": 1, "gold": ["a.py"],
             "hook_s": 0.5, "action": "inject", "rank_mode": "laya", "chars": 100,
             "inlined": ["a.py"], "gold_inlined": ["a.py"], "gold_named": ["a.py"]},
        ])
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            replay_hooks.summary([path])  # must not raise KeyError


class PilotMode(unittest.TestCase):
    def setUp(self):
        import run_bench
        self.rb = run_bench

    def test_pilot_subset_is_three_tasks_evenly_spread_across_the_file(self):
        tasks = [{"id": "t%02d" % i} for i in range(20)]
        subset = self.rb.pilot_subset(tasks)
        self.assertEqual(len(subset), 3)
        self.assertEqual(subset[0]["id"], "t00")
        self.assertEqual(subset[-1]["id"], "t19")

    def test_pilot_subset_is_deterministic(self):
        tasks = [{"id": "t%02d" % i} for i in range(17)]
        self.assertEqual(self.rb.pilot_subset(tasks), self.rb.pilot_subset(tasks))

    def test_pilot_subset_smaller_than_n_returns_everything(self):
        tasks = [{"id": "a"}, {"id": "b"}]
        self.assertEqual(self.rb.pilot_subset(tasks), tasks)

    def test_pilot_subset_of_one_task(self):
        tasks = [{"id": "solo"}]
        self.assertEqual(self.rb.pilot_subset(tasks), tasks)

    def test_pilot_subset_custom_n(self):
        tasks = [{"id": "t%02d" % i} for i in range(10)]
        subset = self.rb.pilot_subset(tasks, n=5)
        self.assertEqual(len(subset), 5)

    def test_pilot_requires_max_total_usd(self):
        import argparse
        args = argparse.Namespace(pilot=True, max_total_usd=0.0)
        with self.assertRaises(SystemExit):
            self.rb.check_pilot_args(args)

    def test_pilot_with_budget_passes(self):
        import argparse
        args = argparse.Namespace(pilot=True, max_total_usd=5.0)
        self.rb.check_pilot_args(args)  # no raise

    def test_non_pilot_run_does_not_require_a_budget(self):
        import argparse
        args = argparse.Namespace(pilot=False, max_total_usd=0.0)
        self.rb.check_pilot_args(args)  # no raise

    def test_budget_reached(self):
        self.assertTrue(self.rb.budget_reached(5.0, 5.0))
        self.assertTrue(self.rb.budget_reached(5.01, 5.0))
        self.assertFalse(self.rb.budget_reached(4.99, 5.0))
        self.assertFalse(self.rb.budget_reached(1.0, 0.0))  # no cap set


class ReportIncludesLedger(unittest.TestCase):
    """The routine per-run summary (bench/run_bench.py report) must carry the tool-call ledger too,
    not just token/wall/cost means -- the plan asks summaries to report it, not a separate tool."""

    def test_summary_md_includes_the_tool_call_ledger(self):
        import argparse
        import contextlib
        import io
        import run_bench
        out = tempfile.mkdtemp()
        rows = [
            run_row("baseline", "t1", tool_calls={"Grep": 4}, hook_actions={}),
            run_row("laya", "t1", tool_calls={"Grep": 1, "mcp__laya-codex__search": 1}, hook_actions={"inject": 1}),
        ]
        with open(os.path.join(out, "runs.jsonl"), "w") as f:
            f.write("\n".join(json.dumps(r) for r in rows) + "\n")
        with contextlib.redirect_stdout(io.StringIO()):
            run_bench.report(argparse.Namespace(out=out))
        md = open(os.path.join(out, "summary.md")).read()
        self.assertIn("mcp__laya-codex__search", md)
        self.assertIn("tool calls/session", md)


if __name__ == "__main__":
    unittest.main()
