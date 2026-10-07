"""Initialise the laya-code student: answerdotai/ModernBERT-base + a randomly initialised laya decision head, written
in the laya-code layout (the dir train.py --base, export.py --base and crates/laya-model load).

The encoder keeps its pretrained weights (the hub's MLM head is dropped); the decision head (2 transformer layers,
type_emb, scorer, act_head) is the reference DecisionModel's own initialisation under --seed. Tensors are F16 and
`temperature` F32, like laya-code, so the export keeps the dtypes the Rust port loads. max_len is the student's: at
least common.required_max_len(window), so the code window, not max_len, cuts the state. The temperatures start at
1.0: calibrate.py fits the noul one after training.

    hf download answerdotai/ModernBERT-base --local-dir <hf>
    python3 finetune/student.py --hf <hf> --template ~/.cache/laya-codex/models/laya-code-r1 \
        --out ~/.cache/laya-codex/finetune/student-init --max-len 704 --seed 13
"""
import argparse
import hashlib
import importlib.util
import json
import os
import shutil
import sys

import torch

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import common  # noqa: E402

ENCODER = "answerdotai/ModernBERT-base"
TOKENIZER_FILES = ("tokenizer.json", "tokenizer_config.json")
REFERENCE_FILES = ("rl_common.py", "rl_agent_api.py")


def _rl_common(template_dir):
    spec = importlib.util.spec_from_file_location("rl_common_template", os.path.join(template_dir, "rl_common.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def student_config(template_cfg, max_len, encoder=ENCODER):
    cfg = {k: v for k, v in template_cfg.items() if k not in ("finetune", "training")}
    cfg.update(encoder=encoder, model_name="laya-code-student", max_len=max_len)
    cfg["temperature"] = [1.0] * len(cfg["temperature"])
    cfg["temperature_by_options"] = {k: 1.0 for k in cfg.get("temperature_by_options", {})}
    return cfg


def make_student_dir(hf_dir, template_dir, out_dir, max_len=704, seed=13, window=384, encoder=ENCODER):
    """Write the student init dir; returns its provenance (also saved as student_init.json)."""
    if max_len < common.required_max_len(window):
        raise ValueError("max_len %d cuts a %d-token window (needs >= %d)" % (
            max_len, window, common.required_max_len(window)))
    from safetensors.torch import load_file, save_file
    rl = _rl_common(template_dir)
    cfg = student_config(json.load(open(os.path.join(template_dir, "rl_agent_config.json"))), max_len, encoder)
    torch.manual_seed(seed)
    model = rl.build_model(cfg, encoder_dir=hf_dir)
    src = load_file(os.path.join(hf_dir, "model.safetensors"))
    model.encoder.load_state_dict({k[len("model."):]: v for k, v in src.items() if k.startswith("model.")},
                                  strict=True)
    sd = {}
    for k, v in model.state_dict().items():
        v = v.detach().float() if k == "temperature" else v.detach().half()
        if not torch.isfinite(v).all():
            raise ValueError("non-finite values in %s" % k)
        sd[k] = v.contiguous()
    tmp = out_dir.rstrip("/") + ".tmp"
    shutil.rmtree(tmp, ignore_errors=True)
    os.makedirs(os.path.join(tmp, "encoder"))
    os.makedirs(os.path.join(tmp, "tokenizer"))
    shutil.copy2(os.path.join(hf_dir, "config.json"), os.path.join(tmp, "encoder", "config.json"))
    for f in TOKENIZER_FILES:
        shutil.copy2(os.path.join(hf_dir, f), os.path.join(tmp, "tokenizer", f))
    for f in REFERENCE_FILES:
        shutil.copy2(os.path.join(template_dir, f), os.path.join(tmp, f))
    json.dump(cfg, open(os.path.join(tmp, "rl_agent_config.json"), "w"), indent=2)
    save_file(sd, os.path.join(tmp, "model.safetensors"))
    prov = {"encoder": encoder, "hf_model.safetensors_sha256": sha256(os.path.join(hf_dir, "model.safetensors")),
            "template": os.path.abspath(template_dir), "seed": seed, "max_len": max_len, "window": window,
            "tensors": len(sd), "model.safetensors_sha256": sha256(os.path.join(tmp, "model.safetensors"))}
    json.dump(prov, open(os.path.join(tmp, "student_init.json"), "w"), indent=1)
    if os.path.exists(out_dir):
        shutil.rmtree(out_dir)
    os.replace(tmp, out_dir)
    return prov


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--hf", required=True, help="answerdotai/ModernBERT-base download (config.json, model.safetensors,"
                                                " tokenizer.json)")
    ap.add_argument("--template", default=common.BASE_MODEL, help="laya-code dir: rl_common.py, rl_agent_api.py and"
                                                                  " the agent config to start from")
    ap.add_argument("--out", required=True)
    ap.add_argument("--max-len", type=int, default=704)
    ap.add_argument("--window", type=int, default=384, help="largest state window the student must see uncut")
    ap.add_argument("--seed", type=int, default=13)
    args = ap.parse_args()
    print(json.dumps(make_student_dir(args.hf, args.template, os.path.expanduser(args.out), args.max_len, args.seed,
                                      args.window), indent=1))


if __name__ == "__main__":
    main()
