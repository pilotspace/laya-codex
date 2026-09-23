#!/bin/sh
# Tests the Claude Code plugin in plugin/: manifest and hook/MCP config validity, and the
# wrapper scripts that locate the laya-codex binary (fail open, one install hint, no double hooks).
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
grep -q '${CLAUDE_PLUGIN_ROOT}/scripts/laya-codex-hook' "$P/hooks/hooks.json" || fail "hooks do not use the wrapper"
grep -q '"laya-codex": {' "$P/.mcp.json" || fail "the MCP server is not named laya-codex"
[ -x "$P/scripts/laya-codex-hook" ] && [ -x "$P/scripts/laya-codex-mcp" ] || fail "wrappers missing or not executable"
pass "config files"

# The plugin version follows the workspace version.
ws="$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/Cargo.toml" | head -1)"
grep -q "\"version\": \"$ws\"" "$P/.claude-plugin/plugin.json" || fail "plugin.json version is not $ws"
pass "plugin version matches Cargo.toml ($ws)"

# A stub laya-codex that records how it was called.
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
        sh "$P/scripts/laya-codex-hook"
}
start='{"session_id":"s","hook_event_name":"SessionStart","source":"startup"}'
prompt='{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"where is x"}'

# 2. No laya-codex anywhere: SessionStart shows one install hint; other events stay silent; exit 0.
out="$(hook "$start")" || fail "missing laya-codex: SessionStart exited non-zero"
echo "$out" | python3 -c 'import json,sys; m=json.load(sys.stdin)["systemMessage"]; assert "install.sh" in m and "laya-codex" in m, m' ||
    fail "missing laya-codex: no install hint on SessionStart (got: $out)"
out="$(hook "$prompt")" || fail "missing laya-codex: prompt hook exited non-zero"
[ -z "$out" ] || fail "missing laya-codex: prompt hook printed output: $out"
pass "fails open with a single install hint"

# 3. A 0.1.x `laya` binary is not used (clean break): still idle, still the install hint.
cp "$T/stub" "$T/home/.local/bin/laya"
: >"$T/calls"
out="$(hook "$prompt")"
[ -z "$out" ] && [ ! -s "$T/calls" ] || fail "the 0.1.x laya binary was run"
rm "$T/home/.local/bin/laya"
pass "ignores a 0.1.x laya binary"

# 4. laya-codex in ~/.local/bin (not on PATH): the event is passed to `laya-codex hook` unchanged.
cp "$T/stub" "$T/home/.local/bin/laya-codex"
: >"$T/calls"
out="$(hook "$prompt")"
[ "$out" = '{"stub": true}' ] || fail "stub output not passed through: $out"
grep -q "^args: hook$" "$T/calls" || fail "laya-codex not called as 'laya-codex hook': $(cat "$T/calls")"
grep -q '"prompt":"where is x"' "$T/calls" || fail "event not passed on stdin"
pass "finds ~/.local/bin/laya-codex and passes the event through"

# 5. LAYA_CODEX_BIN wins over everything else.
cp "$T/stub" "$T/bin/custom-laya-codex"
: >"$T/calls"
EXTRA="LAYA_CODEX_BIN=$T/bin/custom-laya-codex" hook "$prompt" >/dev/null
grep -q "^args: hook$" "$T/calls" || fail "LAYA_CODEX_BIN not used"
rm "$T/home/.local/bin/laya-codex"
: >"$T/calls"
EXTRA="LAYA_CODEX_BIN=$T/bin/custom-laya-codex" hook "$prompt" >/dev/null
grep -q "^args: hook$" "$T/calls" || fail "LAYA_CODEX_BIN not used when ~/.local/bin/laya-codex is absent"
pass "LAYA_CODEX_BIN override"

# 6. A repo already wired by `laya-codex init` keeps its own hooks; the plugin stays out of the
#    way. Leftover 0.1.x `laya hook` entries do not count: the plugin still runs.
cp "$T/stub" "$T/home/.local/bin/laya-codex"
mkdir -p "$T/proj/.claude"
for cmd in "laya-codex hook" "LAYA_CODEX_ADAPTIVE=1 /opt/homebrew/bin/laya-codex hook"; do
    printf '{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"%s"}]}]}}' "$cmd" \
        >"$T/proj/.claude/settings.local.json"
    : >"$T/calls"
    out="$(hook "$prompt")"
    [ -z "$out" ] && [ ! -s "$T/calls" ] || fail "plugin ran although laya-codex init hooks exist ($cmd)"
done
printf '{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"laya hook"}]}]}}' \
    >"$T/proj/.claude/settings.local.json"
: >"$T/calls"
out="$(hook "$prompt")"
[ "$out" = '{"stub": true}' ] || fail "plugin stepped aside for 0.1.x laya hooks: $out"
rm "$T/proj/.claude/settings.local.json"
pass "defers to laya-codex init hooks only"

# 7. The MCP wrapper: runs `laya-codex mcp`; without it it fails with an actionable message.
: >"$T/calls"
echo '{}' | env -i PATH="/usr/bin:/bin" HOME="$T/home" sh "$P/scripts/laya-codex-mcp" >/dev/null
grep -q "^args: mcp$" "$T/calls" || fail "laya-codex-mcp did not run 'laya-codex mcp'"
rm "$T/home/.local/bin/laya-codex"
cp "$T/stub" "$T/home/.local/bin/laya"
if env -i PATH="/usr/bin:/bin" HOME="$T/home" sh "$P/scripts/laya-codex-mcp" </dev/null 2>"$T/err"; then
    fail "laya-codex-mcp succeeded without laya-codex"
fi
grep -q "install.sh" "$T/err" || fail "laya-codex-mcp error has no install hint: $(cat "$T/err")"
rm "$T/home/.local/bin/laya"
pass "MCP wrapper"

# 8. Claude Code's own validator, when the CLI is installed (not on CI runners).
if command -v claude >/dev/null 2>&1; then
    (cd "$here" && claude plugin validate . >"$T/v1" 2>&1) || { cat "$T/v1"; fail "marketplace validation"; }
    (cd "$here" && claude plugin validate ./plugin >"$T/v2" 2>&1) || { cat "$T/v2"; fail "plugin validation"; }
    pass "claude plugin validate (marketplace and plugin)"
else
    echo "skip - claude plugin validate (claude CLI not installed)"
fi

echo "all plugin tests passed"
