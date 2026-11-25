# Terminal rendering matrix

This matrix records what has been visually validated. Detection alone is not validation: a terminal is listed as high-resolution only after real rendered pixels and terminal input inverse mapping were measured under X11.

## High-resolution Kitty graphics protocol

| Terminal | Validation install/version | Result |
|---|---|---|
| kitty | official portable Linux kitty 0.51.1 | Supported; prior validation showed exact aspect-fit rendering, resize/font-zoom behavior, and exact input inverse mapping under Xvfb. |
| WezTerm | `wezterm 20240203-110809-5046fc22`, installed Ubuntu 22.04 `wezterm` package at `/usr/bin/wezterm` | Supported. Xvfb validation showed exact rendered bounds in the initial, font-zoom, window-resize, and split-pane phases; measured bounds matched `RenderPlan` exactly (`997x748`, `1005x754`, `792x594`, and `500x375` in the harness geometry). Color sanity passed, split output clipped to the pane, and click inverse mapping reached the expected source pixel `(507,395)` exactly. |
| Ghostty | Ghostty 1.1.3 stable, installed from the community `mkasberg/ghostty-ubuntu` `1.1.3-0-ppa2` Ubuntu 22.04 amd64 deb (`ghostty_1.1.3-0.ppa2_amd64_22.04.deb`, SHA-256 `79388cbe70177114ac8199d9fbfc53ff83b6b906f4174d7995486ec232fa19a8`) | Supported. Xvfb validation showed exact rendered bounds in the initial, font-zoom, window-resize, and split-pane phases; measured bounds matched `RenderPlan` exactly (`938x704`, `936x702`, `704x528`, and `500x375` in the harness geometry). Color sanity passed, split output clipped to the pane, and click inverse mapping reached the expected source pixel `(512,396)` exactly. |

Runtime detection enables the Kitty graphics protocol for self-identified Ghostty and WezTerm sessions, matching the terminal-identity-based kitty detection. The validation evidence above is exact about the versions actually exercised; other versions are not separate validation claims.


## Incremental Kitty rendering negative result

A scratch prototype rendered each frame as independently placed 128-pixel Kitty image tiles. The placement/update/delete semantics were explicit enough to prototype, and WezTerm rendered the tiled output exactly in the Xvfb resize/split harness. Ghostty 1.1.3 repeatedly rendered the initial, font-zoom, and split phases exactly but omitted the final 16-pixel bottom tile row after window resize (512 pixels of the expected 528-pixel height were visible). Because terminal implementations diverged on clipping after resize, tiled Kitty rendering is not enabled.

The experiment was also not a compelling throughput win even before the Ghostty failure: in a synthetic 1024x768 workload where every tile changed at 30 fps, tiled output wrote about 72.8 MB to the terminal in an 8-second measurement versus about 64.9 MB for whole-frame output. Viewer CPU was about 37.9% versus 40.9%, and kitty CPU about 189% versus 208%, but the increased terminal bytes and unproven correctness do not justify the complexity.

The viewer therefore keeps whole-frame Kitty rendering. The only incremental optimization retained is a conservative cropped-viewport skip: when zoomed in, if the exact cropped source pixels are unchanged, the scaled output is necessarily unchanged and no repaint is emitted. This is used only after the captured artifact's SHA-256 validation succeeds; metadata is never trusted without validating artifact bytes.

## Cropped repaint suppression proof

`scripts/validate-cropped-repaint.py` is the reproducible harness for this optimization. It starts a real kitty window under Xvfb, serves deterministic 640x400 frame artifacts through a fake Unix control plane, runs `winlane view` at 30 fps and zoom 200, measures terminal writes with `strace`, clicks through the real terminal input path, and checks X11 pixels rather than escape sequences. The harness downloads the pinned kitty 0.51.1 archive when `--kitty-bin` is not supplied. Required host tools are python3, Xvfb, ffmpeg, xdotool, xwininfo, xdpyinfo, and strace.

```sh
cargo build -p winlane-cli
python3 scripts/validate-cropped-repaint.py --output-dir /tmp/winlane-cropped-repaint
```

The validation run recorded in this branch used kitty 0.51.1 and the debug `winlane` binary on Ubuntu 22.04. A center click at terminal pixel `(605,408)` reached source pixel `(321,202)` exactly, including the zoom-200 crop offset. During 120 captures whose bytes changed only outside the visible crop, the viewer emitted zero repaint writes and zero repaint bytes; a same-sized inside-crop repaint measured 19,770 bytes, so the suppressed phase avoided an estimated 2,372,400 terminal bytes. Viewer RSS stayed at 14,032 KiB across those captures. When pixels inside the crop changed, the next repaint replaced all red pixels with the new yellow frame and left zero stale red pixels. Window resize and font zoom each forced a new repaint despite unchanged source pixels (14,430 and 14,578 bytes in that run), and the split-pane probe clipped output to the left pane with zero red pixels in the right-side probe area.

This is control-plane/rendering validation with synthetic local frame artifacts, not a real Windows guest proof. No Windows/QEMU validation is claimed for this optimization.

## Conservative fallback or unvalidated paths

| Path | Status |
|---|---|
| Ghostty 1.4.0 community AppImage | Not validated here. The `pkgforge-dev/ghostty-appimage` x86_64 AppImage (SHA-256 `50ad4f8b54a01e7d41426e7d12ffae9c88aa1c516ac140755e2bca596e1d2ae6`) launched but its bundled OpenGL/EGL initialization failed with `error.BadMatch` under both Xvfb and Xephyr on this Ubuntu 22.04 VM. This was an environment/rendering startup failure, not evidence that its terminal image protocol is incorrect. |
| iTerm2 | Unsupported at runtime. iTerm2 uses its own OSC 1337 inline-image protocol rather than the Kitty graphics path used by winlane, and macOS is unavailable in this validation environment, so no rendering claim is made. The viewer detects it as an unsupported inline-image variant and uses half-block fallback. |
| tmux | Kitty passthrough is deliberately not enabled. Released tmux does not reliably manage Kitty image placements per pane, so winlane uses half-block fallback inside tmux. |
| sixel | Detected as an unsupported image variant but not encoded; half-block fallback is used. |
| Other terminals | Half-block preview fallback unless independently validated. |

## iTerm2 design note

A future iTerm2 renderer would need a separate OSC 1337 encoder rather than reuse of Kitty frames. It would still scale host-side to the same exact `RenderPlan`, but pixel sizing, subcell placement, clipping, resize invalidation, and input inverse mapping must be validated on macOS before enablement. None of those semantics are claimed here, and detection remains conservative fallback.

All Linux validation used real terminal windows and X11 pixel screenshots, not escape-sequence-only tests. No real Windows guest was required for these terminal-protocol measurements; the frame source was a deterministic local test artifact.
