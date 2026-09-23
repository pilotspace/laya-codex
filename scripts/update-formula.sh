#!/bin/sh
# Bump the Homebrew formula to a published release: rewrites its release URLs (Homebrew reads the
# version from them) and every sha256 (read from the release's <asset>.sha256 files), then checks
# the result.
#
#   scripts/update-formula.sh 0.1.3            # or v0.1.3
#   scripts/update-formula.sh 0.1.3 path/to/laya.rb
#
# The formula is only replaced once every checksum has been fetched and validated, so a failed
# run leaves it untouched. LAYA_RELEASES_URL points at a mirror (or a file:// tree in tests).
set -eu

REPO="pilotspace/laya-codex"
RELEASES_URL="${LAYA_RELEASES_URL:-https://github.com/$REPO/releases}"

die() { printf 'update-formula: error: %s\n' "$*" >&2; exit 1; }

[ $# -ge 1 ] || die "usage: $0 <version> [formula]"
new="${1#v}"
case "$new" in
    [0-9]*.[0-9]*.[0-9]*) ;;
    *) die "version must look like 1.2.3 (got '$1')" ;;
esac
formula="${2:-$(cd "$(dirname "$0")/.." && pwd)/packaging/homebrew/laya.rb}"
[ -f "$formula" ] || die "no formula at $formula"

old="$(sed -n 's|^ *url "[^"]*/releases/download/v\([^/]*\)/.*|\1|p' "$formula" | head -n 1)"
[ -n "$old" ] || die "cannot find a release URL in $formula"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

# 1. URLs: every "v<old>" in a release URL becomes "v<new>".
sed -e "/releases\/download\//s/v$old/v$new/g" "$formula" >"$tmp/stage1"

# 2. The sha256 of each release asset the formula downloads.
: >"$tmp/sums"
sed -n 's|^ *url "[^"]*/releases/download/[^/]*/\([^"]*\)"$|\1|p' "$tmp/stage1" >"$tmp/assets"
while read -r asset; do
    printf 'update-formula: %s\n' "$asset"
    curl --fail --silent --show-error --location --retry 3 --connect-timeout 15 --max-time 60 \
        --output "$tmp/one" "$RELEASES_URL/download/v$new/$asset.sha256" ||
        die "cannot fetch $asset.sha256 (is v$new published?)"
    sum="$(cut -d' ' -f1 <"$tmp/one")"
    printf '%s' "$sum" | grep -Eq '^[0-9a-f]{64}$' || die "bad checksum for $asset: '$sum'"
    printf '%s %s\n' "$asset" "$sum" >>"$tmp/sums"
done <"$tmp/assets"
[ -s "$tmp/sums" ] || die "no release URLs found in $formula"

# 3. Put each checksum on the sha256 line that follows its url line.
awk -v sums="$tmp/sums" '
    BEGIN { while ((getline line < sums) > 0) { split(line, f, " "); sum[f[1]] = f[2] } }
    /^ *url "/ { n = split($0, p, "/"); a = p[n]; sub(/"$/, "", a); want = sum[a] }
    /^ *sha256 "/ && want != "" { sub(/"[0-9a-f]*"/, "\"" want "\""); want = "" }
    { print }
' "$tmp/stage1" >"$tmp/stage2"

# 4. Sanity: the new URLs are there, no old URL survives, every checksum landed.
grep -q "download/v$new/" "$tmp/stage2" || die "URLs not updated"
if grep -q "download/v$old/" "$tmp/stage2" && [ "$old" != "$new" ]; then die "stale URL left"; fi
while read -r asset sum; do
    grep -q "\"$sum\"" "$tmp/stage2" || die "checksum for $asset not written"
done <"$tmp/sums"

cp "$tmp/stage2" "$formula"
printf 'update-formula: %s -> %s in %s\n' "$old" "$new" "$formula"
