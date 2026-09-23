"""Fine-tune laya-base -> laya-code on (task, chunk, label) pairs.

- Model = reference DecisionModel (rl_common.build_model) loaded from laya-base; same keys, same architecture.
- Trainable: top N encoder layers + encoder.final_norm + decision head + type_emb + scorer. Lower layers and
  act_head frozen (act_head is unused by noul and is exported unchanged).
- Loss: log loss (strictly proper) on the 2 noul logits vs soft target [1-y, y], raw logits (T=1);
  temperature is fitted post-hoc on val (calibrate.py).
- Stratified micro-batches with a fixed class composition (pos / same-file soft / neg) = the natural data mix,
  so batches are balanced across classes without shifting the prior away from deployment.
- AdamW, two LR groups, warmup + linear decay, grad accumulation, grad clip; atomic resumable checkpoints.

    python3 finetune/train.py --top-layers 8 --steps 2500 [--resume]
    python3 finetune/train.py --bench            # throughput probe (fp32 vs bf16, N layers)
"""
import argparse
import json
import math
import os
import random
import sys
import time

import numpy as np
import torch
import torch.nn.functional as F

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import common  # noqa: E402
from rl_common import QTYPES, auroc, build_model, collate_items, load_cfg  # noqa: E402

CKPT_DIR = os.environ.get("LAYA_CODEX_FT_CKPT", os.path.join(common.WORK, "ckpt"))
Q_PROBS = [0.6, 0.2, 0.2]  # primary inference question most of the time, paraphrases for robustness


def load_rows(path):
    with open(path) as f:
        return [json.loads(l) for l in f]


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


def make_items(tok, rows, rng, train=True):
    items = []
    for r in rows:
        qi = rng.choices(range(len(common.QUESTIONS)), Q_PROBS)[0] if train else 0
        ids, markers = common.encode(tok, common.QUESTIONS[qi].format(task=r["task"]), r["state"])
        if len(markers) != 2:
            continue
        y = float(r["label"])
        items.append({"ids": ids, "markers": markers, "qtype": QTYPES["noul"], "target": [1.0 - y, y], "label": int(y >= 0.5),
                      "episode": 0, "ep_step": 0, "ep_len": 1, "src": r["repo"]})
    return items


def forward_logits(model, b, device, amp):
    with torch.autocast(device_type=device.type, dtype=torch.bfloat16, enabled=amp):
        logits, _ = model(b["input_ids"].to(device), b["attention_mask"].to(device), b["marker_pos"].to(device),
                          b["marker_mask"].to(device), b["qtype"].to(device))
    return logits.float()[:, :2]


def noul_loss(logits2, target):
    return -(target * F.log_softmax(logits2, -1)).sum(-1).mean()


class StratifiedSampler:
    """Fixed per-batch composition; each class is cycled through in its own shuffled order (epoch-style)."""

    def __init__(self, rows, comp, seed):
        self.rng = random.Random(seed)
        self.pools = {"pos": [], "soft": [], "neg": []}
        for i, r in enumerate(rows):
            y = float(r["label"])
            self.pools["pos" if y >= 1.0 else "neg" if y <= 0.0 else "soft"].append(i)
        self.comp = comp
        self.order = {k: [] for k in self.pools}
        self.epochs = {k: 0 for k in self.pools}

    def _take(self, k, n):
        out = []
        while len(out) < n and self.pools[k]:
            if not self.order[k]:
                self.order[k] = self.pools[k][:]
                self.rng.shuffle(self.order[k])
                self.epochs[k] += 1
            out.append(self.order[k].pop())
        return out

    def batch(self):
        idx = []
        for k, n in self.comp.items():
            idx += self._take(k, n)
        self.rng.shuffle(idx)
        return idx

    def state_dict(self):
        return {"rng": self.rng.getstate(), "order": self.order, "epochs": self.epochs}

    def load_state_dict(self, s):
        self.rng.setstate(s["rng"])
        self.order, self.epochs = s["order"], s["epochs"]


@torch.no_grad()
def predict(model, tok, rows, device, amp, bs=32, question=0):
    """Raw (T=1) noul logits [n, 2] for rows, primary question by default."""
    was_training = model.training
    model.train(False)
    out = []
    for s in range(0, len(rows), bs):
        chunk = rows[s:s + bs]
        items = []
        for r in chunk:
            ids, markers = common.encode(tok, common.QUESTIONS[question].format(task=r["task"]), r["state"])
            items.append({"ids": ids, "markers": markers, "qtype": QTYPES["noul"], "target": [0.0, 0.0], "label": -1,
                          "episode": 0, "ep_step": 0, "ep_len": 1, "src": ""})
        b = collate_items([items], tok.pad_token_id)
        out.append(forward_logits(model, b, device, amp).cpu())
    model.train(was_training)
    return torch.cat(out).numpy()


def val_metrics(logits, rows, T=1.0):
    y = np.array([float(r["label"]) for r in rows])
    z = logits / T
    p = np.exp(z[:, 1] - np.logaddexp(z[:, 0], z[:, 1]))
    nll = float(-(y * np.log(np.clip(p, 1e-9, 1)) + (1 - y) * np.log(np.clip(1 - p, 1e-9, 1))).mean())
    hard = np.array([yy in (0.0, 1.0) for yy in y])
    return {"nll": round(nll, 4), "auroc_pos_vs_neg": round(auroc(p[hard], y[hard].astype(int)), 4),
            "mean_p": round(float(p.mean()), 4), "base_rate": round(float(y.mean()), 4)}


def atomic_save(obj, path):
    tmp = path + ".tmp"
    torch.save(obj, tmp)
    os.replace(tmp, path)


def trainable_state(model):
    return {n: p.detach().cpu().clone() for n, p in model.named_parameters() if p.requires_grad}


def bench(args, device):
    model, tok, cfg, _ = load_base_model(args.base, device)
    data_dir = os.path.join(common.WORK, "data")
    rows = load_rows(os.path.join(data_dir, sorted(os.listdir(data_dir))[0]))
    rng = random.Random(0)
    for amp in [bool(a) for a in args.bench_amp]:
        for n in args.bench_layers:
            enc_p, head_p = set_trainable(model, n)
            if args.grad_ckpt:
                enable_checkpointing(model)
            opt = torch.optim.AdamW([{"params": enc_p, "lr": 1e-5}, {"params": head_p, "lr": 5e-5}])
            model.train()
            times = []
            for _ in range(4):
                batch = rng.sample(rows, args.micro)
                b = collate_items([make_items(tok, batch, rng)], tok.pad_token_id)
                t0 = time.perf_counter()
                loss = noul_loss(forward_logits(model, b, device, amp), b["target"][:, :2].to(device))
                loss.backward()
                opt.step()
                opt.zero_grad(set_to_none=True)
                if device.type == "mps":
                    torch.mps.synchronize()
                times.append(time.perf_counter() - t0)
                if not math.isfinite(loss.item()):
                    print("non-finite loss", amp, n)
            mem = torch.mps.driver_allocated_memory() / 2**30 if device.type == "mps" else 0
            med = float(np.median(times[1:]))
            print("amp=%s top=%d micro=%d L=%d  step %.2fs (%.1f seq/s) loss %.3f mem %.1fGB" % (
                amp, n, args.micro, b["input_ids"].shape[1], med, args.micro / med, loss.item(), mem), flush=True)
            del opt
    # numeric check fp32 vs bf16 forward (weights were perturbed by the bench steps; only the delta matters)
    b = collate_items([make_items(tok, rows[:16], random.Random(1), train=False)], tok.pad_token_id)
    model.train(False)
    with torch.no_grad():
        a = forward_logits(model, b, device, False)
        c = forward_logits(model, b, device, True)
    print("fp32 vs bf16 max |dlogit| = %.4f" % (a - c).abs().max().item())


def train(args, device):
    torch.manual_seed(args.seed)
    os.makedirs(CKPT_DIR, exist_ok=True)
    model, tok, cfg, _ = load_base_model(args.base, device)
    enc_p, head_p = set_trainable(model, args.top_layers)
    if args.grad_ckpt:
        enable_checkpointing(model)
    n_tr = sum(p.numel() for p in enc_p + head_p)
    print("trainable params %.1fM (top %d layers + head)" % (n_tr / 1e6, args.top_layers), flush=True)
    opt = torch.optim.AdamW([{"params": enc_p, "lr": args.lr_enc, "weight_decay": 0.01},
                             {"params": head_p, "lr": args.lr_head, "weight_decay": 0.01}])
    base_lrs = [args.lr_enc, args.lr_head]

    def progress(step, secs):
        """Fraction of the run done: by steps or by wall-clock budget, whichever is further (shared machine)."""
        return max(step / args.steps, secs / (args.max_hours * 3600) if args.max_hours else 0.0)

    def lr_scale(step, secs):
        if step < args.warmup:
            return (step + 1) / args.warmup
        return max(0.1, 1.0 - 0.9 * progress(step, secs))

    train_rows = load_rows(os.path.join(common.WORK, "train.jsonl"))
    val_rows = load_rows(os.path.join(common.WORK, "val.jsonl"))
    vr = random.Random(7)
    val_sub = vr.sample(val_rows, min(args.val_n, len(val_rows)))
    total = args.micro
    comp = {"pos": round(total * args.frac_pos), "soft": round(total * args.frac_soft)}
    comp["neg"] = total - comp["pos"] - comp["soft"]
    sampler = StratifiedSampler(train_rows, comp, args.seed)
    rng = random.Random(args.seed + 1)
    step, log, best, train_secs = 0, [], None, 0.0
    last = os.path.join(CKPT_DIR, "last.pt")
    if args.resume and os.path.exists(last):
        ck = torch.load(last, map_location="cpu", weights_only=False)
        diff = set(trainable_names(model)) ^ set(ck["params"])
        assert not diff, "checkpoint trainable set differs (different --top-layers?): %s" % sorted(diff)[:5]
        with torch.no_grad():
            for n, p in model.named_parameters():
                if n in ck["params"]:
                    p.copy_(ck["params"][n].to(device))
        opt.load_state_dict(ck["opt"])
        sampler.load_state_dict(ck["sampler"])
        rng.setstate(ck["rng"])
        step, log, best, train_secs = ck["step"], ck["log"], ck.get("best"), ck.get("train_secs", 0.0)
        print("resumed at step %d" % step, flush=True)
    else:
        if args.init_from:  # warm start: weights only (fresh optimizer, schedule and sampler on the new data)
            ck = torch.load(args.init_from, map_location="cpu", weights_only=False)
            diff = set(trainable_names(model)) ^ set(ck["params"])
            assert not diff, "init checkpoint trainable set differs: %s" % sorted(diff)[:5]
            with torch.no_grad():
                for n, p in model.named_parameters():
                    if n in ck["params"]:
                        p.copy_(ck["params"][n].to(device))
            print("initialised from %s (step %s)" % (args.init_from, ck.get("step")), flush=True)
        m = val_metrics(predict(model, tok, val_sub, device, args.amp), val_sub)
        log.append({"step": 0, "val": m})
        print("step 0 val %s" % m, flush=True)
    print("train rows %d  val rows %d (monitor %d)  comp %s" % (len(train_rows), len(val_rows), len(val_sub), comp), flush=True)
    model.train()
    t0, ema = time.time(), None
    finished = False
    while not finished:
        ts = time.time()
        for g, lr in zip(opt.param_groups, base_lrs):
            g["lr"] = lr * lr_scale(step, train_secs)
        tot = 0.0
        for _ in range(args.accum):
            batch = [train_rows[i] for i in sampler.batch()]
            b = collate_items([make_items(tok, batch, rng)], tok.pad_token_id)
            loss = noul_loss(forward_logits(model, b, device, args.amp), b["target"][:, :2].to(device)) / args.accum
            loss.backward()
            tot += loss.item()
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
            print("step %d loss %.4f (ema %.4f) lr %.2e | %.1fs/step progress %.3f epochs %s" % (
                step, tot, ema, opt.param_groups[0]["lr"], el / 25, progress(step, train_secs), sampler.epochs), flush=True)
            t0 = time.time()
        if step % args.eval_every == 0 or finished:
            if device.type == "mps":
                torch.mps.empty_cache()  # validation on top of training allocations OOMed under the 9.6 GB cap
            m = val_metrics(predict(model, tok, val_sub, device, args.amp, bs=args.val_bs), val_sub)
            if device.type == "mps":
                torch.mps.empty_cache()
            log.append({"step": step, "train_loss_ema": round(ema, 4), "val": m})
            print("step %d val %s" % (step, m), flush=True)
            if best is None or m["nll"] < best["nll"]:
                best = dict(m, step=step)
                atomic_save({"params": trainable_state(model), "step": step, "val": m, "top_layers": args.top_layers},
                            os.path.join(CKPT_DIR, "best.pt"))
            t0 = time.time()
        if step % args.ckpt_every == 0 or finished:
            atomic_save({"params": trainable_state(model), "opt": opt.state_dict(), "sampler": sampler.state_dict(),
                         "rng": rng.getstate(), "step": step, "log": log, "best": best, "args": vars(args),
                         "train_secs": train_secs}, last)
    json.dump({"log": log, "best": best, "args": vars(args)}, open(os.path.join(CKPT_DIR, "train_log.json"), "w"), indent=1)
    print("done. best %s" % best, flush=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default=common.BASE_MODEL)
    ap.add_argument("--device", default="mps" if torch.backends.mps.is_available() else "cpu")
    ap.add_argument("--top-layers", type=int, default=8)
    ap.add_argument("--steps", type=int, default=2500)
    ap.add_argument("--micro", type=int, default=16)
    ap.add_argument("--accum", type=int, default=2)
    ap.add_argument("--lr-enc", type=float, default=2e-5)
    ap.add_argument("--lr-head", type=float, default=1e-4)
    ap.add_argument("--warmup", type=int, default=100)
    ap.add_argument("--frac-pos", type=float, default=0.25)
    ap.add_argument("--frac-soft", type=float, default=0.125)
    ap.add_argument("--amp", action="store_true", help="bf16 autocast (only if --bench shows it is stable)")
    ap.add_argument("--eval-every", type=int, default=250)
    ap.add_argument("--ckpt-every", type=int, default=100)
    ap.add_argument("--val-n", type=int, default=1200)
    ap.add_argument("--seed", type=int, default=13)
    ap.add_argument("--resume", action="store_true")
    ap.add_argument("--grad-ckpt", action="store_true")
    ap.add_argument("--init-from", default=None, help="trainable-params checkpoint to warm start from")
    ap.add_argument("--val-bs", type=int, default=16)
    ap.add_argument("--max-hours", type=float, default=0.0, help="wall-clock budget for the optimisation loop (0 = steps only)")
    ap.add_argument("--bench", action="store_true")
    ap.add_argument("--bench-layers", type=int, nargs="*", default=[6, 10])
    ap.add_argument("--bench-amp", type=int, nargs="*", default=[0, 1])
    args = ap.parse_args()
    device = torch.device(args.device)
    if args.bench:
        bench(args, device)
    else:
        train(args, device)


if __name__ == "__main__":
    main()
