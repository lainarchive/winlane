# Integration tests

The scripted ladder currently covers:

1. contract tests — `scripts/integration-smoke.sh contracts`
2. workspace build — `scripts/integration-smoke.sh build`
3. Linux/QEMU lifecycle proof on the host — `scripts/integration-smoke.sh linux-vm`
4. unattended Windows setup — `WINLANE_WINDOWS_ISO=... WINLANE_VIRTIO_ISO=... WINLANE_ARTIFACT_ROOT=... scripts/integration-smoke.sh windows-setup`

Windows desktop control, checkpoints, UIA, MCP, and evidence validation require a provisioned golden image. Those proofs are currently run as live integration exercises rather than a single scripted pipeline; the README records exactly which ones have passed and which remain open.

A stage is not done because code exists. It is done when the corresponding host or guest proof passes on real machinery.
