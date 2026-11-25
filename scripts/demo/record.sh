#!/usr/bin/env bash
set -Eeuo pipefail

usage() {
  cat >&2 <<'USAGE'
usage: record.sh --golden-image PATH --runtime-root PATH --control-socket PATH --evidence-dir PATH --output-dir PATH [--machine-name NAME]

Requires tmux, asciinema, python3, and an already built target/debug/winlane.
Writes winlane-demo.cast in the output directory; writes gif/mp4 only when agg/ffmpeg are available.
USAGE
}

fail() {
  echo "record.sh error: $*" >&2
  exit 2
}

abs_path() {
  python3 -c 'import os, sys; print(os.path.abspath(os.path.expanduser(sys.argv[1])))' "$1"
}

require_empty_or_absent_dir() {
  local path=$1
  local label=$2
  if [[ -e $path && ! -d $path ]]; then
    fail "$label exists and is not a directory: $path"
  fi
  if [[ -d $path ]] && find "$path" -mindepth 1 -print -quit | grep -q .; then
    fail "$label must be absent or empty for a reproducible run: $path"
  fi
  mkdir -p -- "$path"
}

golden_image=""
runtime_root=""
control_socket=""
evidence_dir=""
output_dir=""
machine_name=""

while (($#)); do
  if [[ $1 != --help && $1 != -h && $# -lt 2 ]]; then
    fail "$1 requires a value"
  fi
  case "$1" in
    --golden-image) golden_image=$2; shift 2 ;;
    --runtime-root) runtime_root=$2; shift 2 ;;
    --control-socket) control_socket=$2; shift 2 ;;
    --evidence-dir) evidence_dir=$2; shift 2 ;;
    --output-dir) output_dir=$2; shift 2 ;;
    --machine-name) machine_name=$2; shift 2 ;;
    --help|-h) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage; exit 2 ;;
  esac
done

[[ -n $golden_image ]] || fail "missing --golden-image"
[[ -n $runtime_root ]] || fail "missing --runtime-root"
[[ -n $control_socket ]] || fail "missing --control-socket"
[[ -n $evidence_dir ]] || fail "missing --evidence-dir"
[[ -n $output_dir ]] || fail "missing --output-dir"

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/../.." && pwd)
winlane_bin="$repo_root/target/debug/winlane"

command -v python3 >/dev/null 2>&1 || fail "python3 is required"
command -v tmux >/dev/null 2>&1 || fail "tmux is required"
command -v asciinema >/dev/null 2>&1 || fail "asciinema is required"
[[ -x $winlane_bin ]] || fail "winlane binary missing; run: cargo build -p winlane-cli"

golden_image=$(abs_path "$golden_image")
runtime_root=$(abs_path "$runtime_root")
control_socket=$(abs_path "$control_socket")
evidence_dir=$(abs_path "$evidence_dir")
output_dir=$(abs_path "$output_dir")

[[ -f $golden_image && -r $golden_image ]] || fail "golden image is not a readable file: $golden_image"
if [[ -z $machine_name ]]; then
  machine_name="winlane-demo-$$"
fi
[[ $machine_name =~ ^[A-Za-z0-9_-]{1,48}$ ]] || fail "machine name must be 1-48 characters of letters, digits, underscore, or dash"

if [[ -e $runtime_root && ! -d $runtime_root ]]; then
  fail "runtime root exists and is not a directory: $runtime_root"
fi
mkdir -p -- "$runtime_root"
[[ ! -e $runtime_root/$machine_name ]] || fail "machine name already exists under runtime root: $machine_name"

control_parent=$(dirname -- "$control_socket")
mkdir -p -- "$control_parent"
[[ ! -e $control_socket ]] || fail "control socket path already exists: $control_socket"
require_empty_or_absent_dir "$evidence_dir" "evidence directory"
require_empty_or_absent_dir "$output_dir" "output directory"

work_dir=$(mktemp -d "${TMPDIR:-/tmp}/winlane-demo.XXXXXX")
session_name="winlane-demo-tmux-$$"
daemon_pid=""
view_pane=""
control_pane=""

cleanup() {
  trap - EXIT
  if [[ -n $session_name ]]; then
    tmux kill-session -t "$session_name" >/dev/null 2>&1 || true
  fi
  if [[ -S $control_socket && -x $winlane_bin ]]; then
    timeout 10 "$winlane_bin" machine stop \
      --control-socket "$control_socket" \
      --machine "$machine_name" >/dev/null 2>&1 || true
    timeout 10 "$winlane_bin" machine destroy \
      --control-socket "$control_socket" \
      --machine "$machine_name" >/dev/null 2>&1 || true
  fi
  if [[ -n $daemon_pid ]] && kill -0 "$daemon_pid" >/dev/null 2>&1; then
    kill "$daemon_pid" >/dev/null 2>&1 || true
    for _ in 1 2 3 4 5 6 7 8 9 10; do
      kill -0 "$daemon_pid" >/dev/null 2>&1 || break
      sleep 0.2
    done
    kill -KILL "$daemon_pid" >/dev/null 2>&1 || true
    wait "$daemon_pid" >/dev/null 2>&1 || true
  fi
  rm -f -- "$control_socket"
  rm -rf -- "$work_dir"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

"$winlane_bin" daemon \
  --root "$runtime_root" \
  --control-socket "$control_socket" >"$work_dir/daemon.log" 2>&1 &
daemon_pid=$!

daemon_ready=0
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
  if [[ -S $control_socket ]]; then
    daemon_ready=1
    break
  fi
  if ! kill -0 "$daemon_pid" >/dev/null 2>&1; then
    break
  fi
  sleep 0.25
done
if [[ $daemon_ready -ne 1 ]]; then
  echo "record.sh error: control daemon did not create socket: $control_socket" >&2
  sed -n '1,120p' "$work_dir/daemon.log" >&2 || true
  exit 1
fi

view_pane=$(tmux new-session -d -s "$session_name" -x 132 -y 40 -P -F '#{pane_id}' \
  "printf 'winlane view will start here after the machine starts\\n'; exec bash")

printf -v driver_command '%q ' bash "$script_dir/drive-demo.sh" \
  --winlane-bin "$winlane_bin" \
  --demo-dir "$script_dir" \
  --control-socket "$control_socket" \
  --machine-name "$machine_name" \
  --golden-image "$golden_image" \
  --evidence-dir "$evidence_dir" \
  --output-dir "$output_dir" \
  --work-dir "$work_dir" \
  --view-pane "$view_pane"

control_pane=$(tmux split-window -h -t "$session_name" -P -F '#{pane_id}' "$driver_command")
tmux set-option -t "$session_name" remain-on-exit on
tmux select-pane -t "$control_pane"

cast_file="$output_dir/winlane-demo.cast"
record_command='if [ -z "${TERM:-}" ] || [ "${TERM:-}" = dumb ]; then export TERM=xterm; fi; stty cols 132 rows 40 2>/dev/null || true; exec tmux attach-session -t '
record_command+="$session_name"
if [[ -z ${TERM:-} || ${TERM:-} == dumb ]]; then
  export TERM=xterm
fi
export COLUMNS=132
export LINES=40
if ! asciinema rec --overwrite --idle-time-limit 2 --command "$record_command" "$cast_file"; then
  fail "asciinema recording failed"
fi

if [[ ! -s $cast_file ]]; then
  fail "asciinema did not write a nonempty cast: $cast_file"
fi
if [[ -f $work_dir/driver-status ]]; then
  driver_status=$(cat "$work_dir/driver-status")
  if [[ $driver_status != 0 ]]; then
    fail "demo driver failed with status $driver_status; inspect the cast and rerun with a clean runtime root"
  fi
else
  fail "demo driver status is missing; the session may have been interrupted"
fi

if command -v agg >/dev/null 2>&1; then
  gif_file="$output_dir/winlane-demo.gif"
  if ! agg "$cast_file" "$gif_file"; then
    echo "warning: agg failed; keeping cast only" >&2
    rm -f -- "$gif_file"
  elif command -v ffmpeg >/dev/null 2>&1; then
    mp4_file="$output_dir/winlane-demo.mp4"
    if ! ffmpeg -y -loglevel error -i "$gif_file" -vf 'pad=ceil(iw/2)*2:ceil(ih/2)*2' -movflags faststart -pix_fmt yuv420p "$mp4_file"; then
      echo "warning: ffmpeg mp4 conversion failed; keeping gif and cast" >&2
      rm -f -- "$mp4_file"
    fi
  fi
fi

printf 'recording: %s\n' "$cast_file"
[[ ! -f $output_dir/winlane-demo.gif ]] || printf 'gif: %s\n' "$output_dir/winlane-demo.gif"
[[ ! -f $output_dir/winlane-demo.mp4 ]] || printf 'mp4: %s\n' "$output_dir/winlane-demo.mp4"
printf 'evidence export: %s\n' "$output_dir/evidence-export"
