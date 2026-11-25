#!/usr/bin/env bash
set -Eeuo pipefail

usage() {
  cat >&2 <<'USAGE'
usage: drive-demo.sh --winlane-bin PATH --demo-dir PATH --control-socket PATH --machine-name NAME --golden-image PATH --evidence-dir PATH --output-dir PATH --work-dir PATH --view-pane PANE
USAGE
}

winlane_bin=""
demo_dir=""
control_socket=""
machine_name=""
golden_image=""
evidence_dir=""
output_dir=""
work_dir=""
view_pane=""

while (($#)); do
  if [[ $# -lt 2 ]]; then
    echo "drive-demo error: $1 requires a value" >&2
    usage
    exit 2
  fi
  case "$1" in
    --winlane-bin) winlane_bin=$2; shift 2 ;;
    --demo-dir) demo_dir=$2; shift 2 ;;
    --control-socket) control_socket=$2; shift 2 ;;
    --machine-name) machine_name=$2; shift 2 ;;
    --golden-image) golden_image=$2; shift 2 ;;
    --evidence-dir) evidence_dir=$2; shift 2 ;;
    --output-dir) output_dir=$2; shift 2 ;;
    --work-dir) work_dir=$2; shift 2 ;;
    --view-pane) view_pane=$2; shift 2 ;;
    *) echo "unknown driver argument: $1" >&2; usage; exit 2 ;;
  esac
done

for value_name in winlane_bin demo_dir control_socket machine_name golden_image evidence_dir output_dir work_dir view_pane; do
  if [[ -z ${!value_name} ]]; then
    echo "drive-demo error: missing --${value_name//_/-}" >&2
    usage
    exit 2
  fi
done
if [[ ! -x $winlane_bin ]]; then
  echo "drive-demo error: winlane binary is not executable: $winlane_bin" >&2
  exit 2
fi
if [[ ! -f $golden_image ]]; then
  echo "drive-demo error: golden image does not exist: $golden_image" >&2
  exit 2
fi
if [[ ! $machine_name =~ ^[A-Za-z0-9_-]{1,48}$ ]]; then
  echo "drive-demo error: machine name must be 1-48 characters of letters, digits, underscore, or dash" >&2
  exit 2
fi

finish() {
  status=$?
  printf '%s\n' "$status" >"$work_dir/driver-status" 2>/dev/null || true
  printf '\n[demo] driver exiting with status %s; detaching recording client\n' "$status"
  sleep 0.2
  tmux detach-client >/dev/null 2>&1 || true
}
trap finish EXIT
trap 'printf "\ndrive-demo error: command failed at line %s\n" "$LINENO" >&2' ERR

while [[ $(tmux display-message -p '#{session_attached}' 2>/dev/null || echo 0) == "0" ]]; do
  sleep 0.1
done
sleep 0.3

run() {
  printf '\n$' >&2
  printf ' %q' "$@" >&2
  printf '\n' >&2
  "$@"
}

run_capture() {
  printf '\n$' >&2
  printf ' %q' "$@" >&2
  printf '\n' >&2
  "$@"
}

start_view() {
  local view_command
  printf -v view_command '%q ' "$winlane_bin" view \
    --control-socket "$control_socket" \
    --machine "$machine_name"
  run tmux respawn-pane -k -t "$view_pane" "$view_command"
}

mcp_call() {
  run python3 "$demo_dir/mcp_call.py" \
    --winlane-bin "$winlane_bin" \
    --control-socket "$control_socket" \
    --evidence-dir "$evidence_dir" \
    "$@"
}

mcp_call_capture() {
  run_capture python3 "$demo_dir/mcp_call.py" \
    --winlane-bin "$winlane_bin" \
    --control-socket "$control_socket" \
    --evidence-dir "$evidence_dir" \
    "$@"
}

valid_node_ref() {
  [[ $1 =~ ^0(/[0-9]+)*$ ]]
}

valid_runtime_id() {
  [[ -z $1 || $1 =~ ^-?[0-9]+(\.-?[0-9]+)*$ ]]
}

write_notepad_text() {
  local text=$1
  local tree_output=$2
  local tree_args node_line node runtime_id set_args
  tree_args=$(printf '{"machine":"%s","max_depth":8,"max_nodes":2000}' "$machine_name")
  mcp_call --timeout-seconds 60 --arguments "$tree_args" ui_tree >"$tree_output"
  printf '\n$ python3 %q %q\n' "$demo_dir/find_notepad_node.py" "$tree_output" >&2
  node_line=$(python3 "$demo_dir/find_notepad_node.py" "$tree_output")
  IFS=$'\t' read -r node runtime_id <<<"$node_line"
  if ! valid_node_ref "$node"; then
    echo "drive-demo error: invalid Notepad node path returned: $node" >&2
    return 1
  fi
  if ! valid_runtime_id "$runtime_id"; then
    echo "drive-demo error: invalid Notepad runtime id returned: $runtime_id" >&2
    return 1
  fi
  if [[ -n $runtime_id ]]; then
    set_args=$(printf '{"machine":"%s","node":"%s","text":"%s","runtime_id":"%s"}' \
      "$machine_name" "$node" "$text" "$runtime_id")
  else
    set_args=$(printf '{"machine":"%s","node":"%s","text":"%s"}' \
      "$machine_name" "$node" "$text")
  fi
  mcp_call --timeout-seconds 60 --arguments "$set_args" ui_set_text
}

printf 'winlane real-machine demo\n'
printf 'machine: %s\n' "$machine_name"
printf 'control socket: %s\n' "$control_socket"

run "$winlane_bin" machine create \
  --control-socket "$control_socket" \
  --name "$machine_name" \
  --base-image "$golden_image"
run "$winlane_bin" machine start \
  --control-socket "$control_socket" \
  --machine "$machine_name"
start_view
run "$demo_dir/wait-for-guest.sh" "$winlane_bin" "$control_socket" "$machine_name" 120

run python3 "$demo_dir/write_notepad_task_xml.py" "$work_dir/notepad-task.xml"
run "$winlane_bin" guest import \
  --machine "$machine_name" \
  --control-socket "$control_socket" \
  --host-path "$work_dir/notepad-task.xml" \
  --guest-path 'C:\Windows\Temp\winlane-demo-notepad.xml'
run "$winlane_bin" guest run \
  --machine "$machine_name" \
  --control-socket "$control_socket" \
  --command 'C:\Windows\System32\schtasks.exe' \
  --arg /Create \
  --arg /TN --arg WinlaneDemoNotepad \
  --arg /XML --arg 'C:\Windows\Temp\winlane-demo-notepad.xml' \
  --arg /F \
  --timeout-ms 10000
run "$winlane_bin" guest run \
  --machine "$machine_name" \
  --control-socket "$control_socket" \
  --command 'C:\Windows\System32\schtasks.exe' \
  --arg /Run \
  --arg /TN --arg WinlaneDemoNotepad \
  --timeout-ms 10000
run sleep 5

write_notepad_text 'before checkpoint' "$work_dir/ui-tree-before.json"
run sleep 2

checkpoint_args=$(printf '{"machine":"%s","label":"before-notepad-checkpoint"}' "$machine_name")
checkpoint_json=$(mcp_call_capture --timeout-seconds 300 --arguments "$checkpoint_args" checkpoint_create)
printf 'checkpoint result: %s\n' "$checkpoint_json"
printf '%s\n' "$checkpoint_json" >"$work_dir/checkpoint.json"

write_notepad_text 'this never happened' "$work_dir/ui-tree-after-checkpoint.json"
run sleep 3

printf '\nrestoring the checkpoint requires the current backend stop/restore/start sequence\n'
run "$winlane_bin" machine stop \
  --control-socket "$control_socket" \
  --machine "$machine_name"
run sleep 2
mcp_call --timeout-seconds 300 --checkpoint-file "$work_dir/checkpoint.json" checkpoint_restore
run "$winlane_bin" machine start \
  --control-socket "$control_socket" \
  --machine "$machine_name"
start_view
run "$demo_dir/wait-for-guest.sh" "$winlane_bin" "$control_socket" "$machine_name" 90

tree_args=$(printf '{"machine":"%s","max_depth":8,"max_nodes":2000}' "$machine_name")
mcp_call --timeout-seconds 60 --arguments "$tree_args" ui_tree >"$work_dir/ui-tree-restored.json"
run python3 "$demo_dir/find_notepad_node.py" "$work_dir/ui-tree-restored.json" >/dev/null
printf 'restored desktop is responsive and Notepad is present; leaving the view visible\n'
run sleep 7

run "$winlane_bin" machine stop \
  --control-socket "$control_socket" \
  --machine "$machine_name"
run "$winlane_bin" machine destroy \
  --control-socket "$control_socket" \
  --machine "$machine_name"
run "$winlane_bin" evidence export \
  --evidence-dir "$evidence_dir" \
  --output "$output_dir/evidence-export"

printf '\n[demo] sequence complete\n'
run sleep 2
