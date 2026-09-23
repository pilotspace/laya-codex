#!/bin/sh
# Tests the Claude Code plugin in plugin/: manifest and hook/MCP config validity, and the
# wrapper scripts that locate the laya binary (fail open, one install hint, no double hooks).
#
#   sh scripts/test-plugin.sh
set -eu

here="$(cd "$(dirname "$0")/.." && pwd)"
P="$here/plugin"
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT INT TERM
fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "ok - $*"; }

# 1. Every JSON file parses; hooks point at the wrapper through ${CLAUDE_PLUGIN_ROOT}.
for f in "$here/.claude-plugin/marketplace.json" "$P/.claude-plugin/plugin.json" "$P/hooks/hooks.json" "$P/.mcp.json"; do
    python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$f" || fail "invalid JSON: $f"
done
grep -q '${CLAUDE_PLUGIN_ROOT}/scripts/laya-hook' "$P/hooks/hooks.json" || fail "hooks do not use the wrapper"
[ -x "$P/scripts/laya-hook" ] && [ -x "$P/scripts/laya-mcp" ] || fail "wrappers missing or not executable"
pass "config files"

# The plugin version follows the workspace version.
ws="$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/Cargo.toml" | head -1)"
grep -q "\"version\": \"$ws\"" "$P/.claude-plugin/plugin.json" || fail "plugin.json version is not $ws"
pass "plugin version matches Cargo.toml ($ws)"

# A stub laya that records how it was called.
mkdir -p "$T/home/.local/bin" "$T/bin" "$T/proj"
cat >"$T/stub" <<EOF
#!/bin/sh
echo "args: \$*" >>"$T/calls"
cat >>"$T/calls"
echo '{"stub": true}'
EOF
chmod 755 "$T/stub"
hook() { # hook <event json> [extra env assignments via env]
    printf '%s' "$1" | env -i PATH="/usr/bin:/bin" HOME="$T/home" CLAUDE_PROJECT_DIR="$T/proj" ${EXTRA:-} \
        sh "$P/scripts/laya-hook"
}
start='{"session_id":"s","hook_event_name":"SessionStart","source":"startup"}'
prompt='{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"where is x"}'

# 2. No laya anywhere: SessionStart shows one install hint; other events stay silent; exit 0.
out="$(hook "$start")" || fail "missing laya: SessionStart exited non-zero"
echo "$out" | python3 -c 'import json,sys; m=json.load(sys.stdin)["systemMessage"]; assert "install.sh" in m, m' ||
    fail "missing laya: no install hint on SessionStart (got: $out)"
out="$(hook "$prompt")" || fail "missing laya: prompt hook exited non-zero"
[ -z "$out" ] || fail "missing laya: prompt hook printed output: $out"
pass "fails open with a single install hint"

# 3. laya in ~/.local/bin (not on PATH): the event is passed through to `laya hook` unchanged.
cp "$T/stub" "$T/home/.local/bin/laya"
out="$(hook "$prompt")"
[ "$out" = '{"stub": true}' ] || fail "stub output not passed through: $out"
grep -q "^args: hook$" "$T/calls" || fail "laya not called as 'laya hook': $(cat "$T/calls")"
grep -q '"prompt":"where is x"' "$T/calls" || fail "event not passed on stdin"
pass "finds ~/.local/bin/laya and passes the event through"

# 4. LAYA_BIN wins over everything else.
cp "$T/stub" "$T/bin/custom-laya"
: >"$T/calls"
EXTRA="LAYA_BIN=$T/bin/custom-laya" hook "$prompt" >/dev/null
grep -q "^args: hook$" "$T/calls" || fail "LAYA_BIN not used"
rm "$T/home/.local/bin/laya"
: >"$T/calls"
EXTRA="LAYA_BIN=$T/bin/custom-laya" hook "$prompt" >/dev/null
grep -q "^args: hook$" "$T/calls" || fail "LAYA_BIN not used when ~/.local/bin/laya is absent"
pass "LAYA_BIN override"

# 5. A repo already wired by `laya init` keeps its own hooks; the plugin stays out of the way.
cp "$T/stub" "$T/home/.local/bin/laya"
mkdir -p "$T/proj/.claude"
printf '{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"laya hook"}]}]}}' \
    >"$T/proj/.claude/settings.local.json"
: >"$T/calls"
out="$(hook "$prompt")"
[ -z "$out" ] && [ ! -s "$T/calls" ] || fail "plugin ran although laya init hooks exist"
rm "$T/proj/.claude/settings.local.json"
pass "defers to laya init hooks"

# 6. The MCP wrapper: runs `laya mcp`; without laya it fails with an actionable message.
: >"$T/calls"
echo '{}' | env -i PATH="/usr/bin:/bin" HOME="$T/home" sh "$P/scripts/laya-mcp" >/dev/null
grep -q "^args: mcp$" "$T/calls" || fail "laya-mcp did not run 'laya mcp'"
rm "$T/home/.local/bin/laya"
if env -i PATH="/usr/bin:/bin" HOME="$T/home" sh "$P/scripts/laya-mcp" </dev/null 2>"$T/err"; then
    fail "laya-mcp succeeded without laya"
fi
grep -q "install.sh" "$T/err" || fail "laya-mcp error has no install hint: $(cat "$T/err")"
pass "MCP wrapper"

# 7. Claude Code's own validator, when the CLI is installed (not on CI runners).
if command -v claude >/dev/null 2>&1; then
    (cd "$here" && claude plugin validate . >"$T/v1" 2>&1) || { cat "$T/v1"; fail "marketplace validation"; }
    (cd "$here" && claude plugin validate ./plugin >"$T/v2" 2>&1) || { cat "$T/v2"; fail "plugin validation"; }
    pass "claude plugin validate (marketplace and plugin)"
else
    echo "skip - claude plugin validate (claude CLI not installed)"
fi

echo "all plugin tests passed"
