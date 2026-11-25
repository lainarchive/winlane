#!/usr/bin/env bash
set -uo pipefail

if [[ $# -ne 4 ]]; then
  echo "usage: $0 WINLANE_BIN CONTROL_SOCKET MACHINE_NAME TIMEOUT_SECONDS" >&2
  exit 2
fi

winlane_bin=$1
control_socket=$2
machine_name=$3
timeout_seconds=$4

if [[ ! -x $winlane_bin ]]; then
  echo "wait-for-guest error: winlane binary is not executable: $winlane_bin" >&2
  exit 2
fi
if [[ ! $timeout_seconds =~ ^[1-9][0-9]*$ ]]; then
  echo "wait-for-guest error: timeout must be a positive integer" >&2
  exit 2
fi

printf 'waiting up to %ss for the guest agent on %s\n' "$timeout_seconds" "$machine_name"
deadline=$((SECONDS + timeout_seconds))
last_output=""
checks=0
while (( SECONDS < deadline )); do
  checks=$((checks + 1))
  if last_output=$("$winlane_bin" guest run \
    --machine "$machine_name" \
    --control-socket "$control_socket" \
    --command 'C:\Windows\System32\cmd.exe' \
    --arg /c \
    --arg exit \
    --arg 0 \
    --timeout-ms 5000 2>&1); then
    printf 'guest agent ready after %s check(s)\n' "$checks"
    exit 0
  fi
  if (( checks % 10 == 0 )); then
    printf 'still waiting for guest agent (%s checks)\n' "$checks"
  fi
  sleep 2
done

printf 'wait-for-guest error: guest agent did not become ready within %ss\n' "$timeout_seconds" >&2
if [[ -n $last_output ]]; then
  printf 'last guest probe error: %s\n' "$last_output" >&2
fi
exit 1
