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
  linux-vm|windows-setup|windows-desktop|checkpoint|uia|evidence)
    echo "stage '$stage' has no runner yet" >&2
    exit 2
    ;;
  *)
    echo "usage: $0 [contracts|build|linux-vm|windows-setup|windows-desktop|checkpoint|uia|evidence]" >&2
    exit 2
    ;;
esac
