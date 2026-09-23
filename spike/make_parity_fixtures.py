"""Reference outputs from the official Laya Python implementation (CPU fp32) for Rust parity tests.

    python3 spike/make_parity_fixtures.py ~/.cache/laya-codex/models/laya-base fixtures/laya_parity.json
"""
import json
import sys

import numpy as np
import torch

model_dir, out = sys.argv[1], sys.argv[2]
sys.path.insert(0, model_dir)
from rl_agent_api import RLAgent  # noqa: E402
from rl_common import QTYPES, build_sequence, collate_items, temp_bucket  # noqa: E402

a = RLAgent(model_dir, device="cpu")
cases = [
    {"state": "file: src/storage/hash.rs (lines 10-30)\nfn hget(&self, key: &[u8]) -> Option<Bytes> {\n    self.map.get(key).cloned()\n}",
     "q": {"t": "noul", "ins": "Is this source code relevant to the software change: \"O(1) fast path for HGET\"?", "crit": None}},
    {"state": "def parse_config(path):\n    with open(path) as f:\n        return json.load(f)\n",
     "q": {"t": "choice", "ins": "How much of this code should the agent read?",
           "crit": {"tight": "only a few lines", "function": "the whole function", "block": "the surrounding block"}}},
    {"state": "fn main() { println!(\"hello\"); }\n" * 60,
     "q": {"t": "score", "ins": "Rate relevance to: add TLS support to the server",
           "crit": ["irrelevant", "somewhat", "relevant", "essential"]}},
]
items = []
for c in cases:
    ids, markers = build_sequence(a.tok, c["state"], c["q"], a.cfg["max_len"], a.cfg["head_max_len"])
    items.append({"ids": ids, "markers": markers, "qtype": QTYPES[c["q"]["t"]], "target": [0.0] * len(markers), "label": -1,
                  "episode": 0, "ep_step": 0, "ep_len": 1, "src": "fx"})
b = collate_items([items], a.tok.pad_token_id)
with torch.no_grad():
    logits, act = a.model(b["input_ids"], b["attention_mask"], b["marker_pos"], b["marker_mask"], b["qtype"])
res = []
for r, (c, it) in enumerate(zip(cases, items)):
    k, qt = len(it["markers"]), it["qtype"]
    T = a.temperature_by_options.get(temp_bucket(qt, k), a.temperature[qt])
    z = logits[r, :k].numpy() / T
    p = np.exp(z - z.max())
    p /= p.sum()
    res.append({"state": c["state"], "question": c["q"], "input_ids": it["ids"], "markers": it["markers"], "qtype": qt,
                "temperature": T, "logits": logits[r, :k].tolist(), "probs": p.tolist(),
                "act_probs": torch.softmax(act[r], -1).tolist()})
json.dump({"model": "convaiinnovations/laya", "max_len": a.cfg["max_len"], "head_max_len": a.cfg["head_max_len"],
           "cases": res}, open(out, "w"), indent=1)
print([(len(x["input_ids"]), x["markers"], [round(v, 4) for v in x["probs"]]) for x in res])
