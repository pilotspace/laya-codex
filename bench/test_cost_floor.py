"""Tests for the session cost floor: `python3 -m unittest bench/test_cost_floor.py`."""
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cost_floor as cf  # noqa: E402


def assistant(write, read, out=5):
    return {"type": "assistant", "message": {"usage": {"cache_creation_input_tokens": write, "input_tokens": 3,
                                                        "cache_read_input_tokens": read, "output_tokens": out}}}


def result(write, read, out, cost):
    return {"type": "result", "total_cost_usd": cost,
            "usage": {"cache_creation_input_tokens": write, "input_tokens": 3, "cache_read_input_tokens": read,
                      "output_tokens": out}}


SESSION = [assistant(5000, 4000), result(6000, 20000, 600, 0.06),
           assistant(700, 10000), result(900, 12000, 500, 0.04)]


class Session(unittest.TestCase):
    def test_first_call_write_is_claude_codes_fixed_context(self):
        s = cf.session(SESSION)
        self.assertEqual(s["first_write"], 5003)          # cache write + uncached input of the first call
        self.assertEqual(s["cache_write"], 6906)          # summed over the prompts' result usage
        self.assertEqual(s["output"], 1100)
        self.assertAlmostEqual(s["reported_usd"], 0.10)

    def test_usd_uses_list_prices(self):
        usd = cf.usd({"cache_write": 1_000_000, "cache_read": 1_000_000, "output": 1_000_000})
        self.assertAlmostEqual(usd, cf.PRICES["cache_write"] + cf.PRICES["cache_read"] + cf.PRICES["output"])


class Floor(unittest.TestCase):
    def test_floor_is_fixed_context_plus_answers_plus_minimum_turn_costs(self):
        f = cf.floor(first_write=5000, answer_tokens=1000, followup_write=500, min_reads=20_000)
        expected = (5000 + 500) * cf.PRICES["cache_write"] / 1e6 + 1000 * cf.PRICES["output"] / 1e6 \
            + 20_000 * cf.PRICES["cache_read"] / 1e6
        self.assertAlmostEqual(f, expected)


if __name__ == "__main__":
    unittest.main()
