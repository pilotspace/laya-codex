# Run r1: every command, in order

The retrained laya-code (Hugging Face revision 2) was produced by these commands on an M4 Pro 24 GB
(macOS, torch 2.11 with MPS, transformers 5.4, huggingface_hub CLI 1.8.0), 2026-09-25/26. Repository
HEADs, data hashes, seeds and results are in [r1.json](r1.json).

Paths used:

```sh
WT=<this repository at feat/reranker-retrain>
export LAYA_CODEX_REPOS_ROOT=~/.cache/laya-codex/train-repos       # training repos and pilot-space
export LAYA_CODEX_BENCH_REPOS=<dir with moon, httpx, hono>          # benchmark checkouts (held out)
export LAYA_CODEX_FT_BASE=~/.cache/laya-codex/models/laya-code-hf   # previous laya-code (warm start)
MOON=<moon binary laya-codex ships>                                # pilotspace/moon 8bba3ced build
CANDGEN=~/.cache/laya-codex/finetune/candgen-target/release/laya-candgen
```

## 1. Repositories (gh CLI, full history, no shallow clones)

```sh
R=$LAYA_CODEX_REPOS_ROOT
gh repo clone openai/codex $R/codex -- --quiet
gh repo clone MervinPraison/PraisonAI $R/PraisonAI -- --quiet
gh repo clone earendil-works/pi $R/pi-mono -- --quiet
gh repo clone ets-labs/python-dependency-injector $R/python-dependency-injector -- --quiet
gh repo clone Netflix/dispatch $R/repo-sample/python/dispatch -- --quiet
gh repo clone TinDang97/velos $R/velos -- --quiet
gh repo clone pilotspace/hydroa $R/ai-proxy -- --quiet
gh repo clone TinDang97/pilot-space $R/pilot-space -- --quiet          # held out
# ai-guard (a local Portkey-AI/gateway checkout in the first laya-code) is unavailable: dropped.
for r in codex PraisonAI pi-mono python-dependency-injector repo-sample/python/dispatch velos ai-proxy pilot-space; do
  git -C $R/$r rev-parse --is-shallow-repository HEAD                  # false + the HEADs in r1.json
done
```

## 2. Models (hf CLI)

```sh
hf download tindang/laya-code --local-dir ~/.cache/laya-codex/models/laya-code-hf
# revision f3d6bd2344e4750dd917f95d40ceacfa81bb81db; model.safetensors sha256 1cc1a371...cae5f8
# laya-base is not needed: the warm start is from laya-code.
```

## 3. Binaries

```sh
cd $WT && cargo build --release --locked -p laya-cli                    # at origin/main 1d55bf3: the replay binary
(cd $WT/finetune/candgen && CARGO_TARGET_DIR=~/.cache/laya-codex/finetune/candgen-target cargo build --release --locked)
```

Candidate-order check against the daemon (httpx, one task, lexical mode):

```sh
printf '%s\n' '{"op":"index","root":"<httpx>","repo":"httpx"}' \
  '{"op":"query","repo":"httpx","prompt":"Fixed `iter_text` adding an empty string","id":"t1"}' |
  $CANDGEN --moon-bin $MOON --port 16561 --home <scratch>
LAYA_CODEX_HOME=<scratch2> LAYA_CODEX_MOON_PORT=16562 LAYA_CODEX_MOON_BIN=$MOON LAYA_CODEX_BUDGET_MS=0 \
  LAYA_CODEX_NO_MODEL=1 target/release/laya-codex query 'Fixed `iter_text` adding an empty string' \
  --repo <httpx> --top 20 --json                                          # same order; then `laya-codex stop`
```

## 4. Leakage check and lists

```sh
cd $WT/finetune && python3 leakage.py --out leakage-report.json      # repos (and later rows): ok
cd $WT
python3 finetune/build_data.py --candgen $CANDGEN --moon-bin $MOON --only velos --max-commits 30 --workers 1  # trial
python3 finetune/build_data.py --candgen $CANDGEN --moon-bin $MOON --max-commits 1200 --workers 4 --heldout pilot-space
# The session interrupt killed this at codex 894/1,200 (the other 6 repos and pilot-space had finished).
# Resume (c02bf12 keeps finished lists); the first attempt timed out replaying the old Moon AOF, so:
rm -rf ~/.cache/laya-codex/finetune/candgen-home/codex/moon
python3 finetune/build_data.py --candgen $CANDGEN --moon-bin $MOON --only codex --max-commits 1200 --workers 1
# Stopped by hand at 1,170 codex lists to start training; codex.jsonl.tmp (complete lines) -> codex.jsonl,
# scratch worktree removed with `git worktree remove --force` + `git worktree prune`.
cd $WT/finetune && python3 leakage.py --out leakage-report.json      # ok on 4,466 lists
```

## 5. Training

```sh
cd $WT
python3 finetune/train.py --base $LAYA_CODEX_FT_BASE --bench --grad-ckpt --bench-layers 8 12 16   # throughput
python3 finetune/train.py --base $LAYA_CODEX_FT_BASE --bench --bench-layers 8 12
LAYA_CODEX_FT_CKPT=~/.cache/laya-codex/finetune/ckpt-smoke \
  python3 finetune/train.py --base $LAYA_CODEX_FT_BASE --steps 4 --eval-every 2 --val-n 20 --top-layers 8  # smoke
# r1 (a first launch without --grad-ckpt reached 24 GB, swapped and was killed at step 0; 2375782 fixed it):
LAYA_CODEX_FT_CKPT=~/.cache/laya-codex/finetune/ckpt-r1 PYTORCH_ENABLE_MPS_FALLBACK=0 \
  python3 finetune/train.py --base $LAYA_CODEX_FT_BASE --top-layers 12 --epochs 2 --lists-per-step 4 \
  --lr-enc 2e-5 --lr-head 1e-4 --warmup 30 --list-weight 1.0 --eval-every 180 --val-n 400 --max-hours 5 --grad-ckpt
# Stopped by hand (SIGTERM) after the update-550 checkpoint, of 1,084 planned; best.pt = update 540.
cp ~/.cache/laya-codex/finetune/ckpt-r1/best.pt ~/.cache/laya-codex/finetune/ckpt-r1/best-s540.pt
```

## 6. Export, calibration, parity, list evaluation

```sh
OUT=~/.cache/laya-codex/models/laya-code-r1
python3 finetune/export.py --base $LAYA_CODEX_FT_BASE --ckpt ~/.cache/laya-codex/finetune/ckpt-r1/best.pt --out $OUT
python3 finetune/calibrate.py --model $OUT                               # T 0.9410 -> 0.8057
python3 finetune/export.py --base $LAYA_CODEX_FT_BASE --verify $OUT
cp fixtures/laya_parity.json /tmp/laya_parity.committed.json
python3 spike/make_parity_fixtures.py $OUT fixtures/laya_parity.json
LAYA_CODEX_MODEL_DIR=$OUT cargo test -p laya-model --release --locked --features metal --test parity -- --nocapture
cp /tmp/laya_parity.committed.json fixtures/laya_parity.json          # 7 of 7 passed on CPU and Metal
python3 finetune/eval_lists.py --models current=$LAYA_CODEX_FT_BASE candidate=$OUT --out eval-lists-laya-code-r1.json
rm -rf $OUT/__pycache__
```

## 7. Replay gate

```sh
# baseline arms, then with the candidate (a scratch home and port owned by this run):
sh bench/replay_decide.sh --bin target/release/laya-codex --home <scratch> --model-dir $LAYA_CODEX_FT_BASE \
  --moon-bin $MOON --repos-dir $LAYA_CODEX_BENCH_REPOS --port 16560 --out replay-baseline.jsonl
sh bench/replay_decide.sh --bin target/release/laya-codex --home <scratch> --model-dir $LAYA_CODEX_FT_BASE \
  --moon-bin $MOON --repos-dir $LAYA_CODEX_BENCH_REPOS --port 16560 --out replay-r1.jsonl \
  --candidate-model-dir $OUT
```

## 8. Release files

`release/hf-laya-code/MANIFEST.sha256` = `shasum -a 256` of the seven model files of `$OUT`; the
upload is the owner's (see the model card and `install.sh`: the pinned revision is filled in after it).
