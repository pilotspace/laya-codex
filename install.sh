#!/bin/sh
# laya-codex installer: puts `laya-codex` and its pinned Moon sidecar in one directory and downloads
# the laya-code re-ranker (a fine-tune of the Laya model) from Hugging Face. Re-running it upgrades
# in place.
#
#   curl -fsSL https://raw.githubusercontent.com/pilotspace/laya-codex/main/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- --version v0.1.0 --dir ~/bin --no-model
#   curl -fsSL .../install.sh | sh -s -- --model-only    # just the re-ranker (e.g. after brew install)
#
# Every download is retried, time-limited and checked against a SHA-256 before anything is
# installed. Binaries are swapped in with a rename, so an interrupted run leaves the previous
# install working. Environment overrides: LAYA_CODEX_VERSION, LAYA_CODEX_INSTALL_DIR, LAYA_CODEX_HOME, LAYA_CODEX_MODEL
# (1 = download the model, 0 = skip), LAYA_CODEX_RELEASES_URL and LAYA_CODEX_MODEL_URL (mirrors and tests).
set -eu

REPO="pilotspace/laya-codex"
VERSION="${LAYA_CODEX_VERSION:-latest}"
INSTALL_DIR="${LAYA_CODEX_INSTALL_DIR:-$HOME/.local/bin}"
LAYA_CODEX_HOME="${LAYA_CODEX_HOME:-$HOME/.cache/laya-codex}"
RELEASES_URL="${LAYA_CODEX_RELEASES_URL:-https://github.com/$REPO/releases}"
MODEL_URL="${LAYA_CODEX_MODEL_URL:-https://huggingface.co/tindang/laya-code/resolve/main}"
MODEL="${LAYA_CODEX_MODEL:-auto}"
MODEL_ONLY=0

say() { printf 'laya-codex-install: %s\n' "$*"; }
die() { printf 'laya-codex-install: error: %s\n' "$*" >&2; exit 1; }

usage() {
    cat <<'EOF'
Usage: install.sh [--version vX.Y.Z] [--dir DIR] [--no-model | --model | --model-only]

  --version   release tag to install (default: the latest release)
  --dir       where laya-codex and moon go (default: ~/.local/bin)
  --no-model  skip the ~850 MB re-ranker download (laya-codex then ranks lexically)
  --model     download the re-ranker even on Linux (it runs on CPU there, which is slow)
  --model-only
              download and verify only the re-ranker into $LAYA_CODEX_HOME/models/laya-code and leave
              the binaries alone (for installs made another way, e.g. Homebrew)
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --version) [ $# -ge 2 ] || die "--version needs a value"; VERSION="$2"; shift 2 ;;
        --dir) [ $# -ge 2 ] || die "--dir needs a value"; INSTALL_DIR="$2"; shift 2 ;;
        --no-model) MODEL=0; shift ;;
        --model) MODEL=1; shift ;;
        --model-only) MODEL=1; MODEL_ONLY=1; shift ;;
        -h | --help) usage; exit 0 ;;
        *) usage >&2; die "unknown option: $1" ;;
    esac
done

need() { command -v "$1" >/dev/null 2>&1 || die "'$1' is required but not installed"; }
need curl
need tar
need uname
need mktemp

if command -v sha256sum >/dev/null 2>&1; then
    sha256() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
    sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
    die "need sha256sum or shasum to verify downloads"
fi

# Transfers slower than 10 KB/s for this many seconds are abandoned and retried.
STALL_SECONDS="${LAYA_CODEX_STALL_SECONDS:-30}"
# Also retry after a stall or a dropped connection (curl 7.71+; older curl retries fewer cases).
RETRY_ALL=""
if curl --help all 2>/dev/null | grep -q -- --retry-all-errors; then RETRY_ALL="--retry-all-errors"; fi

# fetch <url> <file> [max seconds]: download with retries, time limits and stall detection.
# Always writes to a file (never stdout), so curl truncates a partial download before retrying
# instead of appending to it. file:// URLs work too (used by the installer test).
fetch() {
    rm -f "$2"
    # shellcheck disable=SC2086 # RETRY_ALL is empty or one flag
    curl --fail --silent --show-error --location --retry 5 --retry-delay 2 $RETRY_ALL \
        --connect-timeout 15 --speed-limit 10240 --speed-time "$STALL_SECONDS" \
        --max-time "${3:-900}" --output "$2" "$1"
}

# Download the laya-code re-ranker listed in the Hugging Face MANIFEST.sha256, verifying each file.
get_model() {
    dest="$LAYA_CODEX_HOME/models/laya-code"
    say "fetching the laya-code re-ranker manifest"
    fetch "$MODEL_URL/MANIFEST.sha256" "$tmp/MANIFEST.sha256" 60 || die "could not fetch the model manifest"
    mkdir -p "$dest"
    # One "<sha256>  <path>" line per file; download only what is missing or stale.
    while read -r want file; do
        [ -n "$file" ] || continue
        case "$file" in /* | *..*) die "unsafe path in model manifest: $file" ;; esac
        if [ -f "$dest/$file" ] && [ "$(sha256 "$dest/$file")" = "$want" ]; then
            continue
        fi
        say "downloading model file $file"
        mkdir -p "$(dirname "$dest/$file")"
        fetch "$MODEL_URL/$file" "$dest/$file.part" 3600 || die "download failed: model $file"
        got="$(sha256 "$dest/$file.part")"
        [ "$got" = "$want" ] || { rm -f "$dest/$file.part"; die "checksum mismatch for model $file"; }
        mv -f "$dest/$file.part" "$dest/$file"
    done <"$tmp/MANIFEST.sha256"
    say "model ready in $dest"
}

if [ "$MODEL_ONLY" = 1 ]; then
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT INT TERM
    get_model
    exit 0
fi

os="$(uname -s)"
arch="$(uname -m)"
case "$os/$arch" in
    Darwin/arm64) target="aarch64-apple-darwin" ;;
    Linux/x86_64 | Linux/amd64) target="x86_64-unknown-linux-gnu" ;;
    *) die "no prebuilt laya-codex for $os/$arch yet; build from source: https://github.com/$REPO#build" ;;
esac
if [ "$MODEL" = auto ]; then
    # Linux has no GPU path yet: the model would run on CPU and mostly miss its time budget.
    if [ "$os" = Darwin ]; then MODEL=1; else MODEL=0; fi
fi

if [ "$VERSION" = latest ]; then
    # The /latest URL redirects to /tag/<version>; read the tag from the final URL.
    final="$(curl --fail --silent --location --retry 3 --connect-timeout 15 --max-time 60 \
        --output /dev/null --write-out '%{url_effective}' "$RELEASES_URL/latest")" ||
        die "could not reach $RELEASES_URL/latest"
    VERSION="${final##*/}"
    case "$VERSION" in v[0-9]*) ;; *) die "could not resolve the latest release (got '$VERSION')" ;; esac
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

# Download <asset> of this release and check it against <asset>.sha256.
get_asset() {
    asset="$1"
    say "downloading $asset"
    fetch "$RELEASES_URL/download/$VERSION/$asset" "$tmp/$asset" || die "download failed: $asset"
    fetch "$RELEASES_URL/download/$VERSION/$asset.sha256" "$tmp/$asset.sha256" 60 ||
        die "download failed: $asset.sha256"
    want="$(cut -d' ' -f1 <"$tmp/$asset.sha256")"
    got="$(sha256 "$tmp/$asset")"
    [ -n "$want" ] && [ "$want" = "$got" ] || die "checksum mismatch for $asset (want $want, got $got)"
    tar -xzf "$tmp/$asset" -C "$tmp" || die "could not unpack $asset"
}

cli_pkg="laya-codex-$VERSION-$target"
moon_pkg="moon-$VERSION-$target"
get_asset "$cli_pkg.tar.gz"
get_asset "$moon_pkg.tar.gz"
[ -x "$tmp/$cli_pkg/laya-codex" ] || die "$cli_pkg.tar.gz has no laya-codex binary"
[ -x "$tmp/$moon_pkg/moon" ] || die "$moon_pkg.tar.gz has no moon binary"
"$tmp/$cli_pkg/laya-codex" --help >/dev/null 2>&1 || die "the downloaded laya-codex does not run on this machine"

# Stop a running daemon first (whichever version serves $LAYA_CODEX_HOME), so the next hook starts
# the new binaries.
LAYA_CODEX_HOME="$LAYA_CODEX_HOME" "$tmp/$cli_pkg/laya-codex" stop >/dev/null 2>&1 || true

mkdir -p "$INSTALL_DIR" || die "cannot create $INSTALL_DIR"
for bin in "$cli_pkg/laya-codex" "$moon_pkg/moon"; do
    name="${bin##*/}"
    cp "$tmp/$bin" "$INSTALL_DIR/.$name.new" || die "cannot write to $INSTALL_DIR"
    chmod 755 "$INSTALL_DIR/.$name.new"
    mv -f "$INSTALL_DIR/.$name.new" "$INSTALL_DIR/$name"
done
share="$LAYA_CODEX_HOME/share"
mkdir -p "$share"
cp "$tmp/$moon_pkg/LICENSE" "$share/moon-LICENSE" 2>/dev/null || true
cp "$tmp/$moon_pkg/SOURCE" "$share/moon-SOURCE" 2>/dev/null || true
say "installed laya-codex $VERSION and moon to $INSTALL_DIR"

if [ "$MODEL" = 1 ]; then
    get_model
else
    say "skipped the model: laya-codex ranks lexically (re-run with --model to add it)"
fi

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) say "note: $INSTALL_DIR is not on PATH; add it, e.g. export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac
# 0.1.x installed the CLI as `laya`; v0.2.0 renamed it without an alias, so the old one is dead
# weight (and its `laya hook` entries in repos need `laya-codex init`).
if [ -x "$INSTALL_DIR/laya" ]; then
    say "note: $INSTALL_DIR/laya is the 0.1.x CLI (renamed laya-codex in v0.2.0); remove it: rm $INSTALL_DIR/laya"
    say "note: in each repo set up with \`laya init\`, run \`laya-codex init\` and delete the old \`laya hook\` entries (see the CHANGELOG)"
fi

cat <<EOF

Next, enable laya-codex in Claude Code, either everywhere with the plugin (inside Claude Code):
  /plugin marketplace add pilotspace/laya-codex
  /plugin install laya-codex@laya-codex
or per repository:
  laya-codex init --repo /path/to/repo     # add the hooks and MCP server, then index the repo
Then check it:
  laya-codex doctor --repo /path/to/repo
EOF
