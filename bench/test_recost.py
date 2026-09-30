"""Tests for re-deriving session costs from raw transcripts: `python3 -m unittest bench/test_recost.py`."""
import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import recost  # noqa: E402
import runs  # noqa: E402


def assistant(mid, read):
    return {"type": "assistant", "message": {"id": mid, "content": [], "usage": {"cache_read_input_tokens": read}}}


def result(total):
    return {"type": "result", "result": "FILES: a.py", "total_cost_usd": total}


class Prices(unittest.TestCase):
    def test_list_prices_reproduce_a_reported_cost(self):
        # A v14 session's usage and its modelUsage costUSD (0.04923), one-hour cache writes.
        usage = {"input_tokens": 8, "cache_creation_input_tokens": 5015, "cache_read_input_tokens": 67770,
                 "output_tokens": 1560}
        self.assertAlmostEqual(runs.priced(usage), 0.04923, places=8)


class SessionCost(unittest.TestCase):
    def test_last_running_total_is_the_session_cost(self):
        s = recost.session_cost([assistant("m1", 5000), assistant("m1", 5000), assistant("m2", 9000),
                                 result(0.08), assistant("m3", 20000), result(0.11)])
        self.assertAlmostEqual(s["cost_usd"], 0.11)
        self.assertEqual(s["prompt_cost_usd"], [0.08, 0.03])
        self.assertEqual(s["first_call_cache_read"], 5000)

    def test_a_session_without_a_result_costs_nothing(self):
        self.assertEqual(recost.session_cost([assistant("m1", 10)])["cost_usd"], 0.0)


class Rewrite(unittest.TestCase):
    def run_dir(self, d, sessions):
        os.makedirs(os.path.join(d, "raw"))
        with open(os.path.join(d, "runs.jsonl"), "w") as f:
            for (task, arm), (summed, events) in sessions.items():
                f.write(json.dumps({"task_id": task, "arm": arm, "rep": 0, "cost_usd": summed}) + "\n")
                with open(os.path.join(d, "raw", "%s_%s.jsonl" % (task, arm)), "w") as r:
                    r.write("\n".join(json.dumps(e) for e in events))

    def test_rewrite_keeps_the_summed_cost_and_is_idempotent(self):
        with tempfile.TemporaryDirectory() as d:
            self.run_dir(d, {("t1", "laya"): (0.19, [assistant("m", 4000), result(0.08), result(0.11)])})
            recost.rewrite(d)
            recost.rewrite(d)
            row = json.loads(open(os.path.join(d, "runs.jsonl")).read())
        self.assertAlmostEqual(row["cost_usd"], 0.11)
        self.assertAlmostEqual(row["cost_usd_summed"], 0.19)
        self.assertEqual(row["prompt_cost_usd"], [0.08, 0.03])
        self.assertEqual(row["first_call_cache_read"], 4000)


class Cold(unittest.TestCase):
    def test_a_first_call_read_above_the_arms_usual_is_repriced_as_a_write(self):
        # Rows in run order. Arm b ran second on t1 and read a's cached first request (9000 tokens
        # more than its usual 1000); on t2 it ran first and read only its own system prompt.
        rows = [{"task_id": "t1", "arm": "a", "cost_usd": 0.10, "first_call_cache_read": 1000},
                {"task_id": "t1", "arm": "b", "cost_usd": 0.05, "first_call_cache_read": 10000},
                {"task_id": "t2", "arm": "b", "cost_usd": 0.09, "first_call_cache_read": 1000},
                {"task_id": "t2", "arm": "a", "cost_usd": 0.10, "first_call_cache_read": 1000}]
        cold = recost.cold_costs(rows)
        write_minus_read = (runs.PRICES_PER_M["cache_creation_input_tokens"]
                            - runs.PRICES_PER_M["cache_read_input_tokens"]) / 1e6
        self.assertAlmostEqual(cold[("t1", "b")], 0.05 + 9000 * write_minus_read)
        self.assertAlmostEqual(cold[("t2", "b")], 0.09)
        self.assertAlmostEqual(cold[("t1", "a")], 0.10)


if __name__ == "__main__":
    unittest.main()
