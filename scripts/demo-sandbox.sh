#!/usr/bin/env bash
# demo-sandbox.sh: a throwaway prx setup for screenshots and demo recordings,
# filled with one fictional team (the same cast the window preview serves:
# menubar/settings-ui-tests/preview-mock.js). Nothing in it comes from, or
# touches, your real prx: HOME, PRX_CONFIG, PRX_DATA_DIR and
# PRX_APP_SEARCH_DIRS point into the sandbox, `gh` is the tests/fixtures
# fake, and the review engines are a stub that answers with canned reviews.
#
# Usage:
#   scripts/demo-sandbox.sh [DIR]         build or rebuild the sandbox
#                                         (DIR: $PRX_DEMO_DIR, else /tmp/prx-demo)
#   PRX_BIN=/path/to/prx scripts/demo-sandbox.sh    use this prx (default:
#                                         cargo build, target/debug/prx)
#   PRX_DEMO_TZ=America/New_York ...      the timezone prx shows times in
#                                         inside the sandbox (default
#                                         Europe/Berlin)
#
# Then, with DIR=/tmp/prx-demo:
#   $DIR/prx status                       one command inside the sandbox
#   source $DIR/env.sh                    a whole shell inside it (use a
#                                         throwaway terminal: it changes HOME,
#                                         PATH and SHELL), then `prx status`,
#                                         `prx history`, `prx usage`, ...
#   $DIR/run-app [--background]           the native menu bar app inside it
#                                         (needs target/debug/prx-menubar)
#
# The sandbox is laid out like a small filesystem root, so paths on screen
# read naturally: DIR/Users/maya is HOME, DIR/opt/homebrew/bin holds gh and
# two of the engines. Timestamps are relative to the moment the script runs;
# run it again for a fresh "today".
#
# Why run-app and not a plain launch: the app asks $SHELL for the login
# PATH and puts that first (menubar/src/shellenv.rs), so a real gh on the
# login PATH would beat the fake one and list real repositories. run-app
# starts the app with a clean environment whose SHELL is a stub that answers
# with the sandbox PATH, and with CFFIXED_USER_HOME so AppKit and WebKit
# keep their files in the sandbox too.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SANDBOX="${1:-${PRX_DEMO_DIR:-/tmp/prx-demo}}"
MARKER=".prx-demo-sandbox"
LOGIN="maya"

command -v node >/dev/null || { echo "error: node is needed to generate the demo data" >&2; exit 2; }

if [[ -z "${PRX_BIN:-}" ]]; then
    (cd "$ROOT" && cargo build --quiet --locked --bin prx)
    PRX_BIN="${CARGO_TARGET_DIR:-$ROOT/target}/debug/prx"
fi
[[ -x "$PRX_BIN" ]] || { echo "error: no prx binary at $PRX_BIN" >&2; exit 2; }
PRX_BIN="$(cd "$(dirname "$PRX_BIN")" && pwd)/$(basename "$PRX_BIN")"

# Only ever replace a directory this script made, and never under a running
# demo daemon (it would keep writing into the new state).
if [[ -e "$SANDBOX" ]]; then
    [[ -f "$SANDBOX/$MARKER" ]] || { echo "error: $SANDBOX exists and is not a prx demo sandbox; choose another directory" >&2; exit 2; }
    for pidfile in "$SANDBOX/Users/$LOGIN/.local/share/prx/daemon.pid" "$SANDBOX/Users/$LOGIN/.local/share/prx/app.pid"; do
        if [[ -f "$pidfile" ]] && kill -0 "$(tr -dc '0-9' <"$pidfile")" 2>/dev/null; then
            echo "error: the demo app or daemon is still running in $SANDBOX (pid $(cat "$pidfile")); quit it first" >&2
            exit 2
        fi
    done
    rm -rf "$SANDBOX"
fi
mkdir -p "$SANDBOX"
SANDBOX="$(cd "$SANDBOX" && pwd)"
touch "$SANDBOX/$MARKER"

DEMO_HOME="$SANDBOX/Users/$LOGIN"
DEMO_CONFIG="$DEMO_HOME/.config/prx/config.yml"
DEMO_DATA="$DEMO_HOME/.local/share/prx"
BREW_BIN="$SANDBOX/opt/homebrew/bin"
LOCAL_BIN="$DEMO_HOME/.local/bin"
GH_DIR="$SANDBOX/var/gh"
AGENT_DIR="$SANDBOX/var/reviews"
DEMO_PATH="$BREW_BIN:$LOCAL_BIN:/usr/bin:/bin:/usr/sbin:/sbin"
mkdir -p "$DEMO_HOME/Library/LaunchAgents" "$DEMO_HOME/Applications" "$(dirname "$DEMO_CONFIG")" \
    "$DEMO_DATA" "$BREW_BIN" "$LOCAL_BIN" "$GH_DIR" "$AGENT_DIR" "$SANDBOX/bin" "$SANDBOX/tmp"

# --- config, state, history, gh fixtures, canned reviews ---------------------
node "$ROOT/scripts/demo-seed.js" "$DEMO_CONFIG" "$DEMO_DATA" "$GH_DIR" "$AGENT_DIR"

# --- gh: the test fake, plus the two answers it lacks -------------------------
install -m 0755 "$ROOT/tests/fixtures/gh" "$SANDBOX/bin/gh-fixture"
cat >"$BREW_BIN/gh" <<EOF
#!/bin/sh
# Demo gh: no network. \`auth status\` and the repo picker's \`api user/repos\`
# are answered here; everything else by the prx test fake.
export FAKE_GH_DIR="$GH_DIR" FAKE_GH_LOG="$GH_DIR/calls.log"
if [ "\${1:-}" = "auth" ] && [ "\${2:-}" = "status" ]; then
  printf '%s\n' "\$*" >>"\$FAKE_GH_LOG"
  echo "github.com"
  echo "  ✓ Logged in to github.com account $LOGIN (keyring)"
  echo "  - Active account: true"
  echo "  - Git operations protocol: ssh"
  echo "  - Token scopes: 'read:org', 'repo', 'workflow'"
  exit 0
fi
if [ "\${1:-}" = "api" ]; then
  case "\${2:-}" in
    user/repos\\?*page=1) printf '%s\n' "\$*" >>"\$FAKE_GH_LOG"; cat "$GH_DIR/repos.tsv"; exit 0 ;;
    user/repos\\?*) exit 0 ;;
  esac
fi
exec "$SANDBOX/bin/gh-fixture" "\$@"
EOF
chmod 0755 "$BREW_BIN/gh"

# --- review engines: one stub under three names -------------------------------
# A review run (prx sets PRX_MANIFEST_PATH, PRX_REPO, PRX_PR_NUMBER) waits a
# little, then writes the PR's canned manifest; `claude` also reports usage
# the way Claude Code's JSON output does. Any other run prints a one-line
# brief. PRX_DEMO_REVIEW_SECONDS sets how long a review "takes" (default 20).
cat >"$SANDBOX/bin/demo-agent" <<EOF
#!/bin/sh
name="\$(basename "\$0")"
if [ -z "\${PRX_MANIFEST_PATH:-}" ]; then
  case " \$* " in *" --version "*) echo "\$name 1.0.0 (prx demo stub)"; exit 0 ;; esac
  [ "\$name" = "codex" ] && cat >/dev/null
  echo "A small, self-contained change with tests for the new behaviour. Nothing in it touches stored data or a public API."
  exit 0
fi
[ "\$name" = "codex" ] && cat >/dev/null
sleep "\${PRX_DEMO_REVIEW_SECONDS:-20}"
canned="$AGENT_DIR/\$(printf '%s' "\${PRX_REPO:-}" | tr '/' '_')#\${PRX_PR_NUMBER:-}.json"
[ -f "\$canned" ] || canned="$AGENT_DIR/default.json"
mkdir -p "\$(dirname "\$PRX_MANIFEST_PATH")"
cp "\$canned" "\$PRX_MANIFEST_PATH"
if [ "\$name" = "claude" ]; then
  echo '{"type":"result","subtype":"success","is_error":false,"duration_ms":20000,"num_turns":11,"result":"Wrote the review manifest.","session_id":"5d0c7a1e-9b42-4f6a-a3d8-71c2e9b4f605","total_cost_usd":0.2874,"usage":{"input_tokens":212,"cache_creation_input_tokens":21480,"cache_read_input_tokens":318420,"output_tokens":3890}}'
fi
exit 0
EOF
chmod 0755 "$SANDBOX/bin/demo-agent"
ln -s "$SANDBOX/bin/demo-agent" "$LOCAL_BIN/claude"
ln -s "$SANDBOX/bin/demo-agent" "$BREW_BIN/codex"
ln -s "$SANDBOX/bin/demo-agent" "$BREW_BIN/pi"

# --- the app's login-shell stand-in (see the header) --------------------------
cat >"$SANDBOX/bin/login-shell" <<'EOF'
#!/bin/sh
# Answers prx-menubar's `$SHELL -ilc` PATH probe with the sandbox PATH.
printf '%s%s%s' '__PRX_PATH__' "$PATH" '__PRX_PATH__'
EOF
chmod 0755 "$SANDBOX/bin/login-shell"

# --- prx on the sandbox PATH -------------------------------------------------
ln -s "$PRX_BIN" "$LOCAL_BIN/prx"

# --- env.sh, the one-command wrapper, the app launcher ------------------------
cat >"$SANDBOX/env.sh" <<EOF
# prx demo sandbox: source this in a throwaway shell.
export HOME="$DEMO_HOME"
export CFFIXED_USER_HOME="$DEMO_HOME"
export USER="$LOGIN" LOGNAME="$LOGIN"
export PRX_CONFIG="$DEMO_CONFIG"
export PRX_DATA_DIR="$DEMO_DATA"
export PRX_APP_SEARCH_DIRS="$DEMO_HOME/Applications"
export FAKE_GH_DIR="$GH_DIR"
export FAKE_GH_LOG="$GH_DIR/calls.log"
export PATH="$DEMO_PATH"
export SHELL="$SANDBOX/bin/login-shell"
export TMPDIR="$SANDBOX/tmp"
# The fictional team's timezone, so no output shows this machine's.
export TZ="${PRX_DEMO_TZ:-Europe/Berlin}"
# The brief engine's key variable: a placeholder (its base_url is a closed
# local port, so no LLM API is ever called from the sandbox).
export ANTHROPIC_API_KEY="demo-sandbox-not-a-key"
unset XDG_CONFIG_HOME XDG_DATA_HOME GH_TOKEN GITHUB_TOKEN GH_HOST GH_CONFIG_DIR
EOF

cat >"$SANDBOX/prx" <<EOF
#!/bin/sh
# Run one prx command inside the demo sandbox: $SANDBOX/prx status
exec /usr/bin/env -i TERM="\${TERM:-xterm-256color}" LANG="\${LANG:-en_US.UTF-8}" /bin/sh -c '. "$SANDBOX/env.sh" && exec prx "\$@"' prx "\$@"
EOF
chmod 0755 "$SANDBOX/prx"

MENUBAR_BIN="$(dirname "$PRX_BIN")/prx-menubar"
cat >"$SANDBOX/run-app" <<EOF
#!/bin/sh
# Start the native menu bar app inside the demo sandbox (clean environment;
# it spawns its own sandboxed daemon from the prx beside it). Quit it from
# its menu ("Quit PRX"), which also stops that daemon.
[ -x "$MENUBAR_BIN" ] || { echo "error: build it first: cargo build -p prx-menubar" >&2; exit 2; }
exec /usr/bin/env -i TERM="\${TERM:-xterm-256color}" LANG="\${LANG:-en_US.UTF-8}" /bin/sh -c '. "$SANDBOX/env.sh" && exec "$MENUBAR_BIN" "\$@"' prx-menubar "\$@"
EOF
chmod 0755 "$SANDBOX/run-app"

cat <<EOF

prx demo sandbox ready: $SANDBOX
  one command:   $SANDBOX/prx status
  a shell:       source $SANDBOX/env.sh     (throwaway terminal; then: prx status)
  native app:    $SANDBOX/run-app           (never start prx-menubar in the sandbox any other way)
Try: status, history, usage, insights, brief acme/web#1288, doctor, repos list, config show
EOF
