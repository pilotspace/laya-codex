#!/bin/sh
# laya-codex installer: puts `laya` and its pinned Moon sidecar in one directory and downloads the
# laya-code re-ranker from Hugging Face. Re-running it upgrades in place.
#
#   curl -fsSL https://raw.githubusercontent.com/pilotspace/laya-codex/main/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- --version v0.1.0 --dir ~/bin --no-model
#
# Every download is retried, time-limited and checked against a SHA-256 before anything is
# installed. Binaries are swapped in with a rename, so an interrupted run leaves the previous
# install working. Environment overrides: LAYA_VERSION, LAYA_INSTALL_DIR, LAYA_HOME, LAYA_MODEL
# (1 = download the model, 0 = skip), LAYA_RELEASES_URL and LAYA_MODEL_URL (mirrors and tests).
set -eu

REPO="pilotspace/laya-codex"
VERSION="${LAYA_VERSION:-latest}"
INSTALL_DIR="${LAYA_INSTALL_DIR:-$HOME/.local/bin}"
LAYA_HOME="${LAYA_HOME:-$HOME/.cache/laya-codex}"
RELEASES_URL="${LAYA_RELEASES_URL:-https://github.com/$REPO/releases}"
MODEL_URL="${LAYA_MODEL_URL:-https://huggingface.co/tindang/laya-code/resolve/main}"
MODEL="${LAYA_MODEL:-auto}"

say() { printf 'laya-install: %s\n' "$*"; }
die() { printf 'laya-install: error: %s\n' "$*" >&2; exit 1; }

usage() {
    cat <<'EOF'
Usage: install.sh [--version vX.Y.Z] [--dir DIR] [--no-model | --model]

  --version   release tag to install (default: the latest release)
  --dir       where laya and moon go (default: ~/.local/bin)
  --no-model  skip the ~850 MB re-ranker download (laya then ranks lexically)
  --model     download the re-ranker even on Linux (it runs on CPU there, which is slow)
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --version) [ $# -ge 2 ] || die "--version needs a value"; VERSION="$2"; shift 2 ;;
        --dir) [ $# -ge 2 ] || die "--dir needs a value"; INSTALL_DIR="$2"; shift 2 ;;
        --no-model) MODEL=0; shift ;;
        --model) MODEL=1; shift ;;
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

# curl with retries and time limits; file:// URLs work too (used by the installer test).
fetch() {
    curl --fail --silent --show-error --location --retry 3 --retry-delay 2 \
        --connect-timeout 15 --max-time "${2:-600}" "$1"
}

os="$(uname -s)"
arch="$(uname -m)"
case "$os/$arch" in
    Darwin/arm64) target="aarch64-apple-darwin" ;;
    Linux/x86_64 | Linux/amd64) target="x86_64-unknown-linux-gnu" ;;
    *) die "no prebuilt laya for $os/$arch yet; build from source: https://github.com/$REPO#build" ;;
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
    fetch "$RELEASES_URL/download/$VERSION/$asset" >"$tmp/$asset" || die "download failed: $asset"
    fetch "$RELEASES_URL/download/$VERSION/$asset.sha256" 60 >"$tmp/$asset.sha256" ||
        die "download failed: $asset.sha256"
    want="$(cut -d' ' -f1 <"$tmp/$asset.sha256")"
    got="$(sha256 "$tmp/$asset")"
    [ -n "$want" ] && [ "$want" = "$got" ] || die "checksum mismatch for $asset (want $want, got $got)"
    tar -xzf "$tmp/$asset" -C "$tmp" || die "could not unpack $asset"
}

laya_pkg="laya-$VERSION-$target"
moon_pkg="moon-$VERSION-$target"
get_asset "$laya_pkg.tar.gz"
get_asset "$moon_pkg.tar.gz"
[ -x "$tmp/$laya_pkg/laya" ] || die "$laya_pkg.tar.gz has no laya binary"
[ -x "$tmp/$moon_pkg/moon" ] || die "$moon_pkg.tar.gz has no moon binary"
"$tmp/$laya_pkg/laya" --help >/dev/null 2>&1 || die "the downloaded laya does not run on this machine"

# Stop a running daemon first, so the next hook starts the new binaries.
if [ -x "$INSTALL_DIR/laya" ]; then
    "$INSTALL_DIR/laya" stop >/dev/null 2>&1 || true
fi

mkdir -p "$INSTALL_DIR" || die "cannot create $INSTALL_DIR"
for bin in "$laya_pkg/laya" "$moon_pkg/moon"; do
    name="${bin##*/}"
    cp "$tmp/$bin" "$INSTALL_DIR/.$name.new" || die "cannot write to $INSTALL_DIR"
    chmod 755 "$INSTALL_DIR/.$name.new"
    mv -f "$INSTALL_DIR/.$name.new" "$INSTALL_DIR/$name"
done
share="$LAYA_HOME/share"
mkdir -p "$share"
cp "$tmp/$moon_pkg/LICENSE" "$share/moon-LICENSE" 2>/dev/null || true
cp "$tmp/$moon_pkg/SOURCE" "$share/moon-SOURCE" 2>/dev/null || true
say "installed laya $VERSION and moon to $INSTALL_DIR"

if [ "$MODEL" = 1 ]; then
    dest="$LAYA_HOME/models/laya-code"
    say "fetching the laya-code re-ranker manifest"
    fetch "$MODEL_URL/MANIFEST.sha256" 60 >"$tmp/MANIFEST.sha256" || die "could not fetch the model manifest"
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
        fetch "$MODEL_URL/$file" 3600 >"$dest/$file.part" || die "download failed: model $file"
        got="$(sha256 "$dest/$file.part")"
        [ "$got" = "$want" ] || { rm -f "$dest/$file.part"; die "checksum mismatch for model $file"; }
        mv -f "$dest/$file.part" "$dest/$file"
    done <"$tmp/MANIFEST.sha256"
    say "model ready in $dest"
else
    say "skipped the model: laya ranks lexically (re-run with --model to add it)"
fi

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) say "note: $INSTALL_DIR is not on PATH; add it, e.g. export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac
cat <<EOF

Next, in each repository you use with Claude Code:
  laya init --repo /path/to/repo     # add laya's hooks and MCP server, then index the repo
  laya doctor --repo /path/to/repo   # check that everything is wired up
EOF
