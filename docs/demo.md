# Demo recording

`scripts/demo/record-x11.sh` is the high-resolution demo recorder. It records the actual pixels shown by a modern kitty terminal: one kitty split runs `winlane view` directly without tmux, and the other split runs the real CLI/MCP driver. ffmpeg captures the bare Xvfb display with `x11grab`; asciinema/term-stream terminal rasterization is not used for this artifact.

The older `scripts/demo/record.sh` tmux/asciinema harness is retained only as a legacy/fallback terminal transcript. Because released tmux does not reliably manage Kitty image placements per pane, that harness shows the explicitly labeled half-block preview and is not representative of high-resolution winlane rendering.

## Prerequisites

Build the release CLI first:

```sh
cargo build --release -p winlane-cli
```

Required host tools: Xvfb, ffmpeg/ffprobe, python3, `xwininfo`, and `xdpyinfo`. The harness does not install QEMU, Windows, drivers, X11 tools, or ffmpeg; kitty is downloaded into the user cache rather than system-installed.

A modern kitty binary is required. By default the recorder downloads the official portable Linux kitty 0.51.1 release into the user cache outside the repository and verifies its pinned SHA-256 before use. To use an existing modern kitty instead, pass `--kitty-bin /path/to/kitty` or set `WINLANE_KITTY_BIN=/path/to/kitty`. Do not put the binary in the repository.

Required inputs:

- a provisioned golden image such as `/path/to/winlane-state/win11-alpha/golden.qcow2`
- a writable runtime root
- a control socket path that does not already exist
- an absent or empty evidence directory
- an absent or empty output directory

## Record

```sh
scripts/demo/record-x11.sh \
  --golden-image /path/to/winlane-state/win11-alpha/golden.qcow2 \
  --runtime-root /path/to/winlane-runtime \
  --control-socket /tmp/winlane-demo-control.sock \
  --evidence-dir /path/to/demo-evidence \
  --output-dir /path/to/demo-output
```

Optional arguments:

```sh
  --machine-name winlane-demo-1
  --kitty-bin /path/to/kitty
  --winlane-bin /path/to/winlane
```

`--driver-script` exists as a validation hook for mechanical harness tests; normal demo runs should omit it.

## What is recorded

Xvfb creates a bare 1920x1080 display. One kitty OS window fills that display, so there is no VM window chrome or desktop noise to crop. Kitty's native splits provide two readable terminal panes:

- left: a view supervisor running `winlane view` directly in kitty
- right: the real CLI/MCP control sequence

The driver signals the supervisor through a small state file. The supervisor starts or restarts `winlane view` after the initial machine start and after the stop/restore/start sequence. `winlane view` is not inside tmux, so terminal detection can use the Kitty graphics path.

ffmpeg starts only after the driver proves interactive desktop/UIA readiness. The boot sequence and initial machine setup are therefore not presented as part of the recorded action sequence. The terminal scrollback visible when recording starts can still contain prior create/start/probe output, which is intentional evidence of that setup rather than a claim that boot pixels were captured.

The sequence creates and starts a disposable machine, launches Notepad in the interactive session, dismisses and verifies absence of the Notepad first-run popup when present, writes `before checkpoint` through MCP `ui_set_text`, creates an MCP checkpoint, writes `this never happened`, then stops/restores/starts because the current backend requires that sequence. After restore, the harness reads a fresh UI Automation node, runs a guest-side PowerShell reader in the interactive session, exports its JSON result, and requires the Notepad document text from `ValuePattern` or `TextPattern` to be exactly `before checkpoint`. Any mismatch or missing read result fails the demo. It then stops and destroys the machine. Evidence is exported under the output directory.

Outputs:

- `winlane-demo-x11.mp4` — real-time 1920x1080 H.264 x11grab capture; no time compression, speedup, or fabricated terminal output
- `capture-info.json` — MP4 dimensions/duration/codec/SHA-256, kitty version/path, display number, crop/time-compression declarations, explicit post-readiness recording disclosure, and driver exit status
- `driver.log` and `driver-status` — real driver output and exit status
- `ffmpeg.log`, `kitty.log`, `xvfb.log`, and `daemon.log`
- `evidence-export/`

Once recording has started, the MP4 is written and `capture-info.json` is validated even when the driver fails, so the failure remains inspectable instead of being hidden. If the driver fails before interactive-desktop readiness, recording never starts and those capture artifacts do not exist.

## Caveats

- The golden image must already provision Windows, virtio drivers, QEMU Guest Agent, and the interactive user session used by winlane.
- Notepad and the post-restore text reader run through temporary Scheduled Tasks using the portable `<GroupId>S-1-5-4</GroupId>` principal. If no interactive session exists, the demo correctly fails.
- UI Automation paths are read fresh immediately before each write and before the post-restore text read. Runtime-id checks are used for semantic writes; the independent post-restore reader verifies the freshly resolved text node content exactly.
- Running checkpoints can be large and slow; checkpoint timeout is 300 seconds in each direction. Real-time recording means long operations produce a long MP4.
- The fixed 1920x1080 layout is intended to make the left Windows view approximately native size for a 1024x768 guest while retaining a readable driver pane. It is still a terminal demo, not a Windows compositor.
- Temporary driver files are removed. The daemon, Xvfb display, kitty process, ffmpeg process, machine, and view supervisor are cleaned up on normal failure or interruption; a hard host crash can still leave machine state under the runtime root.

## Legacy tmux/asciinema recording

`scripts/demo/record.sh` remains useful when only a terminal transcript is needed or when ffmpeg/X11 cannot run. Its outputs (`winlane-demo.cast` and optional gif/mp4 conversions) use conservative half-block rendering and asciinema idle-time compression. Do not publish it as evidence of Kitty high-resolution rendering.
