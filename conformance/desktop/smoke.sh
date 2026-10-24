#!/bin/sh
set -eu
log_file=$(mktemp)
appearance_state=$(mktemp -d)
export XDG_STATE_HOME="$appearance_state"
trap 'rm -f "$log_file"; rm -rf "$appearance_state"' EXIT
./target/debug/tachikoma-gui --demo --appearance "${1:-light}" --smoke-test >"$log_file" 2>&1 &
gui_pid=$!
trap 'kill "$gui_pid" 2>/dev/null || true; rm -f "$log_file"; rm -rf "$appearance_state"' EXIT
window_id=$(timeout 30 xdotool search --sync --onlyvisible --name '^tachikoma$' | head -1)
[ -n "$window_id" ] || { cat "$log_file"; echo "Native X11 window missing" >&2; exit 1; }
xdotool windowfocus --sync "$window_id"
python3 - "$log_file" <<'PYCODE'
from pathlib import Path
import sys,time
log=Path(sys.argv[1])
deadline=time.monotonic()+30
while "Decision UI first frame rendered" not in log.read_text():
    if time.monotonic()>deadline:
        raise RuntimeError("Native window never rendered its first frame")
    time.sleep(0.1)
PYCODE
xdotool key ctrl+f
xdotool type --delay 35 registry.example
xdotool key ctrl+a
xdotool type --delay 35 no-match
xdotool key ctrl+a BackSpace Escape ctrl+3 ctrl+1 Right Down Left ctrl+shift+t ctrl+shift+t
if ! wait "$gui_pid"; then cat "$log_file"; exit 1; fi
cat "$log_file"
grep -F 'Decision UI native smoke test complete' "$log_file"
for action in Search Health Inbox FocusRight Next FocusLeft CycleAppearance; do
    grep -F "name: \"desktop::$action\"" "$log_file" >/dev/null
done

python3 - "$appearance_state/tachikoma/desktop/appearance.json" "${1:-light}" <<'PYCODE'
import json,sys
from pathlib import Path
expected={"light":"system","dark":"light"}[sys.argv[2]]
assert json.loads(Path(sys.argv[1]).read_text())["appearance"]==expected
assert Path(sys.argv[1]).stat().st_mode & 0o777==0o600
print("Appearance switching and persistence verified")
PYCODE
