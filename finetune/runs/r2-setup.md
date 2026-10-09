# Run r2: every command, in order

laya-code-r2 is a ModernBERT-base re-ranker in the laya-code layout: the pretrained
`answerdotai/ModernBERT-base` encoder with a freshly initialised laya decision head, trained on the
r1 candidate lists. It is laya-codex's default model from v0.5.0, served at 128 tokens per
candidate with the top 12 scored. It is published on the `r2` branch of `tindang/laya-code`
(commit `831fa8321213ab66a8085d39f0014c5f9f8b5f91`); `main` and `r1` are untouched.

It was produced on an M4 Pro 24 GB (macOS, torch 2.11 with MPS, transformers 5.4, huggingface_hub CLI 1.8.0),
2026-10-07 to 2026-10-09. Hashes, seeds, the validation history and the replay results are in
[r2.json](r2.json).

Paths used:

```sh
WT=<this repository at feat/laya-code-student>
F=~/.cache/laya-codex/finetune                                     # lists, checkpoints, logs
M=~/.cache/laya-codex/models
export LAYA_CODEX_FT_BASE=$M/laya-code-r1                          # template and teacher (r1)
export PYTORCH_ENABLE_MPS_FALLBACK=0
MOON=<moon binary laya-codex ships>
```

## 1. Repositories and candidate lists

Unchanged from r1: the same 7 training repositories at the same HEADs, the same 4,466 lists in
`$F/data_v3` and the same held-out repositories (moon, httpx, hono, pilot-space). See
[r1-setup.md](r1-setup.md) steps 1, 3 and 4 and the `repos` and `data` blocks of r2.json.

## 2. Models (hf CLI)

```sh
hf download answerdotai/ModernBERT-base --local-dir <hf>          # revision 8949b909ec900327062f0ebf497f51aef5e6f0c8
                                                                   # model.safetensors sha256 340ac08b...8023a9
# laya-code-r1 (r1-setup.md) is the template for rl_common.py, rl_agent_api.py and the agent
# config, and the teacher of the distilled arm. None of its weights go into r2.
```

## 3. Student init

```sh
cd $WT
python3 finetune/student.py --hf <hf> --template $M/laya-code-r1 --out $F/student-init --max-len 704 --seed 13
# 170 tensors (F16 + F32 temperature); student_init.json records the hashes.
# max_len 704 leaves a 384-token window uncut (finetune/common.py required_max_len).
```

Before training, the init got Python-to-Rust parity fixtures (`spike/make_parity_fixtures.py`)
and a short smoke run into `$F/student-smoke`, exported to `$F/student-smoke-export` to check the
export path end to end. Their logs were scratch files and are not kept; the parity that counts is
the exported model's (step 6).

## 4. Teacher cache (distilled arm only)

```sh
python3 finetune/teacher.py --teacher $M/laya-code-r1 --out $F/student-teacher/r1-w128.npz --window 128 --device mps
```

## 5. Training: two arms, the same settings, with and without distillation

`$F/run-students.sh` (each arm resumes from its last checkpoint after a crash, up to 3 retries):

```sh
COMMON="--base $F/student-init --top-layers 22 --lists-per-step 4 --lr-enc 5e-5 --lr-head 3e-4 --warmup 50 \
  --list-weight 1.0 --windows 128 256 384 --val-windows 128 256 384 --val-n 400 --grad-ckpt --pad-multiple 64 \
  --epochs 4 --max-hours 10 --eval-every 271 --ckpt-every 50 --seed 13"
python3 finetune/train.py $COMMON --ckpt-dir $F/student-kd --kd-weight 1.0 --teacher-cache $F/student-teacher/r1-w128.npz
python3 finetune/train.py $COMMON --ckpt-dir $F/student-nokd --kd-weight 0      # r2 comes from this arm
```

- Each training list is drawn at one of the windows 128, 256 and 384; validation scores all three.
- Both arms ran all 2,168 updates (4 epochs). `best.pt` is the update with the best mean blend top-2
  gold over the three validation windows; for the no-distillation arm, update 542.
- The distilled arm's learning-rate schedule was compressed by a 2.7 h machine sleep; its best
  checkpoint (update 271) was not chosen.

## 6. Export, calibration, parity

```sh
INIT=$F/student-init
python3 finetune/export.py --base $INIT --ckpt $F/student-nokd/best.pt --out $M/laya-code-student-nokd-best
python3 finetune/calibrate.py --model $M/laya-code-student-nokd-best --window 256        # screening window
python3 finetune/export.py --base $INIT --verify $M/laya-code-student-nokd-best
# Per-window serving copies: same weights (a symlink), noul temperature refitted at that window.
for w in 128 384; do
  out=$M/laya-code-student-nokd-best-w$w; mkdir -p $out
  cp -R $M/laya-code-student-nokd-best/{encoder,tokenizer,rl_agent_api.py,rl_common.py,rl_agent_config.json} $out/
  ln -s $M/laya-code-student-nokd-best/model.safetensors $out/model.safetensors
  python3 finetune/calibrate.py --model $out --window $w                                 # W=128: T 0.8383
done
python3 spike/make_parity_fixtures.py $M/laya-code-student-nokd-best-w128 <fixtures>
LAYA_CODEX_MODEL_DIR=$M/laya-code-student-nokd-best-w128 LAYA_CODEX_PARITY_FIXTURES=<fixtures> \
  cargo test -p laya-model --release --locked --features metal --test parity              # 7 of 7, CPU and Metal
rm -rf $M/laya-code-student-nokd-best*/__pycache__
```

## 7. Replay screen (G1) and latency

Every arm goes through the real hook with `bench/replay_hooks.py` on tasks-v8 and tasks-heldout
(120 first prompts, 238 gold files), on a scratch home and Moon port:

```sh
python3 bench/replay_hooks.py --bin target/release/laya-codex --home <scratch> --moon-port <port> \
  --moon-bin $MOON --repo <clone> --tasks bench/<tasks-v8|tasks-heldout>/<repo>.jsonl --out <part> \
  --label <arm> --model-dir <dir> --env LAYA_CODEX_STATE_TOKENS=<w> --env LAYA_CODEX_SCORE_TOP=<k>
```

- Screen: keywords 118, r1 132, student nokd-best at (W 128, K 24) 145, (256, 16) 142 and (384, 12)
  142 gold.
- Latency, arms rotated per round on one warm index: the student at (128, 12) inlines 141 with the
  hook at 167 ms p50 and 180 ms p90; r1 at (128, 16) takes 517 ms p50. The owner chose (128, 12)
  for latency (2026-10-09).

## 8. Packaging laya-code-r2

```sh
OUT=$M/laya-code-r2
mkdir $OUT.tmp
for f in encoder tokenizer model.safetensors rl_agent_api.py rl_agent_config.json rl_common.py; do
  cp -RL $M/laya-code-student-nokd-best-w128/$f $OUT.tmp/                 # real files, no symlinks
done
# rl_agent_config.json: add the serving block laya-codex reads, name the model, and correct the
# provenance text (this arm had no distillation).
python3 - $OUT.tmp/rl_agent_config.json <<'EOF'
import json, sys
p = sys.argv[1]; cfg = json.load(open(p))
cfg["model_name"] = "laya-code-r2"
cfg["finetune"]["base"] = ("laya-code-r2: answerdotai/ModernBERT-base with a laya decision head, trained on laya-codex "
    "candidate lists with gold labels only (no distillation), windows drawn from 128/256/384; checkpoint student-nokd step 542")
cfg["serving"] = {"state_tokens": 128, "score_top": 12, "search_score_top": 12}
json.dump(cfg, open(p, "w"), indent=2)
EOF
mv $OUT.tmp $OUT
find $OUT -type l                                                          # nothing
(cd $OUT && shasum -a 256 encoder/config.json model.safetensors rl_agent_api.py rl_agent_config.json \
  rl_common.py tokenizer/tokenizer.json tokenizer/tokenizer_config.json) > release/hf-laya-code/MANIFEST.sha256
shasum -a 256 release/hf-laya-code/MANIFEST.sha256                         # f5d20914...a623, pinned by install.sh
```

The weights are byte-identical to `laya-code-student-nokd-best/model.safetensors`; the
tokenizer, encoder config and temperature are the w128 copy's, so its parity result carries over.

## 9. Default wiring check

The feat/r2-default binary, with only `LAYA_CODEX_MODEL_DIR` set (no serving variables), through
the real hook on the same 120 first prompts: laya-code-r2 inlines 141 of 238, scores 12 on every
prompt, all in `laya` mode, hook 165 ms p50; the same binary with laya-code-r1 inlines 132 and
scores 16 (r1 has no serving block), hook 517 ms p50. The 53 v14 `search` lookups score 12 with
r2 (8 with r1, which its 400 ms budget allows).

## 10. Release files

`release/hf-laya-code/` holds the manifest, the model card (generated by `finetune/model_card.py
--run finetune/runs/r2.json`) and `NOTICE`. The owner uploads the seven model files with them to
the `r2` branch of `tindang/laya-code`; the commit then goes into `install.sh` (`MODEL_REVISION`),
`crates/laya-cli/src/config.rs` (`LAYA_CODE_REVISION`) and the docs, replacing
`REPLACE_WITH_HF_R2_REVISION_SHA_AFTER_UPLOAD`, and into `published.revision` here.
