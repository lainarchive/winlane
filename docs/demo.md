# Demo recording

`scripts/demo/record.sh` records a real terminal demo with tmux and asciinema. It uses the built `winlane` binary, a provisioned Windows golden qcow2 image, the control daemon, `winlane view`, CLI guest operations, and MCP semantic/checkpoint tools. It does not fabricate frames or write committed demo artifacts.

## Prerequisites

Build the CLI first:

```sh
cargo build -p winlane-cli
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

The sequence creates and starts a disposable machine, launches Notepad in the interactive session, writes `before checkpoint` through MCP `ui_set_text`, creates an MCP checkpoint, writes `this never happened`, stops/restores/starts because the current backend requires that sequence, verifies that the restored desktop is responsive and Notepad is present, then stops and destroys the machine. Evidence is exported under the output directory.

Outputs:

- `winlane-demo.cast`
- `winlane-demo.gif` only when `agg` is installed
- `winlane-demo.mp4` only when `agg` and ffmpeg are installed
- `evidence-export/`

The cast is recorded with asciinema `--idle-time-limit 2`; long idle waits are compressed, but terminal output is not fabricated or edited.

## Caveats

- Because the view runs inside tmux, the conservative half-block renderer is used even when the outer terminal supports kitty graphics.
- The golden image must already provision Windows, virtio drivers, QEMU Guest Agent, and the interactive user session used by winlane.
- Notepad is launched through a temporary Scheduled Task using the `INTERACTIVE` principal. If no interactive session exists, the demo correctly fails.
- UI Automation paths are read fresh immediately before each write.
- Running checkpoints can be large and slow; checkpoint timeout is 300 seconds in each direction.
- The harness does not install tmux, asciinema, agg, ffmpeg, QEMU, Windows, or driver dependencies.
- Temporary driver files are removed. The daemon, tmux session, machine, and recording processes are cleaned up on normal failure or interruption; a hard host crash can still leave machine state under the runtime root.
