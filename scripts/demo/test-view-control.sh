#!/usr/bin/env bash
set -Eeuo pipefail
root=$(mktemp -d "${TMPDIR:-/tmp}/winlane-view-control-test.XXXXXX")
trap 'rm -rf "$root"; kill ${supervisor_pid:-} ${child_watch:-} >/dev/null 2>&1 || true' EXIT
cat >"$root/fake-view" <<'EOF'
#!/bin/sh
printf 'ready\n'
while :; do sleep 1; done
EOF
chmod +x "$root/fake-view"
# Extract the production helpers rather than duplicating them.
awk '/^atomic_write\(\)/,/^}/' scripts/demo/drive-demo.sh >"$root/functions"
awk '/^stop_view\(\)/,/^}/' scripts/demo/drive-demo.sh >>"$root/functions"
view_state="$root/state"
. "$root/functions"
scripts/demo/view-supervisor.sh --winlane-bin "$root/fake-view" --control-socket "$root/control.sock" --machine fake --state-file "$view_state" >"$root/supervisor.log" 2>&1 &
supervisor_pid=$!
atomic_write "$view_state" 1
for _ in {1..50}; do grep -q 'starting winlane view' "$root/supervisor.log" 2>/dev/null && break; sleep 0.1; done
grep -q 'starting winlane view' "$root/supervisor.log"
(stop_view; printf 'machine-stop\n' >>"$root/order") &
stopper=$!
for _ in {1..100}; do [[ -f ${view_state}.stopped ]] && break; sleep 0.05; done
[[ -f ${view_state}.stopped ]]
wait "$stopper"
[[ $(cat "$root/order") == machine-stop ]]
! pgrep -f "$root/fake-view" >/dev/null
repeat_start=$SECONDS
stop_view
(( SECONDS - repeat_start < 2 ))
cat >"$root/tree-absent.json" <<'EOF'
{"ok":true,"truncated":false,"root":{"id":"0","children":[]}}
EOF
[[ -z $(python3 scripts/demo/find_notepad_popup_button.py "$root/tree-absent.json") ]]
cat >"$root/tree-truncated.json" <<'EOF'
{"ok":true,"truncated":true,"root":{"id":"0","children":[]}}
EOF
if python3 scripts/demo/find_notepad_popup_button.py "$root/tree-truncated.json" >/dev/null 2>&1; then
  exit 1
fi
cat >"$root/tree-popup.json" <<'EOF'
{"ok":true,"truncated":false,"root":{"id":"0","children":[{"id":"0/1","name":"Got it","controlType":"Button","children":[]}]}}
EOF
[[ $(python3 scripts/demo/find_notepad_popup_button.py "$root/tree-popup.json") == '0/1' ]]
printf 'view control stop/ack regression passed\n'
