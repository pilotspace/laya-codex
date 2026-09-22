"""Export a fine-tuned checkpoint as a laya-base-compatible model dir (same keys, shapes AND dtypes).

laya-base stores 205 tensors as F16 plus `temperature` as F32; the export keeps exactly that, so the candle port
loads laya-code unchanged. Temperatures in rl_agent_config.json are written by calibrate.py afterwards.

    python3 finetune/export.py --ckpt ~/.cache/laya-codex/finetune/ckpt/best.pt --out ~/.cache/laya-codex/models/laya-code
    python3 finetune/export.py --verify ~/.cache/laya-codex/models/laya-code
"""
import argparse
import os
import shutil
import sys

import torch

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import common  # noqa: E402

COPY = ["encoder", "tokenizer", "rl_agent_api.py", "rl_common.py", "rl_agent_config.json"]


def merge_state(base, trained):
    """base state dict (export dtypes) + trained fp32 params -> new state dict with base keys/order/shapes/dtypes."""
    out = dict(base)
    for k, v in trained.items():
        if k not in base:
            raise KeyError("trained key not in base: %s" % k)
        if tuple(v.shape) != tuple(base[k].shape):
            raise ValueError("shape mismatch for %s: %s vs %s" % (k, tuple(v.shape), tuple(base[k].shape)))
        v = v.detach().float()
        if not torch.isfinite(v).all():
            raise ValueError("non-finite values in %s" % k)
        if base[k].dtype == torch.float16 and v.abs().max() > 65000:
            raise ValueError("%s overflows fp16" % k)
        out[k] = v.to(base[k].dtype).contiguous()
    return {k: out[k] for k in base}


def check_same_layout(base_dir, out_dir):
    from safetensors import safe_open
    a = safe_open(os.path.join(base_dir, "model.safetensors"), "pt")
    b = safe_open(os.path.join(out_dir, "model.safetensors"), "pt")
    ka, kb = list(a.keys()), list(b.keys())
    assert sorted(ka) == sorted(kb), "key sets differ"
    for k in ka:
        sa, sb = a.get_slice(k), b.get_slice(k)
        assert sa.get_shape() == sb.get_shape(), k
        assert sa.get_dtype() == sb.get_dtype(), k
    for f in COPY + ["model.safetensors"]:
        assert os.path.exists(os.path.join(out_dir, f)), f
    return len(ka), sorted({a.get_slice(k).get_dtype() for k in ka})


def verify(model_dir, device="cpu"):
    """Load with the reference RLAgent shipped in the exported dir and run one noul query."""
    import importlib
    sys.path.insert(0, model_dir)
    for m in ("rl_agent_api", "rl_common"):
        sys.modules.pop(m, None)
    api = importlib.import_module("rl_agent_api")
    agent = api.RLAgent(model_dir, device=device)
    state = "file: src/cache.rs (lines 1-12)\npub fn evict_lru(&mut self) {\n    while self.len > self.cap {\n        self.pop_tail();\n    }\n}"
    r = agent.system_one(state, {"rel": {"type": "noul", "instructions": common.QUESTIONS[0].format(
        task="fix LRU cache eviction when capacity is exceeded")}})
    return r["answers"]["rel"]["noul"]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default=common.BASE_MODEL)
    ap.add_argument("--ckpt", default=os.path.join(common.WORK, "ckpt", "best.pt"))
    ap.add_argument("--out", default=os.path.expanduser("~/.cache/laya-codex/models/laya-code"))
    ap.add_argument("--verify", default=None, help="only verify an exported dir")
    args = ap.parse_args()
    if args.verify:
        print("layout", check_same_layout(args.base, args.verify))
        print("noul(relevant example) =", verify(args.verify))
        return
    from safetensors.torch import load_file, save_file
    base = load_file(os.path.join(args.base, "model.safetensors"))
    ck = torch.load(args.ckpt, map_location="cpu", weights_only=False)
    merged = merge_state(base, ck["params"])
    tmp = args.out + ".tmp"
    shutil.rmtree(tmp, ignore_errors=True)
    os.makedirs(tmp)
    for f in COPY:
        src = os.path.join(args.base, f)
        (shutil.copytree if os.path.isdir(src) else shutil.copy2)(src, os.path.join(tmp, f))
    save_file(merged, os.path.join(tmp, "model.safetensors"))
    if os.path.exists(args.out):
        shutil.rmtree(args.out)
    os.replace(tmp, args.out)
    n, dtypes = check_same_layout(args.base, args.out)
    print("exported %s: %d tensors, dtypes %s, from step %s (%d trained tensors)" % (
        args.out, n, dtypes, ck.get("step"), len(ck["params"])))


if __name__ == "__main__":
    main()
