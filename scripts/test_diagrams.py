"""Tests for the animated README diagrams: `python3 -m unittest scripts/test_diagrams.py`."""
import os
import re
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import diagrams  # noqa: E402

NS = "{http://www.w3.org/2000/svg}"


def rendered():
    for name, fn in diagrams.DIAGRAMS:
        for theme, t in diagrams.THEMES.items():
            yield f"{name}-{theme}", fn(t)


class Rendering(unittest.TestCase):
    def test_every_diagram_is_well_formed_svg_in_both_themes(self):
        for label, doc in rendered():
            with self.subTest(label):
                root = ET.fromstring(doc)
                self.assertEqual(root.tag, NS + "svg")
                self.assertTrue(root.findtext(NS + "title"), "needs a <title> for screen readers")

    def test_no_scripts_or_external_references(self):
        # GitHub shows README SVGs through <img>: scripts and external files never load there.
        for label, doc in rendered():
            with self.subTest(label):
                self.assertNotIn("<script", doc)
                self.assertNotRegex(doc, r'(href|src)="(?!#)')

    def test_motion_stops_for_readers_who_ask_for_less(self):
        for label, doc in rendered():
            with self.subTest(label):
                self.assertIn("prefers-reduced-motion", doc)

    def test_the_static_frame_is_the_complete_picture(self):
        # Hidden starting states come only from keyframes, so with animation off nothing is missing.
        for label, doc in rendered():
            with self.subTest(label):
                self.assertNotRegex(doc, r'opacity="0(\.0*)?"')
                self.assertNotRegex(doc, r'width="0(\.0*)?"')

    def test_main_writes_light_and_dark_files(self):
        with tempfile.TemporaryDirectory() as out:
            diagrams.write_all(out)
            names = sorted(os.listdir(out))
        expected = sorted(f"diagram-{n}-{t}.svg" for n, _ in diagrams.DIAGRAMS for t in diagrams.THEMES)
        self.assertEqual(names, expected)


class SessionRace(unittest.TestCase):
    def test_bars_grow_at_one_speed_so_their_lengths_are_the_measured_times(self):
        s = diagrams.SESSION
        lanes = diagrams.session_lanes()
        stock, laya = lanes["stock"], lanes["laya"]
        self.assertAlmostEqual(laya["bar_w"] / stock["bar_w"], s["laya"]["wall_s"] / s["stock"]["wall_s"], places=3)
        self.assertAlmostEqual(laya["grow_s"] / stock["grow_s"], s["laya"]["wall_s"] / s["stock"]["wall_s"], places=3)

    def test_labels_carry_the_benchmark_numbers(self):
        doc = diagrams.session(diagrams.THEMES["light"])
        for needle in ("23.2 s", "18.7 s", "$0.081", "$0.069", "5.9 tool calls", "2.3 tool calls",
                       "turn 4", "52 of 60"):
            with self.subTest(needle):
                self.assertIn(needle, doc)

    def test_tool_call_chips_round_the_measured_means(self):
        lanes = diagrams.session_lanes()
        self.assertEqual(len(lanes["stock"]["calls"]), round(diagrams.SESSION["stock"]["tool_calls"]))
        self.assertEqual(len(lanes["laya"]["calls"]), round(diagrams.SESSION["laya"]["tool_calls"]))


class Rerank(unittest.TestCase):
    def test_candidate_bars_are_to_scale(self):
        bars = {b["n"]: b["w"] for b in diagrams.rerank_bars() if b.get("n")}
        self.assertAlmostEqual(bars[16] / bars[24], 16 / 24, places=3)
        self.assertAlmostEqual(bars[2] / bars[24], 2 / 24, places=3)

    def test_states_the_blend(self):
        doc = diagrams.rerank(diagrams.THEMES["light"])
        self.assertIn("0.5 × keyword rank + 0.5 × model probability", doc)


class Pipeline(unittest.TestCase):
    def test_names_every_stage(self):
        doc = diagrams.pipeline(diagrams.THEMES["light"])
        for needle in ("tree-sitter", "Moon", "24 candidates", "top 16", "9,500", "MCP search", "Claude Code"):
            with self.subTest(needle):
                self.assertIn(needle, doc)


if __name__ == "__main__":
    unittest.main()
