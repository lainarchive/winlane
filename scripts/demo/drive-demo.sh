#!/usr/bin/env bash
set -Eeuo pipefail

usage() {
  cat >&2 <<'USAGE'
usage: drive-demo.sh --winlane-bin PATH --demo-dir PATH --control-socket PATH --machine-name NAME --golden-image PATH --evidence-dir PATH --output-dir PATH --work-dir PATH [--view-pane PANE | --view-state PATH] [--ready-file PATH]
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
view_state=""
ready_file=""

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
    --view-state) view_state=$2; shift 2 ;;
    --ready-file) ready_file=$2; shift 2 ;;
    *) echo "unknown driver argument: $1" >&2; usage; exit 2 ;;
  esac
done

for value_name in winlane_bin demo_dir control_socket machine_name golden_image evidence_dir output_dir work_dir; do
  if [[ -z ${!value_name} ]]; then
    echo "drive-demo error: missing --${value_name//_/-}" >&2
    usage
    exit 2
  fi
done
if [[ -z $view_pane && -z $view_state ]]; then
  echo "drive-demo error: one of --view-pane or --view-state is required" >&2
  usage
  exit 2
fi
if [[ -n $view_pane && -n $view_state ]]; then
  echo "drive-demo error: --view-pane and --view-state are mutually exclusive" >&2
  usage
  exit 2
fi
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
  stop_view >/dev/null 2>&1 || true
  printf '%s\n' "$status" >"$work_dir/driver-status" 2>/dev/null || true
  if [[ -n $view_pane ]]; then
    printf '\n[demo] driver exiting with status %s; detaching recording client\n' "$status"
    sleep 0.2
    tmux detach-client >/dev/null 2>&1 || true
  else
    printf '\n[demo] driver exiting with status %s\n' "$status"
    sleep 0.2
  fi
}
trap finish EXIT
trap 'printf "\ndrive-demo error: command failed at line %s\n" "$LINENO" >&2' ERR

if [[ -n $ready_file ]]; then
  while [[ ! -e $ready_file ]]; do
    sleep 0.1
  done
elif [[ -n $view_pane ]]; then
  while [[ $(tmux display-message -p '#{session_attached}' 2>/dev/null || echo 0) == "0" ]]; do
    sleep 0.1
  done
fi
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

atomic_write() {
  local path=$1 value=$2
  printf '%s
' "$value" >"${path}.tmp"
  mv -f -- "${path}.tmp" "$path"
}

start_view() {
  if [[ -n $view_state ]]; then
    local next_request=1
    local counter_file="${view_state}.counter"
    if [[ -f $counter_file ]]; then
      local current_request
      current_request=$(cat "$counter_file" 2>/dev/null || echo 0)
      if [[ $current_request =~ ^[0-9]+$ ]]; then
        next_request=$((current_request + 1))
      fi
    fi
    atomic_write "$counter_file" "$next_request"
    atomic_write "$view_state" "$next_request"
    printf 'requested external winlane view restart (request %s)\n' "$next_request"
    return 0
  fi
  local view_command
  printf -v view_command '%q ' "$winlane_bin" view \
    --control-socket "$control_socket" \
    --machine "$machine_name"
  run tmux respawn-pane -k -t "$view_pane" "$view_command"
}

stop_view() {
  if [[ -n $view_state ]]; then
    rm -f -- "${view_state}.stopped"
    atomic_write "$view_state" 0
    for _ in {1..100}; do
      [[ -f ${view_state}.stopped ]] && return 0
      sleep 0.1
    done
    echo "drive-demo error: timed out waiting for winlane view to stop" >&2
    return 1
  fi
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
  for attempt in 1 2; do
    if mcp_call --timeout-seconds 60 --arguments "$tree_args" ui_tree >"$tree_output"; then
      break
    fi
    if [[ $attempt -eq 2 ]]; then
      return 1
    fi
    sleep 3
  done
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
    set_args=$(printf '{"machine":"%s","node":"%s","text":"%s","runtime_id":"%s","timeout_ms":60000}' \
      "$machine_name" "$node" "$text" "$runtime_id")
  else
    set_args=$(printf '{"machine":"%s","node":"%s","text":"%s","timeout_ms":60000}' \
      "$machine_name" "$node" "$text")
  fi
  for attempt in 1 2; do
    if mcp_call --timeout-seconds 60 --arguments "$set_args" ui_set_text; then
      return 0
    fi
    if [[ $attempt -eq 2 ]]; then
      return 1
    fi
    sleep 3
  done
}


dismiss_notepad_first_run_popup() {
  local tree_file="$work_dir/ui-tree-popup.json"
  local tree_args node_line node runtime_id invoke_args
  tree_args=$(printf '{"machine":"%s","max_depth":8,"max_nodes":2000}' "$machine_name")
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    if ! mcp_call --timeout-seconds 60 --arguments "$tree_args" ui_tree >"$tree_file"; then
      sleep 2
      continue
    fi
    printf '\n$ python3 %q %q\n' "$demo_dir/find_notepad_popup_button.py" "$tree_file" >&2
    node_line=$(python3 "$demo_dir/find_notepad_popup_button.py" "$tree_file")
    if [[ -z $node_line ]]; then
      printf 'Notepad first-run popup absent or already dismissed\n'
      return 0
    fi
    IFS=$'\t' read -r node runtime_id <<<"$node_line"
    if ! valid_node_ref "$node" || ! valid_runtime_id "$runtime_id"; then
      echo "drive-demo error: invalid popup node reference" >&2
      return 1
    fi
    if [[ -n $runtime_id ]]; then
      invoke_args=$(printf '{"machine":"%s","node":"%s","runtime_id":"%s","timeout_ms":30000}' "$machine_name" "$node" "$runtime_id")
    else
      invoke_args=$(printf '{"machine":"%s","node":"%s","timeout_ms":30000}' "$machine_name" "$node")
    fi
    if ! mcp_call --timeout-seconds 45 --arguments "$invoke_args" ui_invoke; then
      echo 'drive-demo error: failed to invoke Notepad first-run dismissal button' >&2
      return 1
    fi
    sleep 1
    if mcp_call --timeout-seconds 60 --arguments "$tree_args" ui_tree >"$tree_file" \
      && [[ -z $(python3 "$demo_dir/find_notepad_popup_button.py" "$tree_file") ]]; then
      printf 'Notepad first-run popup dismissed and verified absent\n'
      return 0
    fi
  done
  echo 'drive-demo error: Notepad first-run popup remained present after dismissal attempts' >&2
  return 1
}

printf 'winlane real-machine demo\n'
printf 'machine: %s\n' "$machine_name"
printf 'control socket: %s\n' "$control_socket"

disk_gb=$(python3 - "$golden_image" <<'PYEOF'
import json
import subprocess
import sys

info = json.loads(subprocess.check_output(
    ["qemu-img", "info", "--output=json", sys.argv[1]], text=True
))
size = int(info["virtual-size"])
gib = 1024**3
print((size + gib - 1) // gib)
PYEOF
)
if [[ ! $disk_gb =~ ^[1-9][0-9]*$ ]]; then
  echo "drive-demo error: unable to derive disk size from $golden_image" >&2
  exit 1
fi

run "$winlane_bin" machine create \
  --control-socket "$control_socket" \
  --name "$machine_name" \
  --base-image "$golden_image" \
  --disk-gb "$disk_gb"
run "$winlane_bin" machine start \
  --control-socket "$control_socket" \
  --machine "$machine_name"
run "$demo_dir/wait-for-guest.sh" "$winlane_bin" "$control_socket" "$machine_name" 120
run "$demo_dir/wait-for-interactive-desktop.sh" "$winlane_bin" "$control_socket" "$machine_name" 120 "$demo_dir" "$work_dir/interactive-desktop-initial.json"
printf 'interactive desktop ready; enabling recording for demo actions\n'
touch "$work_dir/ui-ready"
while [[ ! -e $work_dir/recording-ready ]]; do
  sleep 0.1
done
start_view

run python3 "$demo_dir/write_task_xml.py" "$work_dir/notepad-task.xml" \
  --command 'C:\Windows\System32\notepad.exe'
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
dismiss_notepad_first_run_popup

write_notepad_text 'before checkpoint' "$work_dir/ui-tree-before.json"
run sleep 2

checkpoint_args=$(printf '{"machine":"%s","label":"before-notepad-checkpoint"}' "$machine_name")
checkpoint_json=$(mcp_call_capture --timeout-seconds 300 --arguments "$checkpoint_args" checkpoint_create)
printf 'checkpoint result: %s\n' "$checkpoint_json"
printf '%s\n' "$checkpoint_json" >"$work_dir/checkpoint.json"

write_notepad_text 'this never happened' "$work_dir/ui-tree-after-checkpoint.json"
run sleep 3

printf '\nrestoring the checkpoint requires the current backend stop/restore/start sequence\n'
stop_view
run "$winlane_bin" machine stop \
  --control-socket "$control_socket" \
  --machine "$machine_name"
run sleep 2
mcp_call --timeout-seconds 300 --checkpoint-file "$work_dir/checkpoint.json" checkpoint_restore
run "$winlane_bin" machine start \
  --control-socket "$control_socket" \
  --machine "$machine_name"
run "$demo_dir/wait-for-guest.sh" "$winlane_bin" "$control_socket" "$machine_name" 90
run "$demo_dir/wait-for-interactive-desktop.sh" "$winlane_bin" "$control_socket" "$machine_name" 90 "$demo_dir" "$work_dir/interactive-desktop-restored.json"
start_view

tree_args=$(printf '{"machine":"%s","max_depth":8,"max_nodes":2000}' "$machine_name")
mcp_call --timeout-seconds 60 --arguments "$tree_args" ui_tree >"$work_dir/ui-tree-restored.json"
printf '\n$ python3 %q %q\n' "$demo_dir/find_notepad_node.py" "$work_dir/ui-tree-restored.json" >&2
restored_node_line=$(python3 "$demo_dir/find_notepad_node.py" "$work_dir/ui-tree-restored.json")
IFS=$'\t' read -r restored_node restored_runtime_id <<<"$restored_node_line"
if ! valid_node_ref "$restored_node"; then
  echo "drive-demo error: invalid restored Notepad node path returned: $restored_node" >&2
  exit 1
fi
if ! valid_runtime_id "$restored_runtime_id"; then
  echo "drive-demo error: invalid restored Notepad runtime id returned: $restored_runtime_id" >&2
  exit 1
fi

read_powershell_args="-NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -File C:\\Windows\\Temp\\winlane-demo-read-uia.ps1 -Path $restored_node -OutputPath C:\\Windows\\Temp\\winlane-demo-read.json"
run python3 "$demo_dir/write_task_xml.py" "$work_dir/read-task.xml" \
  --command 'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe' \
  --arguments "$read_powershell_args"
run "$winlane_bin" guest import \
  --machine "$machine_name" \
  --control-socket "$control_socket" \
  --host-path "$demo_dir/read-uia-text.ps1" \
  --guest-path 'C:\Windows\Temp\winlane-demo-read-uia.ps1'
run "$winlane_bin" guest import \
  --machine "$machine_name" \
  --control-socket "$control_socket" \
  --host-path "$work_dir/read-task.xml" \
  --guest-path 'C:\Windows\Temp\winlane-demo-read-task.xml'
run "$winlane_bin" guest run \
  --machine "$machine_name" \
  --control-socket "$control_socket" \
  --command 'C:\Windows\System32\schtasks.exe' \
  --arg /Create \
  --arg /TN --arg WinlaneDemoReadText \
  --arg /XML --arg 'C:\Windows\Temp\winlane-demo-read-task.xml' \
  --arg /F \
  --timeout-ms 10000
run "$winlane_bin" guest run \
  --machine "$machine_name" \
  --control-socket "$control_socket" \
  --command 'C:\Windows\System32\schtasks.exe' \
  --arg /Run \
  --arg /TN --arg WinlaneDemoReadText \
  --timeout-ms 10000

printf 'waiting for guest-side UIA text read result\n'
read_result_ready=0
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
  if "$winlane_bin" guest export \
    --machine "$machine_name" \
    --control-socket "$control_socket" \
    --guest-path 'C:\Windows\Temp\winlane-demo-read.json' \
    --host-path "$work_dir/uia-read.json" >/dev/null 2>&1; then
    read_result_ready=1
    break
  fi
  sleep 1
done
if [[ $read_result_ready -ne 1 ]]; then
  echo 'drive-demo error: guest-side UIA text read did not produce output' >&2
  exit 1
fi
run "$winlane_bin" guest export \
  --machine "$machine_name" \
  --control-socket "$control_socket" \
  --guest-path 'C:\Windows\Temp\winlane-demo-read.json' \
  --host-path "$work_dir/uia-read.json"
run python3 "$demo_dir/verify_uia_text.py" "$work_dir/uia-read.json" 'before checkpoint'
printf 'restored Notepad text is independently verified; leaving the view visible\n'
run sleep 7

stop_view
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
