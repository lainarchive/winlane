#!/usr/bin/env bash
set -Eeuo pipefail

usage() {
  cat >&2 <<'USAGE'
usage: record-x11.sh --golden-image PATH --runtime-root PATH --control-socket PATH --evidence-dir PATH --output-dir PATH [--machine-name NAME] [--kitty-bin PATH] [--winlane-bin PATH] [--driver-script PATH]

Requires Xvfb, ffmpeg, ffprobe, python3, and a modern kitty binary. When --kitty-bin
and WINLANE_KITTY_BIN are absent, the official portable Linux kitty 0.51.1 release
is downloaded into the user cache outside this repository and verified by SHA-256.
Writes winlane-demo-x11.mp4, winlane-demo-x11.mp4.sha256, capture-info.json, driver.log,
driver-status, terminal logs, and evidence-export/ in the output directory.
USAGE
}

fail() {
  echo "record-x11.sh error: $*" >&2
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

ensure_portable_kitty() {
  local version=0.51.1
  local sha256=d309be978f017dbdcccae175d281092fefe7c8bc64478fa620581604ab0b53c0
  local url="https://github.com/kovidgoyal/kitty/releases/download/v${version}/kitty-${version}-x86_64.txz"
  local cache_parent="${XDG_CACHE_HOME:-$HOME/.cache}/winlane-demo"
  local target="$cache_parent/kitty-$version"
  if [[ -x $target/bin/kitty && -f $target/kitty.txz ]]; then
    if (cd "$target" && printf '%s  kitty.txz\n' "$sha256" | sha256sum -c - >/dev/null 2>&1); then
      printf '%s\n' "$target/bin/kitty"
      return 0
    fi
    rm -rf -- "$target"
  fi
  command -v curl >/dev/null 2>&1 || fail "curl is required to download kitty"
  command -v xz >/dev/null 2>&1 || fail "xz is required to unpack kitty"
  local tmp
  tmp=$(mktemp -d "${TMPDIR:-/tmp}/winlane-kitty-download.XXXXXX")
  local rc=0
  (
    cd "$tmp"
    curl -L --fail --silent --show-error -o kitty.txz "$url"
    printf '%s  kitty.txz\n' "$sha256" | sha256sum -c - >/dev/null
    mkdir extract
    tar -xJf kitty.txz -C extract
    mkdir -p -- "$cache_parent"
    rm -rf -- "$target.new"
    mv extract "$target.new"
    mv kitty.txz "$target.new/kitty.txz"
    rm -rf -- "$target"
    mv "$target.new" "$target"
  ) || rc=$?
  rm -rf -- "$tmp"
  if [[ $rc -ne 0 || ! -x $target/bin/kitty ]]; then
    fail "unable to download/verify portable kitty $version"
  fi
  printf '%s\n' "$target/bin/kitty"
}

quoted_command() {
  printf '%q ' "$@"
}

recorder_command=$(quoted_command "$0" "$@")

golden_image=""
runtime_root=""
control_socket=""
evidence_dir=""
output_dir=""
machine_name=""
kitty_bin="${WINLANE_KITTY_BIN:-}"
winlane_bin=""
driver_script=""

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
    --kitty-bin) kitty_bin=$2; shift 2 ;;
    --winlane-bin) winlane_bin=$2; shift 2 ;;
    --driver-script) driver_script=$2; shift 2 ;;
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
if [[ -z $winlane_bin ]]; then
  winlane_bin="$repo_root/target/release/winlane"
fi
if [[ -z $driver_script ]]; then
  driver_script="$script_dir/drive-demo.sh"
fi

for dependency in python3 Xvfb ffmpeg ffprobe sha256sum xwininfo xdpyinfo; do
  command -v "$dependency" >/dev/null 2>&1 || fail "$dependency is required"
done
if [[ -z $kitty_bin ]]; then
  kitty_bin=$(ensure_portable_kitty)
fi

golden_image=$(abs_path "$golden_image")
runtime_root=$(abs_path "$runtime_root")
control_socket=$(abs_path "$control_socket")
evidence_dir=$(abs_path "$evidence_dir")
output_dir=$(abs_path "$output_dir")
winlane_bin=$(abs_path "$winlane_bin")
driver_script=$(abs_path "$driver_script")
kitty_bin=$(abs_path "$kitty_bin")

[[ -f $golden_image && -r $golden_image ]] || fail "golden image is not a readable file: $golden_image"
[[ -x $winlane_bin ]] || fail "winlane release binary missing; run: cargo build --release -p winlane-cli"
[[ -x $driver_script ]] || fail "driver script is not executable: $driver_script"
[[ -x $kitty_bin ]] || fail "kitty binary is not executable: $kitty_bin"
kitty_version_output=$("$kitty_bin" --version 2>/dev/null || true)
[[ $kitty_version_output =~ ^kitty\ ([0-9]+\.[0-9]+\.[0-9]+) ]] || fail "unable to identify kitty version from: $kitty_version_output"
kitty_version=${BASH_REMATCH[1]}

sha256_of() {
  sha256sum "$1" | awk '{print $1}'
}
winlane_version_output=$("$winlane_bin" --version 2>/dev/null | sed -n '1p')
ffmpeg_version_output=$(ffmpeg -version 2>/dev/null | sed -n '1p')
capture_os=$(uname -srmo 2>/dev/null || uname -a)
export WINLANE_CAPTURE_RECORDER_COMMAND="$recorder_command"
export WINLANE_CAPTURE_OS="$capture_os"
export WINLANE_CAPTURE_FFMPEG_VERSION="$ffmpeg_version_output"
export WINLANE_CAPTURE_WINLANE_BIN="$winlane_bin"
export WINLANE_CAPTURE_WINLANE_SHA256="$(sha256_of "$winlane_bin")"
export WINLANE_CAPTURE_WINLANE_VERSION="$winlane_version_output"
export WINLANE_CAPTURE_RECORD_SCRIPT="$script_dir/record-x11.sh"
export WINLANE_CAPTURE_RECORD_SCRIPT_SHA256="$(sha256_of "$script_dir/record-x11.sh")"
export WINLANE_CAPTURE_DRIVER_SCRIPT="$driver_script"
export WINLANE_CAPTURE_DRIVER_SCRIPT_SHA256="$(sha256_of "$driver_script")"

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

work_dir=$(mktemp -d "${TMPDIR:-/tmp}/winlane-demo-x11.XXXXXX")
display_num=""
xvfb_pid=""
kitty_pid=""
ffmpeg_pid=""
daemon_pid=""
display=""
view_state="$work_dir/view-request"
mp4_file="$output_dir/winlane-demo-x11.mp4"
capture_info="$output_dir/capture-info.json"

process_running() {
  local pid=$1
  [[ -n $pid ]] || return 1
  kill -0 "$pid" >/dev/null 2>&1 || return 1
  local state
  state=$(ps -o stat= -p "$pid" 2>/dev/null || true)
  [[ ${state:0:1} != Z ]]
}

cleanup() {
  trap - EXIT
  touch "${view_state}.stop" 2>/dev/null || true
  if [[ -n $ffmpeg_pid ]] && process_running "$ffmpeg_pid"; then
    kill -INT "$ffmpeg_pid" >/dev/null 2>&1 || true
    for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
      process_running "$ffmpeg_pid" || break
      sleep 0.2
    done
    kill -KILL "$ffmpeg_pid" >/dev/null 2>&1 || true
    wait "$ffmpeg_pid" >/dev/null 2>&1 || true
  fi
  if [[ -n $kitty_pid ]] && process_running "$kitty_pid"; then
    kill "$kitty_pid" >/dev/null 2>&1 || true
    wait "$kitty_pid" >/dev/null 2>&1 || true
  fi
  if [[ -S $control_socket && -x $winlane_bin ]]; then
    timeout 10 "$winlane_bin" machine stop \
      --control-socket "$control_socket" \
      --machine "$machine_name" >/dev/null 2>&1 || true
    timeout 10 "$winlane_bin" machine destroy \
      --control-socket "$control_socket" \
      --machine "$machine_name" >/dev/null 2>&1 || true
  fi
  if [[ -n $daemon_pid ]] && process_running "$daemon_pid"; then
    kill "$daemon_pid" >/dev/null 2>&1 || true
    for _ in 1 2 3 4 5 6 7 8 9 10; do
      process_running "$daemon_pid" || break
      sleep 0.2
    done
    kill -KILL "$daemon_pid" >/dev/null 2>&1 || true
    wait "$daemon_pid" >/dev/null 2>&1 || true
  fi
  if [[ -n $xvfb_pid ]] && process_running "$xvfb_pid"; then
    kill "$xvfb_pid" >/dev/null 2>&1 || true
    wait "$xvfb_pid" >/dev/null 2>&1 || true
  fi
  rm -f -- "$control_socket"
  rm -rf -- "$work_dir"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

if [[ -n ${WINLANE_DEMO_DISPLAY:-} ]]; then
  [[ ${WINLANE_DEMO_DISPLAY} =~ ^[0-9]+$ ]] || fail "WINLANE_DEMO_DISPLAY must be a display number"
  display_num=${WINLANE_DEMO_DISPLAY}
else
  for candidate in $(seq 170 199); do
    if ! DISPLAY=:$candidate xdpyinfo >/dev/null 2>&1; then
      display_num=$candidate
      break
    fi
  done
fi
[[ -n $display_num ]] || fail "no free X display number found in 170-199"
display=:$display_num

Xvfb "$display" -screen 0 1920x1080x24 >"$output_dir/xvfb.log" 2>&1 &
xvfb_pid=$!
x_ready=0
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
  if DISPLAY=$display xdpyinfo >/dev/null 2>&1; then
    x_ready=1
    break
  fi
  if ! process_running "$xvfb_pid"; then
    break
  fi
  sleep 0.2
done
if [[ $x_ready -ne 1 ]]; then
  echo "record-x11.sh error: Xvfb did not become ready on $display" >&2
  sed -n '1,120p' "$output_dir/xvfb.log" >&2 || true
  exit 1
fi

view_launcher="$work_dir/view-launcher.sh"
driver_launcher="$work_dir/driver-launcher.sh"
{
  echo '#!/usr/bin/env bash'
  printf 'exec '
  quoted_command bash "$script_dir/view-supervisor.sh" \
    --winlane-bin "$winlane_bin" \
    --control-socket "$control_socket" \
    --machine "$machine_name" \
    --state-file "$view_state"
  printf '\n'
} >"$view_launcher"
chmod +x "$view_launcher"

{
  echo '#!/usr/bin/env bash'
  echo 'set -o pipefail'
  quoted_command bash "$driver_script" \
    --winlane-bin "$winlane_bin" \
    --demo-dir "$script_dir" \
    --control-socket "$control_socket" \
    --machine-name "$machine_name" \
    --golden-image "$golden_image" \
    --evidence-dir "$evidence_dir" \
    --output-dir "$output_dir" \
    --work-dir "$work_dir" \
    --view-state "$view_state" \
    --ready-file "$work_dir/daemon-ready"
  printf ' 2>&1 | '
  quoted_command tee "$output_dir/driver.log"
  printf '\nexit "${PIPESTATUS[0]}"\n'
} >"$driver_launcher"
chmod +x "$driver_launcher"

session_file="$work_dir/kitty.session"
cat >"$session_file" <<EOF_SESSION
os_window_size 1920 1080
os_window_position 0x0
os_window_title winlane disposable Windows demo
layout splits
EOF_SESSION
printf 'launch --hold --title winlane-view %q %q\n' /bin/bash "$view_launcher" >>"$session_file"
printf 'launch --location=vsplit --bias=38 --hold --title demo-driver %q %q\n' /bin/bash "$driver_launcher" >>"$session_file"

DISPLAY=$display "$kitty_bin" \
  --config NONE \
  -o font_size=11 \
  -o confirm_os_window_close=0 \
  -o shell_integration=disabled \
  --session "$session_file" >"$output_dir/kitty.log" 2>&1 &
kitty_pid=$!

window_ready=0
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25; do
  if DISPLAY=$display xwininfo -root -children 2>/dev/null | grep -q '1920x1080+0+0'; then
    window_ready=1
    break
  fi
  if ! process_running "$kitty_pid"; then
    break
  fi
  sleep 0.2
done
if [[ $window_ready -ne 1 ]]; then
  echo "record-x11.sh error: kitty did not create the expected 1920x1080 window" >&2
  sed -n '1,160p' "$output_dir/kitty.log" >&2 || true
  exit 1
fi

"$winlane_bin" daemon \
  --root "$runtime_root" \
  --control-socket "$control_socket" >"$output_dir/daemon.log" 2>&1 &
daemon_pid=$!

daemon_ready=0
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
  if [[ -S $control_socket ]]; then
    daemon_ready=1
    break
  fi
  if ! process_running "$daemon_pid"; then
    break
  fi
  sleep 0.25
done
if [[ $daemon_ready -ne 1 ]]; then
  echo "record-x11.sh error: control daemon did not create socket: $control_socket" >&2
  sed -n '1,120p' "$output_dir/daemon.log" >&2 || true
  exit 1
fi

# Let the driver create/start the VM and prove interactive-desktop readiness first.
printf 'daemon-ready\n' >"$work_dir/daemon-ready"

ui_ready=0
for _ in $(seq 1 600); do
  if [[ -e $work_dir/ui-ready ]]; then
    ui_ready=1
    break
  fi
  if [[ -f $work_dir/driver-status ]]; then
    echo "record-x11.sh error: demo driver exited before interactive-desktop readiness" >&2
    exit 1
  fi
  sleep 0.5
done
if [[ $ui_ready -ne 1 ]]; then
  echo "record-x11.sh error: timed out waiting for interactive-desktop readiness" >&2
  exit 1
fi

ffmpeg_command=(
  ffmpeg -hide_banner -loglevel warning -y
  -f x11grab -draw_mouse 0 -framerate 30 -video_size 1920x1080 -i "$display"
  -c:v libx264 -preset veryfast -pix_fmt yuv420p -movflags +faststart
  "$mp4_file"
)
export WINLANE_CAPTURE_FFMPEG_COMMAND="$(quoted_command "${ffmpeg_command[@]}")"
export WINLANE_CAPTURE_STARTED_UNIX="$(python3 -c 'import time; print(time.time())')"
"${ffmpeg_command[@]}" >"$output_dir/ffmpeg.log" 2>&1 &
ffmpeg_pid=$!
sleep 0.7
if ! process_running "$ffmpeg_pid"; then
  echo "record-x11.sh error: ffmpeg x11grab failed to start" >&2
  sed -n '1,160p' "$output_dir/ffmpeg.log" >&2 || true
  exit 1
fi
printf 'recording-ready\n' >"$work_dir/recording-ready"

driver_status=""
while [[ ! -f $work_dir/driver-status ]]; do
  if ! process_running "$kitty_pid"; then
    echo "record-x11.sh error: kitty exited before the demo driver finished" >&2
    exit 1
  fi
  sleep 0.5
done
driver_status=$(cat "$work_dir/driver-status" 2>/dev/null || echo missing)
printf '%s\n' "$driver_status" >"$output_dir/driver-status"

touch "${view_state}.stop"
sleep 1

if process_running "$ffmpeg_pid"; then
  kill -INT "$ffmpeg_pid" >/dev/null 2>&1 || true
fi
ffmpeg_exited=0
for _ in $(seq 1 50); do
  if ! process_running "$ffmpeg_pid"; then
    ffmpeg_exited=1
    break
  fi
  sleep 0.2
done
if [[ $ffmpeg_exited -ne 1 ]]; then
  echo "record-x11.sh error: ffmpeg did not finalize the MP4" >&2
  exit 1
fi
wait "$ffmpeg_pid" >/dev/null 2>&1 || true
ffmpeg_pid=""
export WINLANE_CAPTURE_FINISHED_UNIX="$(python3 -c 'import time; print(time.time())')"

probe_ok=0
probe_stderr=$(mktemp "${TMPDIR:-/tmp}/winlane-ffprobe.XXXXXX")
if python3 "$script_dir/write_capture_info.py" "$mp4_file" "$capture_info" "$kitty_bin" "$kitty_version" "$display" "$driver_status" >"$probe_stderr.info" 2>"$probe_stderr"
then
  probe_ok=1
fi
probe_error=$(cat "$probe_stderr" 2>/dev/null || true)
rm -f -- "$probe_stderr" "$probe_stderr.info"

if [[ $probe_ok -ne 1 ]]; then
  echo "record-x11.sh error: MP4 validation failed: $probe_error" >&2
  exit 1
fi
if [[ $driver_status != 0 ]]; then
  echo "record-x11.sh error: demo driver failed with status $driver_status; inspect driver.log and winlane-demo-x11.mp4" >&2
  exit 1
fi

printf 'mp4: %s\n' "$mp4_file"
printf 'capture info: %s\n' "$capture_info"
printf 'driver log: %s\n' "$output_dir/driver.log"
printf 'evidence export: %s\n' "$output_dir/evidence-export"
