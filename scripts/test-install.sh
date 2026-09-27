#!/bin/sh
# Tests install.sh against a fake release and a fake model repo on local disk (file:// URLs), so it
# runs offline and in CI. Checks: a clean install, an idempotent re-run, a tampered binary that
# must be rejected without touching the existing install, an unsafe model manifest path, a stalled
# download, --model-only (model without binaries), a model manifest that is not the pinned one, and
# that the installer's defaults pin a Hugging Face revision and the committed model manifest.
#
#   sh scripts/test-install.sh
set -eu

here="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT INT TERM
V=v9.9.9
case "$(uname -s)/$(uname -m)" in
    Darwin/arm64) target=aarch64-apple-darwin ;;
    Linux/x86_64) target=x86_64-unknown-linux-gnu ;;
    *) echo "skip: no prebuilt target for this platform"; exit 0 ;;
esac
if command -v sha256sum >/dev/null 2>&1; then sum() { sha256sum "$1" | cut -d' ' -f1; }; else sum() { shasum -a 256 "$1" | cut -d' ' -f1; }; fi

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "ok - $*"; }

# A release: laya-codex and moon tarballs, each with a .sha256 beside it.
rel="$T/releases/download/$V"
mkdir -p "$rel" "$T/pkg/laya-codex-$V-$target" "$T/pkg/moon-$V-$target"
printf '#!/bin/sh\necho "laya-codex stub $*"\n' >"$T/pkg/laya-codex-$V-$target/laya-codex"
printf '#!/bin/sh\necho moon stub\n' >"$T/pkg/moon-$V-$target/moon"
chmod 755 "$T/pkg/laya-codex-$V-$target/laya-codex" "$T/pkg/moon-$V-$target/moon"
echo "GPL-3.0 stub" >"$T/pkg/moon-$V-$target/LICENSE"
echo "https://github.com/pilotspace/moon/tree/abc" >"$T/pkg/moon-$V-$target/SOURCE"
for p in laya-codex moon; do
    tar -C "$T/pkg" -czf "$rel/$p-$V-$target.tar.gz" "$p-$V-$target"
    echo "$(sum "$rel/$p-$V-$target.tar.gz")  $p-$V-$target.tar.gz" >"$rel/$p-$V-$target.tar.gz.sha256"
done

# A model repo: a manifest of files, one in a subdirectory.
model="$T/model"
mkdir -p "$model/tokenizer"
echo weights >"$model/model.safetensors"
echo '{}' >"$model/tokenizer/tokenizer.json"
(cd "$model" && for f in model.safetensors tokenizer/tokenizer.json; do echo "$(sum "$f")  $f"; done) >"$model/MANIFEST.sha256"

run() {
    LAYA_CODEX_RELEASES_URL="file://$T/releases" LAYA_CODEX_MODEL_URL="file://$model" LAYA_CODEX_HOME="$T/home" \
        LAYA_CODEX_MODEL_MANIFEST_SHA256="$(sum "$model/MANIFEST.sha256")" \
        sh "$here/install.sh" --version "$V" --dir "$T/bin" "$@"
}

# 1. Clean install: both binaries, the model, and Moon's license and source pointer.
run --model >"$T/out1" 2>&1 || { cat "$T/out1"; fail "clean install exited non-zero"; }
[ -x "$T/bin/laya-codex" ] && [ -x "$T/bin/moon" ] || fail "binaries not installed"
[ ! -e "$T/bin/laya" ] || fail "a binary named laya was installed"
[ "$("$T/bin/moon")" = "moon stub" ] || fail "moon is not the release binary"
[ -f "$T/home/models/laya-code/tokenizer/tokenizer.json" ] || fail "model subdirectory file missing"
[ -f "$T/home/share/moon-LICENSE" ] && [ -f "$T/home/share/moon-SOURCE" ] || fail "moon license/source missing"
grep -q "laya-codex init" "$T/out1" || fail "no next-step hint"
grep -q "^laya-codex-install: installed laya-codex $V" "$T/out1" || { cat "$T/out1"; fail "no laya-codex-install: summary"; }
pass "clean install"

# 2. Re-run: succeeds and downloads no model file again.
run --model >"$T/out2" 2>&1 || { cat "$T/out2"; fail "re-run exited non-zero"; }
if grep -q "downloading model file" "$T/out2"; then fail "re-run downloaded model files again"; fi
pass "idempotent re-run"

# 3. Tampered binary: rejected, and the installed laya-codex stays the old one.
printf '#!/bin/sh\necho evil\n' >"$T/pkg/laya-codex-$V-$target/laya-codex"
tar -C "$T/pkg" -czf "$rel/laya-codex-$V-$target.tar.gz" "laya-codex-$V-$target"
if run --no-model >"$T/out3" 2>&1; then fail "tampered tarball was accepted"; fi
grep -q "checksum mismatch" "$T/out3" || { cat "$T/out3"; fail "no checksum error"; }
[ "$("$T/bin/laya-codex" x)" = "laya-codex stub x" ] || fail "existing install was modified"
pass "checksum mismatch rejected"

# 4. A manifest path escaping the model directory is refused.
echo "$(sum "$rel/laya-codex-$V-$target.tar.gz")  laya-codex-$V-$target.tar.gz" >"$rel/laya-codex-$V-$target.tar.gz.sha256"
echo "0000  ../../escape" >>"$model/MANIFEST.sha256"
if run --model >"$T/out4" 2>&1; then fail "unsafe manifest path was accepted"; fi
grep -q "unsafe path" "$T/out4" || { cat "$T/out4"; fail "no unsafe-path error"; }
[ ! -e "$T/home/escape" ] && [ ! -e "$T/escape" ] || fail "file written outside the model dir"
pass "unsafe manifest path refused"

# 5. A stalled download is abandoned quickly and retried, instead of waiting for the time limit.
if command -v python3 >/dev/null 2>&1; then
    printf '#!/bin/sh\necho "laya-codex stub $*"\n' >"$T/pkg/laya-codex-$V-$target/laya-codex"
    tar -C "$T/pkg" -czf "$rel/laya-codex-$V-$target.tar.gz" "laya-codex-$V-$target"
    echo "$(sum "$rel/laya-codex-$V-$target.tar.gz")  laya-codex-$V-$target.tar.gz" >"$rel/laya-codex-$V-$target.tar.gz.sha256"
    cat >"$T/stall.py" <<'EOF'
# Serves a directory over HTTP; the first request for each .tar.gz sends a few bytes and stalls.
import http.server, os, sys, threading, time
root, port_file = sys.argv[1], sys.argv[2]
seen, lock = set(), threading.Lock()
class H(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *a, **k):
        super().__init__(*a, directory=root, **k)
    def log_message(self, *a):
        pass
    def do_GET(self):
        with lock:
            first = self.path.endswith(".tar.gz") and self.path not in seen
            seen.add(self.path)
        if first:
            self.send_response(200)
            self.send_header("Content-Length", str(os.path.getsize(root + self.path)))
            self.end_headers()
            self.wfile.write(b"\x1f\x8b")
            self.wfile.flush()
            time.sleep(120)
            return
        super().do_GET()
s = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
s.daemon_threads = True
open(port_file, "w").write(str(s.server_address[1]))
s.serve_forever()
EOF
    python3 "$T/stall.py" "$T" "$T/port" &
    srv=$!
    i=0
    while [ ! -s "$T/port" ] && [ $i -lt 50 ]; do sleep 0.1; i=$((i + 1)); done
    t0=$(date +%s)
    LAYA_CODEX_RELEASES_URL="http://127.0.0.1:$(cat "$T/port")/releases" LAYA_CODEX_STALL_SECONDS=2 \
        LAYA_CODEX_HOME="$T/home" sh "$here/install.sh" --version "$V" --dir "$T/bin" --no-model >"$T/out5" 2>&1
    rc=$?
    elapsed=$(($(date +%s) - t0))
    kill "$srv" 2>/dev/null
    [ $rc -eq 0 ] || { cat "$T/out5"; fail "install over a stalling server failed"; }
    [ $elapsed -lt 60 ] || fail "stalled download took ${elapsed}s; it should be abandoned within seconds"
    pass "stalled download retried (${elapsed}s)"
else
    echo "skip - stalled download (no python3)"
fi

# 6. --model-only: fetches and verifies just the model; no release lookup, no binaries touched.
rm -rf "$T/home2" "$T/bin2"
grep -v escape "$model/MANIFEST.sha256" >"$T/manifest" && mv "$T/manifest" "$model/MANIFEST.sha256"
LAYA_CODEX_RELEASES_URL="file://$T/nonexistent" LAYA_CODEX_MODEL_URL="file://$model" LAYA_CODEX_HOME="$T/home2" \
    LAYA_CODEX_MODEL_MANIFEST_SHA256="$(sum "$model/MANIFEST.sha256")" \
    sh "$here/install.sh" --dir "$T/bin2" --model-only >"$T/out6" 2>&1 && rc=0 || rc=$?
[ "$rc" -eq 0 ] || { cat "$T/out6"; fail "--model-only exited non-zero"; }
[ -f "$T/home2/models/laya-code/tokenizer/tokenizer.json" ] || fail "--model-only did not fetch the model"
[ ! -e "$T/bin2" ] || fail "--model-only installed binaries"
if grep -q "downloading laya-codex-" "$T/out6"; then fail "--model-only downloaded a release asset"; fi
pass "--model-only"

# 7. Upgrading over a 0.1.x install: the old `laya` binary is left alone (not renamed, not
#    removed) and the installer says how to clean it up.
printf '#!/bin/sh\necho "old laya $*" >>"%s/old-calls"\n' "$T" >"$T/bin/laya"
chmod 755 "$T/bin/laya"
run --no-model >"$T/out7" 2>&1 || { cat "$T/out7"; fail "install over 0.1.x exited non-zero"; }
[ -x "$T/bin/laya" ] || fail "the 0.1.x laya binary was removed"
[ ! -e "$T/old-calls" ] || fail "the installer ran the 0.1.x laya binary"
grep -q "rm $T/bin/laya" "$T/out7" || { cat "$T/out7"; fail "no hint to remove the 0.1.x laya binary"; }
pass "upgrade over 0.1.x leaves a removal hint"

# 8. A model manifest other than the pinned one is refused before any model file is fetched.
rm -rf "$T/home3"
LAYA_CODEX_RELEASES_URL="file://$T/nonexistent" LAYA_CODEX_MODEL_URL="file://$model" LAYA_CODEX_HOME="$T/home3" \
    LAYA_CODEX_MODEL_MANIFEST_SHA256="0000000000000000000000000000000000000000000000000000000000000000" \
    sh "$here/install.sh" --dir "$T/bin3" --model-only >"$T/out8" 2>&1 && rc=0 || rc=$?
[ "$rc" -ne 0 ] || fail "a manifest that is not the pinned one was accepted"
grep -q "model manifest checksum mismatch" "$T/out8" || { cat "$T/out8"; fail "no manifest checksum error"; }
[ ! -e "$T/home3/models/laya-code/model.safetensors" ] || fail "a model file was fetched from an unpinned manifest"
pass "unpinned model manifest refused"

# 9. The defaults pin one Hugging Face revision of laya-code (a commit SHA, not a branch) and the
#    manifest committed in release/hf-laya-code, so a later upload to the model repo cannot change
#    what this installer downloads.
# shellcheck disable=SC2016 # the ${...} below is install.sh's text, matched literally
rev="$(sed -n 's/^MODEL_REVISION="${LAYA_CODEX_MODEL_REVISION:-\([^}]*\)}"$/\1/p' "$here/install.sh")"
# shellcheck disable=SC2016
pin="$(sed -n 's/^MODEL_MANIFEST_SHA256="${LAYA_CODEX_MODEL_MANIFEST_SHA256:-\([^}]*\)}"$/\1/p' "$here/install.sh")"
[ "$pin" = "$(sum "$here/release/hf-laya-code/MANIFEST.sha256")" ] ||
    fail "install.sh pins manifest '$pin', not the sha256 of release/hf-laya-code/MANIFEST.sha256"
printf '%s' "$rev" | grep -Eq '^[0-9a-f]{40}$' ||
    fail "install.sh does not pin a Hugging Face commit SHA of laya-code (MODEL_REVISION='$rev')"
pass "defaults pin revision $rev and the committed model manifest"

echo "all installer tests passed"
