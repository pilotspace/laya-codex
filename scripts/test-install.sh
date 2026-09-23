#!/bin/sh
# Tests install.sh against a fake release and a fake model repo on local disk (file:// URLs), so it
# runs offline and in CI. Checks: a clean install, an idempotent re-run, a tampered binary that
# must be rejected without touching the existing install, and an unsafe model manifest path.
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

# A release: laya and moon tarballs, each with a .sha256 beside it.
rel="$T/releases/download/$V"
mkdir -p "$rel" "$T/pkg/laya-$V-$target" "$T/pkg/moon-$V-$target"
printf '#!/bin/sh\necho "laya stub $*"\n' >"$T/pkg/laya-$V-$target/laya"
printf '#!/bin/sh\necho moon stub\n' >"$T/pkg/moon-$V-$target/moon"
chmod 755 "$T/pkg/laya-$V-$target/laya" "$T/pkg/moon-$V-$target/moon"
echo "GPL-3.0 stub" >"$T/pkg/moon-$V-$target/LICENSE"
echo "https://github.com/pilotspace/moon/tree/abc" >"$T/pkg/moon-$V-$target/SOURCE"
for p in laya moon; do
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
    LAYA_RELEASES_URL="file://$T/releases" LAYA_MODEL_URL="file://$model" LAYA_HOME="$T/home" \
        sh "$here/install.sh" --version "$V" --dir "$T/bin" "$@"
}

# 1. Clean install: both binaries, the model, and Moon's license and source pointer.
run --model >"$T/out1" 2>&1 || { cat "$T/out1"; fail "clean install exited non-zero"; }
[ -x "$T/bin/laya" ] && [ -x "$T/bin/moon" ] || fail "binaries not installed"
[ "$("$T/bin/moon")" = "moon stub" ] || fail "moon is not the release binary"
[ -f "$T/home/models/laya-code/tokenizer/tokenizer.json" ] || fail "model subdirectory file missing"
[ -f "$T/home/share/moon-LICENSE" ] && [ -f "$T/home/share/moon-SOURCE" ] || fail "moon license/source missing"
grep -q "laya init" "$T/out1" || fail "no next-step hint"
pass "clean install"

# 2. Re-run: succeeds and downloads no model file again.
run --model >"$T/out2" 2>&1 || { cat "$T/out2"; fail "re-run exited non-zero"; }
if grep -q "downloading model file" "$T/out2"; then fail "re-run downloaded model files again"; fi
pass "idempotent re-run"

# 3. Tampered binary: rejected, and the installed laya stays the old one.
printf '#!/bin/sh\necho evil\n' >"$T/pkg/laya-$V-$target/laya"
tar -C "$T/pkg" -czf "$rel/laya-$V-$target.tar.gz" "laya-$V-$target"
if run --no-model >"$T/out3" 2>&1; then fail "tampered tarball was accepted"; fi
grep -q "checksum mismatch" "$T/out3" || { cat "$T/out3"; fail "no checksum error"; }
[ "$("$T/bin/laya" x)" = "laya stub x" ] || fail "existing install was modified"
pass "checksum mismatch rejected"

# 4. A manifest path escaping the model directory is refused.
echo "$(sum "$rel/laya-$V-$target.tar.gz")  laya-$V-$target.tar.gz" >"$rel/laya-$V-$target.tar.gz.sha256"
echo "0000  ../../escape" >>"$model/MANIFEST.sha256"
if run --model >"$T/out4" 2>&1; then fail "unsafe manifest path was accepted"; fi
grep -q "unsafe path" "$T/out4" || { cat "$T/out4"; fail "no unsafe-path error"; }
[ ! -e "$T/home/escape" ] && [ ! -e "$T/escape" ] || fail "file written outside the model dir"
pass "unsafe manifest path refused"

# 5. A stalled download is abandoned quickly and retried, instead of waiting for the time limit.
if command -v python3 >/dev/null 2>&1; then
    printf '#!/bin/sh\necho "laya stub $*"\n' >"$T/pkg/laya-$V-$target/laya"
    tar -C "$T/pkg" -czf "$rel/laya-$V-$target.tar.gz" "laya-$V-$target"
    echo "$(sum "$rel/laya-$V-$target.tar.gz")  laya-$V-$target.tar.gz" >"$rel/laya-$V-$target.tar.gz.sha256"
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
    LAYA_RELEASES_URL="http://127.0.0.1:$(cat "$T/port")/releases" LAYA_STALL_SECONDS=2 \
        LAYA_HOME="$T/home" sh "$here/install.sh" --version "$V" --dir "$T/bin" --no-model >"$T/out5" 2>&1
    rc=$?
    elapsed=$(($(date +%s) - t0))
    kill "$srv" 2>/dev/null
    [ $rc -eq 0 ] || { cat "$T/out5"; fail "install over a stalling server failed"; }
    [ $elapsed -lt 60 ] || fail "stalled download took ${elapsed}s; it should be abandoned within seconds"
    pass "stalled download retried (${elapsed}s)"
else
    echo "skip - stalled download (no python3)"
fi

echo "all installer tests passed"
