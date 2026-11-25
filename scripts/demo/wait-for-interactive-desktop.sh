#!/usr/bin/env bash
set -Eeuo pipefail

if [[ $# -ne 6 ]]; then
  echo "usage: $0 WINLANE_BIN CONTROL_SOCKET MACHINE_NAME TIMEOUT_SECONDS DEMO_DIR PROBE_OUTPUT" >&2
  exit 2
fi

winlane_bin=$1
control_socket=$2
machine_name=$3
timeout_seconds=$4
demo_dir=$5
probe_output=$6

printf 'waiting up to %ss for interactive desktop UIA on %s\n' "$timeout_seconds" "$machine_name"
deadline=$((SECONDS + timeout_seconds))
checks=0
last_output=""
tree_file=$(mktemp "${TMPDIR:-/tmp}/winlane-ui-ready.XXXXXX")
trap 'rm -f -- "$tree_file"' EXIT
while (( SECONDS < deadline )); do
  checks=$((checks + 1))
  tree_args=$(printf '{"machine":"%s","max_depth":6,"max_nodes":1200}' "$machine_name")
  if python3 "$demo_dir/mcp_call.py" \
    --winlane-bin "$winlane_bin" \
    --control-socket "$control_socket" \
    --timeout-seconds 20 \
    --arguments "$tree_args" ui_tree >"$tree_file" 2>"$tree_file.err"; then
    cp -- "$tree_file" "$probe_output" 2>/dev/null || true
    if ready_output=$(python3 "$demo_dir/assert_interactive_desktop.py" "$tree_file" 2>&1); then
      printf '%s after %s check(s)\n' "$ready_output" "$checks"
      exit 0
    fi
    last_output=$ready_output
  else
    last_output=$(cat "$tree_file.err" 2>/dev/null || true)
  fi
  if (( checks % 5 == 0 )); then
    printf 'still waiting for interactive desktop UIA (%s checks; %s)\n' "$checks" "$last_output"
  fi
  sleep 2
done
printf 'wait-for-interactive-desktop error: desktop UIA did not become ready within %ss\n' "$timeout_seconds" >&2
printf 'last probe result: %s\n' "$last_output" >&2
exit 1
