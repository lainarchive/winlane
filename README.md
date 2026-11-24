# winlane

Disposable Windows machines for coding agents, visible and controllable from a terminal.

winlane is local-first. It provisions Windows into a golden qcow2 image, boots copy-on-write machines under QEMU/KVM, exposes the display in a terminal, and gives agents two control planes: hardware input through QMP and semantic Windows UI Automation through the guest agent.

The first supported path is narrow on purpose: Linux x86_64, KVM, QEMU, Windows 11 x64, UEFI, swtpm, virtio drivers, isolated networking, and kitty or truecolor half-block terminal rendering.

## Why

Agents can already operate cloud browsers and Linux shells. Windows desktop software is different: installers, browsers, Electron apps, enterprise tools, and OS dialogs still assume a real machine. winlane makes that machine disposable and inspectable without sending the desktop to a hosted service.

The interesting part is not "Windows in a terminal" by itself. It is the combination of:

- unattended Windows provisioning with measured timings and readiness evidence
- copy-on-write machine clones that never mutate the golden image
- terminal rendering plus hardware keyboard, mouse, paste, scroll, and screenshot control
- UI Automation tree, invoke, and set-text operations running in the interactive Windows session
- process execution and explicit file import/export through QEMU Guest Agent
- a local MCP boundary for agent control
- an append-only evidence model for machine actions

## Current status

winlane is a working private alpha, not a finished release.

Validated on real Windows 11 Enterprise Evaluation guests:

- two fresh unattended installs completed end-to-end using the committed boot arrangement, about 14.5 minutes each
- an integrated daemon booted a copy-on-write overlay from a golden image
- the golden image hash stayed unchanged after boot and control operations
- QEMU Guest Agent readiness, process execution, and file import/export round trips passed
- frame capture returned a real 1024x768 Windows screen
- UI Automation returned real desktop and Notepad nodes from the interactive session
- invoking a visible Notepad control succeeded through the semantic UI path
- QEMU lifecycle and checkpoint behavior passed real QEMU/KVM tests on blank machines

Not yet proven end-to-end:

- checkpoint restore on a booted Windows image
- visual proof of hardware input changing a Windows desktop state
- network policy behavior inside the guest
- terminal input behavior across a matrix of real terminal emulators
- semantic MCP tools end-to-end against a live machine
- long-running stability, concurrent machine use, and multi-machine cleanup

## Requirements

Host:

- Linux x86_64 with KVM access
- QEMU with `qemu-system-x86_64` and `qemu-img`
- OVMF UEFI firmware
- swtpm
- Rust stable, using the repository toolchain file
- a terminal with kitty graphics support, or any truecolor terminal for half-block rendering

Guest media:

- a licensed or evaluation Windows 11 x64 ISO
- the Fedora virtio-win driver ISO

winlane does not download or redistribute Windows. Evaluation media has its own Microsoft license terms.

## Build

```sh
cargo build --workspace
```

## Provision a golden image

```sh
cargo run -p winlane-provision -- provision \
  --name win11-alpha \
  --iso /path/to/windows-11-enterprise-eval-x64-en-us.iso \
  --virtio-iso /path/to/virtio-win.iso \
  --artifact-root /path/to/winlane-state \
  --timeout-minutes 75 \
  --json
```

The output golden image is written under the artifact root as `win11-alpha/golden.qcow2`. Provisioning writes a machine-readable report next to it. ISOs, disks, screenshots, reports, and machine state are deliberately ignored by git.

## Start the control daemon

```sh
cargo run -p winlane-cli -- daemon \
  --root /path/to/winlane-runtime \
  --control-socket /tmp/winlane-control.sock
```

Keep that process running in its own terminal or service manager.

## Create and boot a disposable machine

```sh
machine=$(cargo run -p winlane-cli -- machine create \
  --control-socket /tmp/winlane-control.sock \
  --name demo-1 \
  --base-image /path/to/winlane-state/win11-alpha/golden.qcow2)

cargo run -p winlane-cli -- machine start \
  --control-socket /tmp/winlane-control.sock \
  --machine-json "$machine"
```

`machine create` prints one JSON object. The current alpha passes that object back to lifecycle, view, and guest commands. This is functional, but it is not the final CLI ergonomics.

## Watch and control the display

```sh
cargo run -p winlane-cli -- view \
  --control-socket /tmp/winlane-control.sock \
  --machine-json "$machine"
```

Ctrl-Q detaches. Terminal paste is normalized and stripped of control bytes before being typed into the guest. Unsupported Unicode text is dropped by the hardware keyboard path rather than being turned into incorrect key events.

## Use the semantic guest plane

Run a process:

```sh
cargo run -p winlane-cli -- guest run \
  --machine-json "$machine" \
  --command 'C:\Windows\System32\cmd.exe' \
  --arg /c \
  --arg 'echo winlane-guest'
```

Read the UI Automation tree:

```sh
cargo run -p winlane-cli -- guest ui-tree \
  --machine-json "$machine" \
  --max-depth 8 \
  --max-nodes 2000
```

Invoke an element from the current tree:

```sh
cargo run -p winlane-cli -- guest invoke \
  --machine-json "$machine" \
  --node 0/1/3
```

Node paths are snapshot-scoped. A path such as `0/1/3` can point somewhere else after windows open, close, or reorder. Generation checking is not finished yet, so use a fresh tree immediately before invoking.

Import and export files explicitly:

```sh
cargo run -p winlane-cli -- guest import \
  --machine-json "$machine" \
  --host-path ./payload.txt \
  --guest-path 'C:\Windows\Temp\payload.txt'

cargo run -p winlane-cli -- guest export \
  --machine-json "$machine" \
  --guest-path 'C:\Windows\Temp\payload.txt' \
  --host-path ./payload.out.txt
```

There is no shared clipboard and no host folder pass-through.

## MCP

Run the stdio MCP server against the control daemon:

```sh
cargo run -p winlane-cli -- mcp \
  --control-socket /tmp/winlane-control.sock \
  --evidence-dir /path/to/evidence
```

The hardware-plane MCP tools cover machine lifecycle, screenshots, input, scrolling, and checkpoints. With `--evidence-dir`, those operations append JSONL evidence events. Typed text is stored as hash plus length, not raw text.

Semantic guest MCP tools are being wired separately; do not treat them as available until they appear in `tools/list` on the integration branch.

## Stop the machine

```sh
cargo run -p winlane-cli -- machine stop \
  --control-socket /tmp/winlane-control.sock \
  --machine-json "$machine"
```

The daemon does not yet have a shutdown operation. If it is externally terminated, its Unix control socket file can remain behind even though machine sockets and QEMU processes are cleaned up.

## Architecture

```text
winlane-cli
  ├─ control daemon ── winlane-ipc ── winlane-core ── QEMU/QMP/swtpm
  ├─ terminal view ─── winlane-view ── kitty / half-block rendering
  ├─ guest control ── winlane-guest ── QEMU Guest Agent / UI Automation
  └─ MCP stdio ────── winlane-mcp ─── control IPC + evidence events

winlane-provision
  └─ unattend.xml + virtio + UEFI + TPM + qcow2 golden image
```

The security boundary is the VM. Host clipboard sharing, host folder sharing, and implicit networking are intentionally absent. Guest UI text is untrusted input; agents should treat it like any other content that may contain prompt injection.

## Repository layout

- `crates/winlane-contracts` — shared machine, control, frame, checkpoint, and evidence types
- `crates/winlane-core` — QEMU lifecycle, QMP, input, screenshots, clones, checkpoints
- `crates/winlane-provision` — unattended Windows installation and readiness probes
- `crates/winlane-view` — terminal capability detection, frame conversion, rendering, input mapping
- `crates/winlane-guest` — QEMU Guest Agent, interactive-session UI Automation, process/file control
- `crates/winlane-ipc` — serialized Unix-socket machine control boundary
- `crates/winlane-mcp` — MCP tool schemas and stdio server
- `crates/winlane-evidence` — append-only JSONL events and content-addressed blobs
- `crates/winlane-cli` — user-facing command wiring

## Development gates

```sh
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Windows integration claims must come from real runs. Do not document simulated provisioning, fake UIA trees, or fixture-only behavior as live validation.
