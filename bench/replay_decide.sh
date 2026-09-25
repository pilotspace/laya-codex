#!/bin/sh
# Offline replay gate: who decides what loads -- keywords (lexical only), blend (the production
# configuration: lexical + Laya, w=0.5, default score_top), model-only (Laya alone, w=1, a
# diagnostic, not the shipped mode), and -- when --candidate-model-dir is given -- candidate (the
# retrained model in the same production blend as `blend`, only the model swapped) and
# candidate-model-only (that model alone, the model-only diagnostic for it), each replayed through
# the real `laya-codex hook` (bench/replay_hooks.py) across every repo.
# No Claude Code session runs; this is free and safe to run before spending on a paid benchmark.
#
# The gate: `candidate` must beat `keywords` and match or beat `blend` -- Laya ships as a reranker
# blended with lexical (owner decision 2026-09-25), never as a model deciding alone, so a candidate
# is judged in that production configuration, not model-only.
#
# Every path and port is a required argument: nothing here is specific to one machine or one job.
#
#   sh bench/replay_decide.sh --bin PATH --home DIR --model-dir DIR --moon-bin PATH \
#       --repos-dir DIR --port N [--candidate-model-dir DIR] [--out FILE] [--repos "httpx hono moon"]
#
# --repos-dir must hold one checkout per repo name in --repos (default: httpx hono moon), each
# matching a bench/tasks-v8/<repo>.jsonl task file already in this repo. Use a --home / --port this
# run alone owns: the daemon and Moon belong to the home, and a shared one would serve whichever
# binary or config started first.
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
REPOS="httpx hono moon"
OUT=""
CANDIDATE_MODEL_DIR=""

usage() {
    echo "usage: sh $0 --bin PATH --home DIR --model-dir DIR --moon-bin PATH --repos-dir DIR --port N" >&2
    echo "           [--candidate-model-dir DIR] [--out FILE] [--repos \"httpx hono moon\"]" >&2
    exit 1
}

while [ $# -gt 0 ]; do
    case "$1" in
        --bin) BIN=$2; shift 2 ;;
        --home) HOME_DIR=$2; shift 2 ;;
        --model-dir) MODEL_DIR=$2; shift 2 ;;
        --moon-bin) MOON_BIN=$2; shift 2 ;;
        --repos-dir) REPOS_DIR=$2; shift 2 ;;
        --port) PORT=$2; shift 2 ;;
        --candidate-model-dir) CANDIDATE_MODEL_DIR=$2; shift 2 ;;
        --out) OUT=$2; shift 2 ;;
        --repos) REPOS=$2; shift 2 ;;
        -h|--help) usage ;;
        *) echo "unknown argument: $1" >&2; usage ;;
    esac
done

: "${BIN:?--bin is required}"
: "${HOME_DIR:?--home is required}"
: "${MODEL_DIR:?--model-dir is required}"
: "${MOON_BIN:?--moon-bin is required}"
: "${REPOS_DIR:?--repos-dir is required}"
: "${PORT:?--port is required}"
[ -n "$OUT" ] || OUT="$HOME_DIR/replay-decide.jsonl"

mkdir -p "$HOME_DIR"
rm -f "$OUT"

# One ranking arm: stop any daemon left on this home, replay every repo's tasks, stop it again so
# the next arm's daemon starts clean with this arm's environment.
one() {
    label=$1; shift
    extra=""
    for kv in "$@"; do extra="$extra --env $kv"; done
    LAYA_CODEX_HOME="$HOME_DIR" "$BIN" stop >/dev/null 2>&1 || true
    for repo in $REPOS; do
        echo "--- $label $repo $(date '+%H:%M:%S')"
        # shellcheck disable=SC2086
        python3 "$HERE/replay_hooks.py" --bin "$BIN" --home "$HOME_DIR" --moon-port "$PORT" \
            --model-dir "$MODEL_DIR" --moon-bin "$MOON_BIN" \
            --repo "$REPOS_DIR/$repo" --tasks "$HERE/tasks-v8/$repo.jsonl" --out "$OUT" --label "$label" $extra
    done
    LAYA_CODEX_HOME="$HOME_DIR" "$BIN" stop >/dev/null 2>&1 || true
}

one blend
one keywords LAYA_CODEX_BUDGET_MS=0
one model-only LAYA_CODEX_WEIGHT=1 LAYA_CODEX_SCORE_TOP=0 LAYA_CODEX_BUDGET_MS=3000
if [ -n "$CANDIDATE_MODEL_DIR" ]; then
    # candidate: the production blend (default weight, score_top and budget) with only the model
    # swapped -- this is what the gate compares with keywords and blend.
    one candidate LAYA_CODEX_MODEL_DIR="$CANDIDATE_MODEL_DIR"
    # candidate-model-only: the same candidate model alone (w=1), a diagnostic only -- never the
    # gate's comparison, since Laya ships blended, not deciding alone.
    one candidate-model-only LAYA_CODEX_MODEL_DIR="$CANDIDATE_MODEL_DIR" LAYA_CODEX_WEIGHT=1 LAYA_CODEX_SCORE_TOP=0 LAYA_CODEX_BUDGET_MS=3000
fi

# Moon is a sidecar the daemon starts under this home; `stop` above does not kill it (see
# CLAUDE.md), so match its --dir path explicitly and only touch what this run started.
pkill -f "moon.*--dir $HOME_DIR/moon" 2>/dev/null || true

# The table below carries every arm run above; read it as: does `candidate` (the production
# blend with the retrained model) beat `keywords` and match or beat `blend`? model-only and
# candidate-model-only are diagnostics only, not the comparison the gate is judged on.
python3 "$HERE/replay_hooks.py" summary "$OUT"
echo "REPLAY-DECIDE-DONE"
