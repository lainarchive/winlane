#!/usr/bin/env bash
set -Eeuo pipefail

usage() {
  cat >&2 <<'USAGE'
usage: view-supervisor.sh --winlane-bin PATH --control-socket PATH --machine NAME --state-file PATH
USAGE
}

winlane_bin=""
control_socket=""
machine_name=""
state_file=""

while (($#)); do
  if [[ $# -lt 2 ]]; then
    echo "view-supervisor error: $1 requires a value" >&2
    usage
    exit 2
  fi
  case "$1" in
    --winlane-bin) winlane_bin=$2; shift 2 ;;
    --control-socket) control_socket=$2; shift 2 ;;
    --machine) machine_name=$2; shift 2 ;;
    --state-file) state_file=$2; shift 2 ;;
    *) echo "unknown view-supervisor argument: $1" >&2; usage; exit 2 ;;
  esac
done

for value_name in winlane_bin control_socket machine_name state_file; do
  if [[ -z ${!value_name} ]]; then
    echo "view-supervisor error: missing --${value_name//_/-}" >&2
    usage
    exit 2
  fi
done

stop_file="${state_file}.stop"
child_pid=""
current_request=""

child_running() {
  [[ -n $child_pid ]] || return 1
  kill -0 "$child_pid" >/dev/null 2>&1 || return 1
  local state
  state=$(ps -o stat= -p "$child_pid" 2>/dev/null || true)
  [[ ${state:0:1} != Z ]]
}

stop_child() {
  if child_running; then
    kill "$child_pid" >/dev/null 2>&1 || true
    for _ in 1 2 3 4 5 6 7 8 9 10; do
      child_running || break
      sleep 0.1
    done
    if child_running; then
      kill -KILL "$child_pid" >/dev/null 2>&1 || true
    fi
  fi
  if [[ -n $child_pid ]]; then
    wait "$child_pid" >/dev/null 2>&1 || true
  fi
  child_pid=""
}

cleanup() {
  trap - EXIT
  stop_child
}
trap cleanup EXIT

printf 'winlane view supervisor waiting for the demo driver\n'

while [[ ! -e $stop_file ]]; do
  desired_request=""
  if [[ -f $state_file ]]; then
    desired_request=$(cat "$state_file" 2>/dev/null || true)
  fi
  if [[ ! $desired_request =~ ^[0-9]+$ ]]; then
    desired_request=""
  fi

  if [[ $desired_request != "$current_request" ]]; then
    if [[ -z $desired_request || $desired_request == 0 ]]; then
      if child_running; then
        printf 'stop requested; waiting for winlane view to exit cleanly\n'
      fi
      current_request=0
    else
      stop_child
      current_request=$desired_request
      printf 'starting winlane view (request %s)\n' "$current_request"
      "$winlane_bin" view \
        --control-socket "$control_socket" \
        --machine "$machine_name" &
      child_pid=$!
    fi
  elif [[ -n $current_request && $current_request != 0 ]] && ! child_running; then
    printf 'winlane view exited; waiting to restart request %s\n' "$current_request"
    if [[ -n $child_pid ]]; then
      wait "$child_pid" >/dev/null 2>&1 || true
      child_pid=""
    fi
    sleep 1
    printf 'restarting winlane view (request %s)\n' "$current_request"
    "$winlane_bin" view \
      --control-socket "$control_socket" \
      --machine "$machine_name" &
    child_pid=$!
  elif [[ $current_request == 0 && -n $child_pid ]] && ! child_running; then
    wait "$child_pid" >/dev/null 2>&1 || true
    child_pid=""
  fi
  sleep 0.2
done

printf 'winlane view supervisor stopping\n'
