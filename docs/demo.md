# Demo recording

`scripts/demo/record.sh` records a real terminal demo with tmux and asciinema. It uses the built `winlane` binary, a provisioned Windows golden qcow2 image, the control daemon, `winlane view`, CLI guest operations, and MCP semantic/checkpoint tools. It does not fabricate frames or write committed demo artifacts.

## Prerequisites

Build the CLI first:

```sh
cargo build --release -p winlane-cli
```

Install tmux and asciinema with the host package manager. `agg` and ffmpeg are optional: if both exist, the script also writes a gif and mp4.

Required inputs:

- a provisioned golden image such as `/path/to/winlane-state/win11-alpha/golden.qcow2`
- a writable runtime root
- a control socket path that does not already exist
- an absent or empty evidence directory
- an absent or empty output directory

## Record

```sh
scripts/demo/record.sh \
  --golden-image /path/to/winlane-state/win11-alpha/golden.qcow2 \
  --runtime-root /path/to/winlane-runtime \
  --control-socket /tmp/winlane-demo-control.sock \
  --evidence-dir /path/to/demo-evidence \
  --output-dir /path/to/demo-output
```

Optional machine name, limited to letters, digits, underscore, and dash:

```sh
  --machine-name winlane-demo-1
```

## What is recorded

The recording attaches to one tmux session with two panes:

- left: `winlane view`, showing the real Windows desktop once the machine starts
- right: the real CLI/MCP control sequence

The sequence creates and starts a disposable machine, launches Notepad in the interactive session, writes `before checkpoint` through MCP `ui_set_text`, creates an MCP checkpoint, writes `this never happened`, then stops/restores/starts because the current backend requires that sequence. After restore, the harness reads a fresh UI Automation node, runs a guest-side PowerShell reader in the interactive session, exports its JSON result, and requires the Notepad document text from `ValuePattern` or `TextPattern` to be exactly `before checkpoint`. Any mismatch or missing read result fails the demo. It then stops and destroys the machine. Evidence is exported under the output directory.

Outputs:

- `winlane-demo.cast`
- `winlane-demo.gif` only when `agg` is installed
- `winlane-demo.mp4` only when `agg` and ffmpeg are installed
- `evidence-export/`

The cast is recorded with asciinema `--idle-time-limit 2`; long idle waits are compressed, but terminal output is not fabricated or edited.

## Caveats

- Because the view runs inside tmux, the conservative half-block preview renderer is used even when the outer terminal supports Kitty graphics. Released tmux does not reliably manage Kitty placements per pane; pane-aware support exists only in unreleased tmux master/next-3.9 work. The fallback reserves one row for a `HALF-BLOCK PREVIEW` label when space permits.
- The golden image must already provision Windows, virtio drivers, QEMU Guest Agent, and the interactive user session used by winlane.
- Notepad and the post-restore text reader run through temporary Scheduled Tasks using the portable `<GroupId>S-1-5-4</GroupId>` principal. If no interactive session exists, the demo correctly fails.
- UI Automation paths are read fresh immediately before each write and before the post-restore text read. Runtime-id checks are used for semantic writes; the independent post-restore reader verifies the freshly resolved text node content exactly.
- Running checkpoints can be large and slow; checkpoint timeout is 300 seconds in each direction.
- The harness does not install tmux, asciinema, agg, ffmpeg, QEMU, Windows, or driver dependencies.
- Temporary driver files are removed. The daemon, tmux session, machine, and recording processes are cleaned up on normal failure or interruption; a hard host crash can still leave machine state under the runtime root.
