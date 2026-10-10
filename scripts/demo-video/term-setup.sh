# Sourced (hidden) at the start of each vhs tape: puts the recording shell
# inside the prx demo sandbox and keeps the screen clean.
source "${PRX_DEMO_DIR:-/tmp/prx-demo-video}/env.sh"
cd

# `prx` on screen is the sandboxed binary with stderr dropped, which hides the
# one-line "daemon not running" note (the demo sandbox has no daemon). stdout
# is prx's real output, byte for byte.
prx() { command prx "$@" 2>/dev/null; }

PS1='\[\e[38;2;76;150;255m\]$\[\e[0m\] '
clear
