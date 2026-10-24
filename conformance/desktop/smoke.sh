#!/bin/sh
set -eu
log_file=$(mktemp)
trap 'rm -f "$log_file"' EXIT
./target/debug/tachikoma-gui --demo --smoke-test >"$log_file" 2>&1 &
gui_pid=$!
trap 'kill "$gui_pid" 2>/dev/null || true; rm -f "$log_file"' EXIT
window_id=$(timeout 20 xdotool search --sync --name '^Tachikoma$' | head -1)
xdotool windowfocus --sync "$window_id"
xdotool key ctrl+f
xdotool type --delay 35 registry.example
xdotool key ctrl+a
xdotool type --delay 35 no-match
xdotool key ctrl+a BackSpace Escape ctrl+3 ctrl+1 Right Down Left
if ! wait "$gui_pid"; then cat "$log_file"; exit 1; fi
grep -F 'Tachikoma native smoke test complete' "$log_file"
