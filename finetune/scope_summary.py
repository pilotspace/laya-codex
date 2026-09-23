"""Fold the text baseline, the zero-shot ranking and the fine-tune status into spike/results/scope_eval.json.

    python3 finetune/scope_summary.py --out spike/results/scope_eval.json
"""
import argparse
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from scope_eval import DATA  # noqa: E402


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    args = ap.parse_args()
    rep = json.load(open(args.out))
    rows = []
    for model, ws in rep["zero_shot"].items():
        for w, sets in ws.items():
            f1 = [s["macro_f1"] for s in sets.values()]
            acc = [s["accuracy"] for s in sets.values()]
            rho = [s["spearman_expected_idx_vs_n_src"] for s in sets.values()]
            rows.append({"model": model, "wording": w, "macro_f1_mean": round(sum(f1) / len(f1), 4),
                         "accuracy_mean": round(sum(acc) / len(acc), 4), "rho_eidx_vs_nsrc_mean": round(sum(rho) / len(rho), 4),
                         "per_set": {k: {"macro_f1": v["macro_f1"], "accuracy": v["accuracy"],
                                         "rho_eidx_vs_nsrc": v["spearman_expected_idx_vs_n_src"]} for k, v in sets.items()}})
    rows.sort(key=lambda r: -r["macro_f1_mean"])
    rep["zero_shot_ranking"] = rows
    rep["text_baseline_bow_logreg"] = json.load(open(os.path.join(DATA, "text_baseline.json")))
    best = rep["zero_shot"][rows[0]["model"]][rows[0]["wording"]]
    rep["label_rule_reference"] = {s: best[s]["spearman_true_idx_vs_n_src"] for s in rows[0]["per_set"]}
    rep["finetune"] = {
        "status": "not run: stopped by the coordinator after ~25 updates (no checkpoint kept, laya-code-v2 not exported); "
                  "oracle-scope replay showed scope barely changes what is loaded",
        "go_threshold_macro_f1": 0.45,
        "best_zero_shot_macro_f1": rows[0]["macro_f1_mean"],
        "measured_throughput": "10.9 s per update of 32 sequences (16 noul replay + 16 scope), M4 Pro MPS fp32, top-8 layers",
        "estimate": "1 epoch of the 9,359 scope train commits = ~585 updates = ~1.8 h; 2 epochs ~3.5 h (shared GPU)"}
    json.dump(rep, open(args.out, "w"), indent=1)
    for r in rows:
        print("%-11s %-10s macroF1 %.3f acc %.3f rho %.3f" % (r["model"], r["wording"], r["macro_f1_mean"], r["accuracy_mean"],
                                                               r["rho_eidx_vs_nsrc_mean"]))
    print("label rule rho(true idx, n_src):", rep["label_rule_reference"])


if __name__ == "__main__":
    main()
