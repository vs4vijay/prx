#!/usr/bin/env bash
# verify-upgrade.sh: repeatable check of the pre-brief -> PRX.app upgrade
# path (OpenSpec change prx-desktop-app-and-briefs, task 8.2).
#
# Everything runs in a throwaway sandbox: HOME, PRX_CONFIG, PRX_DATA_DIR and
# PRX_APP_SEARCH_DIRS point at a temp dir, `gh` is the tests/fixtures fake,
# and the agent is `sh`. The real HOME, config, state, LaunchAgents and any
# running daemon are never touched. `prx daemon install` on macOS only
# writes/removes plist files (it never calls launchctl), so no login item is
# loaded.
#
# The sandbox seeds a pre-change setup:
#   - config with an `executive_summary:` section and `review_prompt` set to
#     the verbatim legacy default (LEGACY_DEFAULT_PROMPT_V1 in src/prompt.rs)
#   - state.json records carrying legacy `exec_summary` strings
#   - legacy com.prx.daemon / com.prx.menubar plists
#   - a PRX.app bundle in the sandbox ~/Applications whose Helpers/prx is the
#     new build
# then runs the bundled CLI and checks: briefs migrated, the deprecation
# warning, the prompt resolving to the default, exactly one com.prx.app
# agent, and `prx doctor` passing with only the expected hints.
#
# Usage: scripts/verify-upgrade.sh            (builds prx with cargo)
#        PRX_BIN=/path/to/prx scripts/verify-upgrade.sh
#        KEEP_SANDBOX=1 ...                   (leave the sandbox for a look)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "skip: the PRX.app upgrade path is macOS-only" >&2
    exit 0
fi

if [[ -z "${PRX_BIN:-}" ]]; then
    (cd "$ROOT" && cargo build --quiet --bin prx)
    PRX_BIN="${CARGO_TARGET_DIR:-$ROOT/target}/debug/prx"
fi
[[ -x "$PRX_BIN" ]] || { echo "error: no prx binary at $PRX_BIN" >&2; exit 2; }

SANDBOX="$(mktemp -d "${TMPDIR:-/tmp}/prx-verify-upgrade.XXXXXX")"
cleanup() {
    if [[ -n "${KEEP_SANDBOX:-}" ]]; then
        echo "sandbox kept: $SANDBOX"
    else
        rm -rf "$SANDBOX"
    fi
}
trap cleanup EXIT

export HOME="$SANDBOX/home"
export PRX_CONFIG="$SANDBOX/config/config.yml"
export PRX_DATA_DIR="$SANDBOX/data"
export PRX_APP_SEARCH_DIRS="$HOME/Applications"
export FAKE_GH_DIR="$SANDBOX/gh"
export FAKE_GH_LOG="$SANDBOX/gh/calls.log"
export PATH="$SANDBOX/gh:$PATH"
unset XDG_CONFIG_HOME XDG_DATA_HOME
mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Applications" \
    "$(dirname "$PRX_CONFIG")" "$PRX_DATA_DIR" "$FAKE_GH_DIR"
install -m 0755 "$ROOT/tests/fixtures/gh" "$FAKE_GH_DIR/gh"

FAILS=0
pass() { echo "PASS  $*"; }
fail() { echo "FAIL  $*"; FAILS=$((FAILS + 1)); }
check() { # check <description> <haystack> <needle>
    if grep -qF -- "$3" <<<"$2"; then pass "$1"; else fail "$1 (missing: $3)"; fi
}

# --- PRX.app bundle holding the new build ---------------------------------
APP="$HOME/Applications/PRX.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Helpers"
printf '#!/bin/sh\nexit 0\n' >"$APP/Contents/MacOS/PRX"
chmod 0755 "$APP/Contents/MacOS/PRX"
cp "$PRX_BIN" "$APP/Contents/Helpers/prx"
PRX="$APP/Contents/Helpers/prx"

# --- pre-change config -----------------------------------------------------
legacy_prompt="$(awk '
    /^pub const LEGACY_DEFAULT_PROMPT_V1: &str = r#"/ {
        f = 1; sub(/^pub const LEGACY_DEFAULT_PROMPT_V1: &str = r#"/, "")
    }
    f && /^"#;/ { exit }
    f { print }' "$ROOT/src/prompt.rs")"
[[ -n "$legacy_prompt" ]] || { echo "error: LEGACY_DEFAULT_PROMPT_V1 not found" >&2; exit 2; }
{
    echo "defaults:"
    echo "  agent: pi"
    echo "  review_prompt: |"
    sed 's/^/    /; s/^    $//' <<<"$legacy_prompt"
    echo "  executive_summary:"
    echo "    enabled: true"
    echo "    timeout_seconds: 90"
    echo "agents:"
    echo "  pi:"
    echo "    command: sh"
    echo "    args: [\"-c\", \"true\"]"
    echo "repos:"
    echo "  - name: acme/widgets"
} >"$PRX_CONFIG"

# --- pre-change state (PrRecord with the legacy exec_summary string) --------
record() { # record <number> <state> <exec_summary json or null>
    cat <<JSON
    "acme/widgets#$1": {
      "repo": "acme/widgets", "number": $1, "title": "PR $1",
      "author": "octo", "head_branch": "feat-$1", "head_sha": "sha$1new",
      "is_draft": false, "gh_created_at": "2026-09-01T10:00:00Z",
      "state": "$2", "previous_state": null, "skip_reason": null,
      "snooze_until": null, "snooze_until_push": false,
      "review_anchor_at": "2026-09-01T10:00:00Z",
      "last_reviewed_sha": "sha$1old",
      "last_review_at": "2026-09-02T12:00:00Z",
      "last_verdict": "comment_only", "last_summary": "1 nit, no blockers",
      "blocking_count": 0, "nit_count": 1, "review_session": null,
      "review_attempts": 1, "diff_too_large": false,
      "exec_summary": $3,
      "updated_at": "2026-09-02T12:00:00Z"
    }
JSON
}
{
    echo '{ "schema_version": 1, "last_poll": null, "prs": {'
    record 13 reviewed_approved '"Adds rate limiting to the public API."'
    echo ','
    record 14 reviewed_changes_requested '"Moves config parsing into its own module."'
    echo ','
    record 15 reviewed_approved null
    echo '} }'
} >"$PRX_DATA_DIR/state.json"
cp "$PRX_DATA_DIR/state.json" "$SANDBOX/state.before.json"

# --- legacy launchd agents ---------------------------------------------------
legacy_plist() { # legacy_plist <label> <program args...>
    local label="$1"; shift
    {
        echo '<?xml version="1.0" encoding="UTF-8"?>'
        echo '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">'
        echo '<plist version="1.0"><dict>'
        echo "  <key>Label</key><string>$label</string>"
        echo '  <key>ProgramArguments</key><array>'
        for arg in "$@"; do echo "    <string>$arg</string>"; done
        echo '  </array>'
        echo '  <key>RunAtLoad</key><true/>'
        echo '  <key>KeepAlive</key><true/>'
        echo '</dict></plist>'
    } >"$HOME/Library/LaunchAgents/$label.plist"
}
mkdir -p "$HOME/.cargo/bin"
cp "$PRX_BIN" "$HOME/.cargo/bin/prx"
printf '#!/bin/sh\nexit 0\n' >"$HOME/.cargo/bin/prx-menubar"
chmod 0755 "$HOME/.cargo/bin/prx-menubar"
legacy_plist com.prx.daemon "$HOME/.cargo/bin/prx" daemon start --foreground
legacy_plist com.prx.menubar "$HOME/.cargo/bin/prx-menubar"

# --- run the new build -------------------------------------------------------
echo "== prx config show"
config_out="$("$PRX" config show 2>&1)" || fail "prx config show exited non-zero"
check "deprecation warning on config load" "$config_out" "executive_summary is deprecated"
check "executive_summary read as brief (timeout carried over)" \
    "$(sed -n '/^ *brief:/,/^ *[a-z_]*:$/p' <<<"$config_out")" "timeout_seconds: 90"

echo "== prx brief (local, no daemon)"
brief13="$("$PRX" brief acme/widgets#13 2>&1)" || fail "prx brief #13 exited non-zero: $brief13"
check "#13 exec_summary migrated to a brief" "$brief13" "Adds rate limiting to the public API."
check "#13 migrated brief carries dates" "$brief13" "created"
check "#13 migrated brief is stale (reviewed sha != head)" "$brief13" "stale"
brief14="$("$PRX" brief acme/widgets#14 2>&1)" || fail "prx brief #14 exited non-zero: $brief14"
check "#14 exec_summary migrated to a brief" "$brief14" "Moves config parsing into its own module."

status_json="$("$PRX" status --json 2>/dev/null)" || fail "prx status --json exited non-zero"
if python3 - "$status_json" <<'PY'
import json, sys
data = json.loads(sys.argv[1])
by_key = {p.get("key"): p for p in data}
b13 = (by_key.get("acme/widgets#13") or {}).get("brief") or {}
assert b13.get("text") == "Adds rate limiting to the public API.", b13
assert b13.get("created_at", "").startswith("2026-09-02T12:00:00"), b13
assert b13.get("head_sha") == "sha13old", b13
assert b13.get("source") == "review", b13
assert b13.get("stale") is True, b13
assert (by_key.get("acme/widgets#14") or {}).get("brief"), by_key.get("acme/widgets#14")
assert not (by_key.get("acme/widgets#15") or {}).get("brief"), by_key.get("acme/widgets#15")
PY
then pass "status --json exposes migrated briefs (dates from last review, source review)"
else fail "status --json brief shape"
fi

echo "== prx daemon install"
install_out="$("$PRX" daemon install 2>&1)" || fail "prx daemon install exited non-zero: $install_out"
echo "$install_out" | sed 's/^/    /'
agents="$(cd "$HOME/Library/LaunchAgents" && ls)"
if [[ "$agents" == "com.prx.app.plist" ]]; then
    pass "exactly one login agent remains: com.prx.app"
else
    fail "LaunchAgents after install: $(tr '\n' ' ' <<<"$agents")"
fi
app_plist="$(cat "$HOME/Library/LaunchAgents/com.prx.app.plist" 2>/dev/null || true)"
check "com.prx.app runs the bundle's PRX" "$app_plist" "<string>$APP/Contents/MacOS/PRX</string>"
check "com.prx.app starts --background" "$app_plist" "<string>--background</string>"

# A second install replaces in place.
"$PRX" daemon install >/dev/null 2>&1 || fail "second prx daemon install exited non-zero"
[[ "$(cd "$HOME/Library/LaunchAgents" && ls)" == "com.prx.app.plist" ]] \
    && pass "re-install keeps exactly one agent" || fail "re-install duplicated agents"

echo "== prx doctor"
doctor_rc=0
doctor_out="$("$PRX" doctor 2>&1)" || doctor_rc=$?
echo "$doctor_out" | sed 's/^/    /'
[[ $doctor_rc -eq 0 ]] && pass "doctor exits 0" || fail "doctor exited $doctor_rc"
check "doctor: prompt resolves to the default" "$doctor_out" \
    "defaults -> built-in default (saved copy of the previous default)"
check "doctor: executive_summary deprecation hint" "$doctor_out" \
    "hint: defaults.executive_summary is deprecated"
check "doctor: PRX.app login item" "$doctor_out" "PRX.app login item"
if grep -qi "leftover" <<<"$doctor_out"; then fail "doctor reports leftover agents"; else pass "doctor: no leftover legacy agents"; fi
if grep -q "manifest schema" <<<"$doctor_out"; then
    fail "doctor: legacy default prompt flagged as a custom schema prompt"
else
    pass "doctor: no custom-schema hint for the legacy default"
fi
# Every check line must be [ok] (hints are reported as ok).
unexpected="$(grep -E '^\s*\[' <<<"$doctor_out" | grep -vE '^\s*\[ok' || true)"
[[ -z "$unexpected" ]] && pass "doctor: only expected hints" || fail "doctor: unexpected lines: $unexpected"

echo
if [[ $FAILS -eq 0 ]]; then
    echo "upgrade path OK"
else
    echo "upgrade path: $FAILS check(s) failed"
    exit 1
fi
