"""Cache a teacher's raw noul logits over the training lists, once, for distillation (train.py --kd-weight).

The teacher (laya-code-r1) is scored at the view it was trained on (--window 128, its own max_len) over exactly
the lists train.py trains on (the first SCORE_TOP candidates of every training list with a positive among them).
Running r1 every step would cost a large-model forward per list; the cache costs one pass. The file is an .npz:
keys ("repo:sha"), offsets, float32 logits [n_candidates, 2] and a JSON meta (teacher dir, its weights' sha256,
window, noul temperature T, data dir). train.py checks that every training list has an entry with the same
number of candidates.

    python3 finetune/teacher.py --teacher ~/.cache/laya-codex/models/laya-code-r1 \
        --out ~/.cache/laya-codex/finetune/student-teacher/r1-w128.npz
"""
import argparse
import hashlib
import json
import os
import sys
import time

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import common  # noqa: E402


def list_key(lst):
    return "%s:%s" % (lst["repo"], lst["sha"])


class TeacherCache(dict):
    """{list key: raw logits [n, 2]} plus the meta it was built with."""

    def __init__(self, logits, meta):
        super().__init__(logits)
        self.meta = meta

    def lookup(self, lst):
        z = self[list_key(lst)]
        if len(z) != len(lst["candidates"]):
            raise ValueError("teacher scored %d candidates for %s, the list has %d" % (
                len(z), list_key(lst), len(lst["candidates"])))
        return z

    def check(self, lists):
        missing = [list_key(l) for l in lists if list_key(l) not in self]
        if missing:
            raise KeyError("teacher cache has no entry for %d of %d lists (e.g. %s)" % (
                len(missing), len(lists), missing[0]))
        for l in lists:
            self.lookup(l)


def save(path, logits, meta):
    keys = list(logits)
    sizes = [len(logits[k]) for k in keys]
    flat = np.concatenate([np.asarray(logits[k], np.float32) for k in keys]) if keys else np.zeros((0, 2), np.float32)
    tmp = path + ".tmp.npz"
    np.savez(tmp, keys=np.array(keys), offsets=np.cumsum([0] + sizes), logits=flat, meta=np.array(json.dumps(meta)))
    os.replace(tmp, path)


def load(path):
    with np.load(path, allow_pickle=False) as f:
        keys, off, flat, meta = list(f["keys"]), f["offsets"], f["logits"], json.loads(str(f["meta"]))
    return TeacherCache({str(k): flat[off[i]:off[i + 1]] for i, k in enumerate(keys)}, meta)


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--teacher", required=True, help="teacher model dir (laya-code layout)")
    ap.add_argument("--out", required=True)
    ap.add_argument("--data", default=None, help="dir of build_data.py lists (default WORK/data_v3)")
    ap.add_argument("--window", type=int, default=common.STATE_TOKENS, help="the teacher's state window")
    ap.add_argument("--device", default=None)
    ap.add_argument("--bs", type=int, default=48)
    args = ap.parse_args(argv)
    import torch
    from train import encode_lists, load_base_model, noul_temperature, predict_lists, training_lists
    device = torch.device(args.device or ("mps" if torch.backends.mps.is_available() else "cpu"))
    lists = training_lists(args.data)
    model, tok, cfg, _ = load_base_model(args.teacher, device)
    t0 = time.time()
    enc = encode_lists(tok, lists, window=args.window, max_len=cfg["max_len"])
    z = predict_lists(model, enc, device, tok.pad_token_id, bs=args.bs)
    secs = time.time() - t0
    meta = {"teacher": os.path.abspath(args.teacher),
            "model.safetensors_sha256": sha256(os.path.join(args.teacher, "model.safetensors")),
            "window": args.window, "max_len": cfg["max_len"], "T": float(noul_temperature(cfg)),
            "data": os.path.abspath(args.data or os.path.join(common.WORK, "data_v3")), "lists": len(lists),
            "candidates": int(sum(len(x) for x in z)), "secs": round(secs, 1)}
    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    save(args.out, {list_key(l): zz for l, zz in zip(lists, z)}, meta)
    print(json.dumps(meta), flush=True)


if __name__ == "__main__":
    main()
