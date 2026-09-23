"""Dump intermediate reference tensors from the official Laya Python implementation (CPU fp32).

Used to bisect parity failures of the Rust port: every stage of the forward pass is recorded
at a few token positions so a mismatch can be attributed to one module.

    python3 crates/laya-model/scripts/dump_intermediates.py \
        ~/.cache/laya-codex/models/laya-base fixtures/laya_parity.json tmp/laya_intermediates.json
"""
import json
import sys

import torch

model_dir, fixtures, out = sys.argv[1], sys.argv[2], sys.argv[3]
sys.path.insert(0, model_dir)
from rl_agent_api import RLAgent  # noqa: E402
from rl_common import collate_items  # noqa: E402

a = RLAgent(model_dir, device="cpu")
fx = json.load(open(fixtures))
case_idx = int(sys.argv[4]) if len(sys.argv) > 4 else 0
c = fx["cases"][case_idx]
item = {"ids": c["input_ids"], "markers": c["markers"], "qtype": c["qtype"], "target": [0.0] * len(c["markers"]),
        "label": -1, "episode": 0, "ep_step": 0, "ep_len": 1, "src": "fx"}
b = collate_items([[item]], a.tok.pad_token_id)
positions = sorted(set([0, 1, len(c["input_ids"]) // 2, len(c["input_ids"]) - 1] + c["markers"]))

rec = {"case": case_idx, "positions": positions, "stages": {}}
enc = a.model.encoder


def grab(name, t):
    rec["stages"][name] = t[0, positions].detach().float().tolist()


hooks = []
hooks.append(enc.embeddings.register_forward_hook(lambda m, i, o: grab("embeddings", o)))
for li, layer in enumerate(enc.layers):
    hooks.append(layer.register_forward_hook(lambda m, i, o, li=li: grab("layer.%d" % li, o[0] if isinstance(o, tuple) else o)))
hooks.append(enc.final_norm.register_forward_hook(lambda m, i, o: grab("final_norm", o)))
for hi, layer in enumerate(a.model.head.layers):
    hooks.append(layer.register_forward_hook(lambda m, i, o, hi=hi: grab("head.%d" % hi, o)))

with torch.no_grad():
    logits, act = a.model(b["input_ids"], b["attention_mask"], b["marker_pos"], b["marker_mask"], b["qtype"])
rec["logits"] = logits[0].tolist()
for h in hooks:
    h.remove()
json.dump(rec, open(out, "w"))
print("wrote", out, "stages:", list(rec["stages"].keys())[:3], "...", "logits", rec["logits"])
