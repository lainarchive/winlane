# winlane

Disposable Windows machines for coding agents, visible and controllable from a terminal.

winlane is being built in private. The first supported path is narrow on purpose: Linux x86_64, KVM, QEMU, Windows 11 x64, isolated networking, and a terminal viewer. Broader host support lands only after that path survives repeated end-to-end runs.

## Shape

- QEMU owns the machine boundary.
- QMP provides hardware-plane screenshots and input.
- A guest helper exposes UI Automation, process execution, and file transfer once Windows is running.
- Golden images are cloned with copy-on-write disks.
- Every meaningful action is written to a local evidence log.

## Status

Scaffold and interface contracts. Nothing here is a working Windows control plane yet.
