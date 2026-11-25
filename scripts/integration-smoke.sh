#!/usr/bin/env bash
set -euo pipefail

stage="${1:-contracts}"

case "$stage" in
  contracts)
    cargo test -p winlane-contracts
    ;;
  build)
    cargo test --workspace
    ;;
  linux-vm)
    cargo test -p winlane-core --test control real_qemu_machine_control_proof -- --ignored --nocapture
    ;;
  windows-setup)
    : "${WINLANE_WINDOWS_ISO:?set WINLANE_WINDOWS_ISO to a licensed or evaluation Windows ISO}"
    : "${WINLANE_VIRTIO_ISO:?set WINLANE_VIRTIO_ISO to the virtio-win ISO}"
    : "${WINLANE_ARTIFACT_ROOT:?set WINLANE_ARTIFACT_ROOT to a writable artifact directory}"
    cargo run -p winlane-provision -- provision \
      --name "${WINLANE_MACHINE_NAME:-win11-smoke}" \
      --iso "$WINLANE_WINDOWS_ISO" \
      --virtio-iso "$WINLANE_VIRTIO_ISO" \
      --artifact-root "$WINLANE_ARTIFACT_ROOT" \
      --timeout-minutes "${WINLANE_TIMEOUT_MINUTES:-75}" \
      --json
    ;;
  *)
    echo "usage: $0 [contracts|build|linux-vm|windows-setup]" >&2
    exit 2
    ;;
esac
