"""Tests for the README benchmark charts: `python3 -m unittest scripts/test_charts.py`."""
import json
import os
import sys
import unittest
import xml.etree.ElementTree as ET

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import charts  # noqa: E402

NS = "{http://www.w3.org/2000/svg}"
HEADLINE = os.path.join(HERE, "..", "bench", "results", "headline-v13.json")


def text_boxes(root):
    """Approximate bounding boxes of every <text>: width from character count, height from size."""
    boxes = []
    for el in root.iter(NS + "text"):
        s = "".join(el.itertext())
        size = float(el.get("font-size", 12))
        w = len(s) * size * 0.56
        x, y = float(el.get("x")), float(el.get("y"))
        anchor = el.get("text-anchor", "start")
        x0 = x - w if anchor == "end" else x - w / 2 if anchor == "middle" else x
        boxes.append((s, x0, y - size * 0.78, x0 + w, y + size * 0.22))
    return boxes


def segments(root):
    """Every line segment of the plotted step lines (polylines)."""
    out = []
    for el in root.iter(NS + "polyline"):
        pts = [tuple(map(float, p.split(","))) for p in el.get("points").split()]
        out += list(zip(pts, pts[1:]))
    return out


def hits(box, seg, pad=2.0):
    """Does an axis-aligned segment (step lines only have those) cross the padded box?"""
    _, x0, y0, x1, y1 = box
    (ax, ay), (bx, by) = seg
    if ax == bx:
        return x0 - pad <= ax <= x1 + pad and min(ay, by) <= y1 + pad and max(ay, by) >= y0 - pad
    return y0 - pad <= ay <= y1 + pad and min(ax, bx) <= x1 + pad and max(ax, bx) >= x0 - pad


def overlap(a, b):
    return a[1] < b[3] and b[1] < a[3] and a[2] < b[4] and b[2] < a[4]


class Journey(unittest.TestCase):
    def setUp(self):
        self.d = json.load(open(HEADLINE))

    def test_no_label_sits_on_a_plotted_line(self):
        for name, t in charts.THEMES.items():
            root = ET.fromstring(charts.journey(self.d, t))
            for box in text_boxes(root):
                for seg in segments(root):
                    with self.subTest(theme=name, label=box[0]):
                        self.assertFalse(hits(box, seg), f"'{box[0]}' crosses a line at {seg}")

    def test_no_two_labels_overlap(self):
        root = ET.fromstring(charts.journey(self.d, charts.THEMES["light"]))
        boxes = text_boxes(root)
        for i, a in enumerate(boxes):
            for b in boxes[i + 1:]:
                with self.subTest(a=a[0], b=b[0]):
                    self.assertFalse(overlap(a, b))

    def test_every_label_fits_inside_the_image(self):
        root = ET.fromstring(charts.journey(self.d, charts.THEMES["light"]))
        w, h = float(root.get("width")), float(root.get("height"))
        for box in text_boxes(root):
            with self.subTest(label=box[0]):
                self.assertGreaterEqual(box[1], 0)
                self.assertLessEqual(box[3], w)
                self.assertLessEqual(box[4], h)

    def test_keeps_a_legend_and_labels_each_line(self):
        doc = charts.journey(self.d, charts.THEMES["light"])
        for needle in ("median turn 0", "median never", "median 4", "52 of 60 tasks"):
            with self.subTest(needle):
                self.assertIn(needle, doc)


if __name__ == "__main__":
    unittest.main()
