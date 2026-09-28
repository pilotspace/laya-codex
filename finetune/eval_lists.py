"""Compare model dirs on production-shaped lists (build_data.py): the validation split of the train repos and the
held-out repos' lists. Each model is scored with its own configured noul temperature, exactly as the daemon would
load it, and ranked by the production blend (listwise.summarize: lexical, blend w=0.5 over the first 16, model only).

    python3 finetune/eval_lists.py --models old=<laya-code dir> new=<candidate dir> --out <file.json>
"""
import argparse
import json
import os
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import listwise  # noqa: E402


def paired(a, b, n=10000, seed=0):
    d = np.asarray(a, float) - np.asarray(b, float)
    rng = np.random.default_rng(seed)
    bs = d[rng.integers(0, len(d), (n, len(d)))].mean(1)
    return {"mean_diff": round(float(d.mean()), 4), "ci95": [round(float(np.percentile(bs, 2.5)), 4),
                                                             round(float(np.percentile(bs, 97.5)), 4)]}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--models", nargs="+", required=True, help="name=dir")
    ap.add_argument("--heldout", nargs="*", default=["pilot-space"])
    ap.add_argument("--device", default=None)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()
    import torch
    from train import encode_lists, evaluate, load_base_model, noul_temperature, predict_lists
    device = torch.device(args.device or ("mps" if torch.backends.mps.is_available() else "cpu"))
    sets = {"val": listwise.load_lists("val")}
    for h in args.heldout:
        sets["heldout-" + h] = listwise.load_lists(names=[h], heldout=True)
    rep, per_list = {"sets": {k: len(v) for k, v in sets.items()}, "models": {}}, {}
    for spec in args.models:
        name, mdir = spec.split("=", 1)
        model, tok, cfg, _ = load_base_model(os.path.expanduser(mdir), device)
        T = noul_temperature(cfg)
        rep["models"][name] = {"dir": mdir, "T": T}
        for sname, lists in sets.items():
            enc = encode_lists(tok, lists)
            z = predict_lists(model, enc, device, tok.pad_token_id)
            rep["models"][name][sname] = evaluate(z, lists, enc, T=T)
            per_list[(name, sname)] = [listwise.list_metrics(l, listwise.blend_order(
                1 / (1 + np.exp(-(zz[:, 1] - zz[:, 0]) / T)))) for l, zz in zip(lists, z)]
            print(name, sname, json.dumps(rep["models"][name][sname]), flush=True)
        del model
        if device.type == "mps":
            torch.mps.empty_cache()
    names = [s.split("=", 1)[0] for s in args.models]
    if len(names) >= 2:
        a, b = names[-1], names[0]
        rep["paired_%s_minus_%s" % (a, b)] = {
            s: {k: paired([m[k] for m in per_list[(a, s)]], [m[k] for m in per_list[(b, s)]])
                for k in ("top2_gold", "file_r@1", "file_r@3", "mrr_pos")} for s in sets}
    json.dump(rep, open(args.out, "w"), indent=1)
    print(json.dumps({k: v for k, v in rep.items() if k.startswith("paired")}, indent=1))


if __name__ == "__main__":
    main()
