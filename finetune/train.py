"""Fine-tune laya-code as the production reranker on production-shaped lists (build_data.py).

- Model = reference DecisionModel (rl_common.build_model), warm-started from a full model dir (--base, e.g. the
  current laya-code); same keys, shapes and input format, so the Rust scorer and export stay unchanged.
- Input = exactly what the Rust scorer builds: the primary question over the retriever's task focus, the state
  cut to 128 tokens (common.encode_ids / common.state_ids).
- One micro-batch = one candidate list (the top 24 lexical candidates of one fixed commit).
- Loss = log loss on the 2 noul logits vs the soft label (strictly proper, feeds calibration) + --list-weight x
  listwise softmax cross-entropy of the margins within the list (the ranking the production blend consumes).
- Trainable: top N encoder layers + final norm + decision head + type_emb + scorer; AdamW with two LR groups,
  warmup + linear decay (by steps or a wall-clock budget), grad clip, atomic resumable checkpoints.
- Validation (every --eval-every updates): NLL, AUROC and the production blend simulation (listwise.summarize,
  temperature fitted on the monitor lists); the best checkpoint by blend top2_gold (gold files in the first two chunks), then MRR.

- Student (finetune/student.py): --base is a ModernBERT-base init dir with a longer max_len; --windows (or
  --window-range) draws the state window per list, so one model serves 128 to 384 code tokens; validation runs at
  every --val-windows. --kd-weight adds listwise.kd_loss against a teacher's cached logits (finetune/teacher.py).

    python3 finetune/train.py --base ~/.cache/laya-codex/models/laya-code-v1 --top-layers 12 --epochs 2
    python3 finetune/train.py --base ... --bench              # throughput probe
    python3 finetune/train.py --base <student-init> --top-layers 22 --windows 128 256 384 --val-windows 128 256 384 \
        --kd-weight 1 --teacher-cache <r1-w128.npz> --grad-ckpt
"""
import argparse
import json
import math
import os
import random
import resource
import sys
import time

import numpy as np
import torch
import torch.nn.functional as F

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import common  # noqa: E402
import listwise  # noqa: E402
from rl_common import QTYPES, auroc, build_model, load_cfg  # noqa: E402

CKPT_DIR = os.environ.get("LAYA_CODEX_FT_CKPT", os.path.join(common.WORK, "ckpt"))


def load_base_model(model_dir, device):
    from safetensors.torch import load_file
    from transformers import AutoTokenizer
    cfg = load_cfg(os.path.join(model_dir, "rl_agent_config.json"))
    tok = AutoTokenizer.from_pretrained(os.path.join(model_dir, "tokenizer"))
    model = build_model(cfg, encoder_dir=os.path.join(model_dir, "encoder"))
    sd = load_file(os.path.join(model_dir, "model.safetensors"))
    model.load_state_dict(sd, strict=True)
    model.encoder.config.reference_compile = False
    model = model.float().to(device)
    return model, tok, cfg, None  # base state dict not kept: RAM is shared with the MPS pool


def enable_checkpointing(model):
    """Activation checkpointing for the encoder layers and the decision head (memory << compute on a shared 24 GB box)."""
    model.encoder.gradient_checkpointing_enable(gradient_checkpointing_kwargs={"use_reentrant": False})
    model.head_checkpointing = True


def set_trainable(model, top_layers):
    for p in model.parameters():
        p.requires_grad_(False)
    layers = model.encoder.layers
    enc_params = []
    for l in layers[len(layers) - top_layers:]:
        enc_params += list(l.parameters())
    enc_params += list(model.encoder.final_norm.parameters())
    head_params = list(model.head.parameters()) + list(model.type_emb.parameters()) + list(model.scorer.parameters())
    for p in enc_params + head_params:
        p.requires_grad_(True)
    return enc_params, head_params


def trainable_names(model):
    return [n for n, p in model.named_parameters() if p.requires_grad]


def noul_temperature(cfg):
    return cfg.get("temperature_by_options", {}).get("noul:2", cfg["temperature"][2])


# ----------------------------------------------------------------------------- data
def encode_lists(tok, lists, limit=None, window=common.STATE_TOKENS, max_len=common.MAX_LEN):
    """[(seqs [(ids, markers)], labels np.array)] per list; `limit` keeps the first candidates only (production
    scores the first listwise.SCORE_TOP). `window` / `max_len`: LAYA_CODEX_STATE_TOKENS and the model's max_len."""
    out = []
    for l in lists:
        l = dict(l, candidates=l["candidates"][:limit]) if limit else l
        out.append((listwise.encode_list(tok, l, window=window, max_len=max_len), labels_of(l)))
    return out


def labels_of(lst):
    return np.array([c["label"] for c in lst["candidates"]], np.float32)


def _head(ls):
    return [dict(l, candidates=l["candidates"][:listwise.SCORE_TOP]) for l in ls]


def training_lists(data_dir=None):
    """What production scores (the first SCORE_TOP candidates) of every training list with a positive among them."""
    return [l for l in _head(listwise.load_lists("train", data_dir=data_dir)) if any(c["label"] > 0 for c in l["candidates"])]


def validation_lists(data_dir=None, n=None):
    """The validation monitor: the same first SCORE_TOP of a seeded sample of the validation lists (the blend leaves
    the tail in lexical order)."""
    all_val = _head(listwise.load_lists("val", data_dir=data_dir))
    return random.Random(7).sample(all_val, min(n or len(all_val), len(all_val)))


def draw_window(rng, windows=None, window_range=None):
    """The state window of one training list: uniform over `windows`, or a uniform integer in `window_range`."""
    if window_range:
        return rng.randint(window_range[0], window_range[1])
    return windows[rng.randrange(len(windows))]


def window_plan(windows, window_range, max_len):
    """Validate the window settings against the model's max_len: (windows, window_range, largest window)."""
    if window_range:
        lo, hi = window_range
        if not 0 < lo <= hi:
            raise ValueError("bad --window-range %s" % (window_range,))
        windows, wmax = None, hi
    else:
        windows = list(windows or [common.STATE_TOKENS])
        if min(windows) <= 0:
            raise ValueError("bad --windows %s" % windows)
        wmax = max(windows)
    if common.required_max_len(wmax) > max_len:
        raise ValueError("a %d-token window needs max_len >= %d, the model has %d (the window would be cut)" % (
            wmax, common.required_max_len(wmax), max_len))
    return windows, (tuple(window_range) if window_range else None), wmax


def collate(seqs, pad_id):
    n, L = len(seqs), max(len(s[0]) for s in seqs)
    ids = torch.full((n, L), pad_id, dtype=torch.long)
    att = torch.zeros((n, L), dtype=torch.long)
    mpos = torch.zeros((n, 2), dtype=torch.long)
    for i, (s, m) in enumerate(seqs):
        ids[i, :len(s)] = torch.tensor(s)
        att[i, :len(s)] = 1
        mpos[i] = torch.tensor(m)
    return {"input_ids": ids, "attention_mask": att, "marker_pos": mpos, "marker_mask": torch.ones((n, 2), dtype=torch.bool),
            "qtype": torch.full((n,), QTYPES["noul"], dtype=torch.long)}


def forward_logits(model, b, device, amp=False):
    with torch.autocast(device_type=device.type, dtype=torch.bfloat16, enabled=amp):
        logits, _ = model(b["input_ids"].to(device), b["attention_mask"].to(device), b["marker_pos"].to(device),
                          b["marker_mask"].to(device), b["qtype"].to(device))
    return logits.float()[:, :2]


def noul_loss(logits2, y):
    target = torch.stack([1 - y, y], -1)
    return -(target * F.log_softmax(logits2, -1)).sum(-1).mean()


@torch.no_grad()
def predict_lists(model, enc, device, pad_id, bs=48):
    """Raw (T=1) noul logits per list [n_i, 2]."""
    was = model.training
    model.train(False)
    flat = [s for seqs, _ in enc for s in seqs]
    order = sorted(range(len(flat)), key=lambda i: len(flat[i][0]))  # length-sorted: less padding
    out = np.zeros((len(flat), 2), np.float32)
    for s in range(0, len(order), bs):
        idx = order[s:s + bs]
        out[idx] = forward_logits(model, collate([flat[i] for i in idx], pad_id), device).cpu().numpy()
    model.train(was)
    res, k = [], 0
    for seqs, _ in enc:
        res.append(out[k:k + len(seqs)])
        k += len(seqs)
    return res


def fit_T(logits_lists, enc):
    from calibrate import fit_temperature
    return fit_temperature(np.concatenate(logits_lists), np.concatenate([lab for _, lab in enc]))


def probs_T(logits, T):
    z = np.asarray(logits, np.float64) / T
    return 1.0 / (1.0 + np.exp(-(z[:, 1] - z[:, 0])))


def evaluate(logits_lists, lists, enc, T=None):
    """NLL / AUROC at T and the blend simulation. T=None fits the temperature on these lists first."""
    T = fit_T(logits_lists, enc) if T is None else T
    y = np.concatenate([lab for _, lab in enc])
    p = probs_T(np.concatenate(logits_lists), T)
    pc = np.clip(p, 1e-9, 1 - 1e-9)
    hard = (y == 0) | (y == 1)
    m = {"T": round(float(T), 4), "nll": round(float(-(y * np.log(pc) + (1 - y) * np.log(1 - pc)).mean()), 4),
         "auroc_hunk_vs_other": round(auroc(p[hard], y[hard].astype(int)), 4),
         "auroc_any_vs_other": round(auroc(p, (y > 0).astype(int)), 4),
         "mean_p": round(float(p.mean()), 4), "base_rate": round(float(y.mean()), 4)}
    m.update(listwise.summarize(lists, [probs_T(z, T) for z in logits_lists]))
    return m


def select_key(m):
    """Checkpoint selection: blend top2_gold then MRR; for per-window validation ({window: m}) their means."""
    if "blend" in m:
        return (m["blend"]["top2_gold"], m["blend"]["mrr_pos"])
    return (round(float(np.mean([v["blend"]["top2_gold"] for v in m.values()])), 4),
            round(float(np.mean([v["blend"]["mrr_pos"] for v in m.values()])), 4))


def validate(model, tok, val_lists, val_sids, windows, max_len, device, T=None):
    """{str(window): evaluate(...)} with the temperature fitted per window (T=None) or fixed."""
    out = {}
    for w in windows:
        enc = [(listwise.build_list(tok, l, s, window=w, max_len=max_len), labels_of(l))
               for l, s in zip(val_lists, val_sids)]
        out[str(w)] = evaluate(predict_lists(model, enc, device, tok.pad_token_id), val_lists, enc, T=T)
        if device.type == "mps":
            torch.mps.empty_cache()
    return out


def memory_now(device):
    """Peak RSS of this process (ru_maxrss: bytes on macOS) and the MPS driver allocation, in GB."""
    rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / (2**30 if sys.platform == "darwin" else 2**20)
    mps = torch.mps.driver_allocated_memory() / 2**30 if device.type == "mps" else 0.0
    return {"peak_rss_gb": round(rss, 2), "mps_gb": round(mps, 2)}


def atomic_save(obj, path):
    tmp = path + ".tmp"
    torch.save(obj, tmp)
    os.replace(tmp, path)


def trainable_state(model):
    return {n: p.detach().cpu().clone() for n, p in model.named_parameters() if p.requires_grad}


def list_loss(model, seqs, labels, device, pad_id, list_weight, amp=False, teacher=None, kd_weight=0.0, kd_T=1.0,
              kd_list_weight=1.0):
    """Gold loss (noul log loss + list_weight x listwise) + kd_weight x listwise.kd_loss against `teacher` logits."""
    logits = forward_logits(model, collate(seqs, pad_id), device, amp)
    y = torch.tensor(labels, device=device)
    loss = noul_loss(logits, y)
    if list_weight:
        loss = loss + list_weight * listwise.listwise_loss(logits[:, 1] - logits[:, 0], y)
    if kd_weight:
        loss = loss + kd_weight * listwise.kd_loss(logits, teacher, kd_T, kd_list_weight)
    return loss


def bench(args, device):
    model, tok, cfg, _ = load_base_model(args.base, device)
    enc = encode_lists(tok, listwise.load_lists("train")[:64], limit=listwise.SCORE_TOP)
    for n in args.bench_layers:
        enc_p, head_p = set_trainable(model, n)
        if args.grad_ckpt:
            enable_checkpointing(model)
        opt = torch.optim.AdamW([{"params": enc_p, "lr": 1e-7}, {"params": head_p, "lr": 1e-7}])
        model.train()
        times = []
        for i in range(6):
            seqs, lab = enc[i]
            t0 = time.perf_counter()
            loss = list_loss(model, seqs, lab, device, tok.pad_token_id, args.list_weight)
            loss.backward()
            opt.step()
            opt.zero_grad(set_to_none=True)
            if device.type == "mps":
                torch.mps.synchronize()
            times.append((time.perf_counter() - t0, len(seqs), max(len(s[0]) for s in seqs)))
        mem = torch.mps.driver_allocated_memory() / 2**30 if device.type == "mps" else 0
        med = float(np.median([t for t, _, _ in times[1:]]))
        seqs_per = float(np.mean([n_ for _, n_, _ in times[1:]]))
        print("top=%d  %.2fs/list (%.1f seq/s, L~%d) loss %.3f mem %.1fGB" % (
            n, med, seqs_per / med, int(np.mean([l for _, _, l in times])), loss.item(), mem), flush=True)
        del opt
    t0 = time.perf_counter()
    predict_lists(model, enc[:20], device, tok.pad_token_id)
    if device.type == "mps":
        torch.mps.synchronize()
    print("inference %.1f seq/s" % (sum(len(s) for s, _ in enc[:20]) / (time.perf_counter() - t0)), flush=True)


def train(args, device):
    torch.manual_seed(args.seed)
    ckpt_dir = args.ckpt_dir
    os.makedirs(ckpt_dir, exist_ok=True)
    model, tok, cfg, _ = load_base_model(args.base, device)
    max_len = cfg["max_len"]
    windows, window_range, wmax = window_plan(args.windows, args.window_range, max_len)
    val_windows = list(args.val_windows or [wmax])
    window_plan(val_windows, None, max_len)
    enc_p, head_p = set_trainable(model, args.top_layers)
    if args.grad_ckpt:
        enable_checkpointing(model)
    print("trainable params %.1fM (top %d layers + head); max_len %d, windows %s, validation at %s" % (
        sum(p.numel() for p in enc_p + head_p) / 1e6, args.top_layers, max_len,
        window_range and "uniform %d-%d" % window_range or windows, val_windows), flush=True)
    opt = torch.optim.AdamW([{"params": enc_p, "lr": args.lr_enc, "weight_decay": 0.01},
                             {"params": head_p, "lr": args.lr_head, "weight_decay": 0.01}])
    base_lrs = [args.lr_enc, args.lr_head]
    t_enc = time.time()
    train_lists = training_lists(args.data)
    val_lists = validation_lists(args.data, args.val_n)
    kd, kd_T = None, 1.0
    if args.kd_weight:
        if not args.teacher_cache:
            raise ValueError("--kd-weight needs --teacher-cache (finetune/teacher.py)")
        import teacher
        kd = teacher.load(args.teacher_cache)
        kd.check(train_lists)  # every training list, with the same candidates
        kd_T = float(kd.meta["T"])
        print("distillation: weight %.2f (list %.2f) from %s at window %s, T %.4f" % (
            args.kd_weight, args.kd_list_weight, kd.meta.get("teacher"), kd.meta.get("window"), kd_T), flush=True)
    # state tokens cut once at the largest window; each step slices the drawn window (no re-tokenization)
    train_sids = [listwise.list_state_ids(tok, l, cap=wmax) for l in train_lists]
    train_lab = [labels_of(l) for l in train_lists]
    val_sids = [listwise.list_state_ids(tok, l, cap=max(val_windows)) for l in val_lists]
    total_steps = args.steps or math.ceil(args.epochs * len(train_lists) / args.lists_per_step)
    print("train lists %d, val lists %d, %d updates of %d lists; encoded in %.0fs" % (
        len(train_lists), len(val_lists), total_steps, args.lists_per_step, time.time() - t_enc), flush=True)

    def progress(step, secs):
        return max(step / total_steps, secs / (args.max_hours * 3600) if args.max_hours else 0.0)

    def lr_scale(step, secs):
        if step < args.warmup:
            return (step + 1) / args.warmup
        return max(0.05, 1.0 - 0.95 * progress(step, secs))

    rng = random.Random(args.seed + 1)
    wrng = random.Random(args.seed + 2)
    order, epoch = [], 0
    step, log, best, train_secs = 0, [], None, 0.0
    seen, list_secs = {}, {}
    last = os.path.join(ckpt_dir, "last.pt")
    if args.resume and os.path.exists(last):
        ck = torch.load(last, map_location="cpu", weights_only=False)
        with torch.no_grad():
            for n, p in model.named_parameters():
                if n in ck["params"]:
                    p.copy_(ck["params"][n].to(device))
        opt.load_state_dict(ck["opt"])
        rng.setstate(ck["rng"])
        if "wrng" in ck:
            wrng.setstate(ck["wrng"])
        order, epoch = ck["order"], ck["epoch"]
        step, log, best, train_secs = ck["step"], ck["log"], ck.get("best"), ck.get("train_secs", 0.0)
        seen = {int(k): v for k, v in ck.get("windows_seen", {}).items()}
        list_secs = {int(k): v for k, v in ck.get("list_secs", {}).items()}
        print("resumed at step %d" % step, flush=True)
    else:
        m0 = validate(model, tok, val_lists, val_sids, val_windows, max_len, device, T=noul_temperature(cfg))
        m0f = validate(model, tok, val_lists, val_sids, val_windows, max_len, device)
        log.append({"step": 0, "val_base_T": m0, "val": m0f, "mem": memory_now(device)})
        print("step 0 val (base T) %s" % json.dumps(m0), flush=True)
        print("step 0 val (fitted T) %s" % json.dumps(m0f), flush=True)
    model.train()
    t0, ema, finished = time.time(), None, progress(step, train_secs) >= 1.0
    while not finished:
        ts = time.time()
        for g, lr in zip(opt.param_groups, base_lrs):
            g["lr"] = lr * lr_scale(step, train_secs)
        tot = 0.0
        for _ in range(args.lists_per_step):
            if not order:
                order = list(range(len(train_lists)))
                rng.shuffle(order)
                epoch += 1
            i = order.pop()
            w = draw_window(wrng, windows, window_range)
            tl = time.perf_counter()
            seqs = listwise.build_list(tok, train_lists[i], train_sids[i], window=w, max_len=max_len)
            loss = list_loss(model, seqs, train_lab[i], device, tok.pad_token_id, args.list_weight,
                             teacher=kd.lookup(train_lists[i]) if kd is not None else None,
                             kd_weight=args.kd_weight, kd_T=kd_T, kd_list_weight=args.kd_list_weight) / args.lists_per_step
            loss.backward()
            tot += loss.item()  # syncs the device: the list time below covers forward + backward
            seen[w] = seen.get(w, 0) + 1
            list_secs[w] = list_secs.get(w, 0.0) + time.perf_counter() - tl
        if not math.isfinite(tot):
            print("non-finite loss at step %d; skipping update" % step, flush=True)
            opt.zero_grad(set_to_none=True)
            step += 1
            continue
        torch.nn.utils.clip_grad_norm_(enc_p + head_p, 1.0)
        opt.step()
        opt.zero_grad(set_to_none=True)
        step += 1
        train_secs += time.time() - ts
        finished = progress(step, train_secs) >= 1.0
        ema = tot if ema is None else 0.98 * ema + 0.02 * tot
        if step % 25 == 0:
            el = time.time() - t0
            per_w = {w: round(list_secs[w] / seen[w], 2) for w in sorted(seen)}
            print("step %d loss %.4f (ema %.4f) lr %.2e | %.1fs/step progress %.3f epoch %d | s/list by window %s %s" % (
                step, tot, ema, opt.param_groups[0]["lr"], el / 25, progress(step, train_secs), epoch,
                json.dumps(per_w), json.dumps(memory_now(device))), flush=True)
            t0 = time.time()
        if step % args.eval_every == 0 or finished:
            if device.type == "mps":
                torch.mps.empty_cache()
            m = validate(model, tok, val_lists, val_sids, val_windows, max_len, device)
            log.append({"step": step, "train_loss_ema": round(ema, 4), "val": m, "mem": memory_now(device),
                        "train_secs": round(train_secs, 1)})
            print("step %d val %s" % (step, json.dumps(m)), flush=True)
            if best is None or select_key(m) > tuple(best["key"]):
                best = {"step": step, "key": list(select_key(m)), "val": m}
                atomic_save({"params": trainable_state(model), "step": step, "val": m, "top_layers": args.top_layers},
                            os.path.join(ckpt_dir, "best.pt"))
            t0 = time.time()
        if step % args.ckpt_every == 0 or finished:
            atomic_save({"params": trainable_state(model), "opt": opt.state_dict(), "rng": rng.getstate(),
                         "wrng": wrng.getstate(), "order": order, "epoch": epoch, "step": step, "log": log,
                         "best": best, "args": vars(args), "train_secs": train_secs, "windows_seen": seen,
                         "list_secs": list_secs}, last)
    summary = {"log": log, "best": best, "args": vars(args), "train_secs": train_secs, "max_len": max_len,
               "windows": {"set": windows, "range": window_range, "validation": val_windows},
               "windows_seen": {str(k): v for k, v in sorted(seen.items())},
               "list_secs_by_window": {str(k): round(list_secs[k] / seen[k], 3) for k in sorted(seen)},
               "kd": {"weight": args.kd_weight, "list_weight": args.kd_list_weight,
                      "teacher": kd.meta.get("teacher") if kd is not None else None,
                      "teacher_window": kd.meta.get("window") if kd is not None else None, "T": kd_T}}
    json.dump(summary, open(os.path.join(ckpt_dir, "train_log.json"), "w"), indent=1)
    print("done. best %s" % json.dumps(best), flush=True)


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default=common.BASE_MODEL, help="full model dir to warm start from")
    ap.add_argument("--device", default="mps" if torch.backends.mps.is_available() else "cpu")
    ap.add_argument("--top-layers", type=int, default=12)
    ap.add_argument("--epochs", type=float, default=2.0)
    ap.add_argument("--steps", type=int, default=0, help="updates (0 = from --epochs)")
    ap.add_argument("--lists-per-step", type=int, default=2)
    ap.add_argument("--lr-enc", type=float, default=2e-5)
    ap.add_argument("--lr-head", type=float, default=1e-4)
    ap.add_argument("--warmup", type=int, default=50)
    ap.add_argument("--list-weight", type=float, default=1.0)
    ap.add_argument("--eval-every", type=int, default=250)
    ap.add_argument("--ckpt-every", type=int, default=50)
    ap.add_argument("--val-n", type=int, default=600)
    ap.add_argument("--seed", type=int, default=13)
    ap.add_argument("--resume", action="store_true")
    ap.add_argument("--grad-ckpt", action="store_true")
    ap.add_argument("--max-hours", type=float, default=0.0, help="wall-clock budget for the optimisation loop")
    ap.add_argument("--bench", action="store_true")
    ap.add_argument("--bench-layers", type=int, nargs="*", default=[8, 12, 16])
    ap.add_argument("--data", default=None, help="dir of build_data.py lists (default WORK/data_v3)")
    ap.add_argument("--ckpt-dir", default=CKPT_DIR, help="checkpoints (default $LAYA_CODEX_FT_CKPT or WORK/ckpt)")
    ap.add_argument("--windows", type=int, nargs="+", default=None,
                    help="state windows drawn per training list (default %d, r1's view)" % common.STATE_TOKENS)
    ap.add_argument("--window-range", type=int, nargs=2, default=None, metavar=("LO", "HI"),
                    help="draw the window uniformly from LO..HI instead of --windows")
    ap.add_argument("--val-windows", type=int, nargs="+", default=None, help="validation windows (default: the largest)")
    ap.add_argument("--kd-weight", type=float, default=0.0, help="weight of the distillation term (0 = off)")
    ap.add_argument("--kd-list-weight", type=float, default=1.0, help="listwise part of the distillation term")
    ap.add_argument("--teacher-cache", default=None, help="teacher logits from finetune/teacher.py")
    args = ap.parse_args(argv)
    device = torch.device(args.device)
    if args.bench:
        bench(args, device)
    else:
        train(args, device)


if __name__ == "__main__":
    main()
