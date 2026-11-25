# Support tiers

## Tier 1: first supported path

- Host: Linux x86_64 with KVM
- Guest: Windows 11 x64
- Hypervisor: QEMU with OVMF, swtpm, virtio storage and network drivers
- Viewer: Kitty graphics protocol where validated, truecolor half-block preview fallback elsewhere
- Network: isolated by default, NAT only by explicit selection

A claim belongs in this tier only after three clean end-to-end runs.

## Tier 2: experimental

- Windows x86_64 host with WHPX
- macOS Apple silicon host with Windows 11 ARM64
- independent validation of terminal graphics beyond kitty, Ghostty, and WezTerm
- higher-rate guest-side capture

Experimental means the code exists and has run once. It does not mean the path is supportable. kitty, Ghostty, and WezTerm use the high-resolution Kitty graphics path; see `terminal-matrix.md` for the exact validated versions and evidence. iTerm2, sixel, tmux, and other terminals are not experimental rendering variants at runtime: they conservatively use the supported half-block preview fallback until independently validated. Released tmux does not reliably manage Kitty image placements per pane. iTerm2 is detected only as an unsupported inline-image variant; Kitty graphics is not enabled there at any version, so the viewer uses half-block fallback.

## Out of scope for the first release

- GPU acceleration and gaming
- shared clipboard
- shared host folders
- deterministic replay of wall-clock GUI timing
- x64 Windows guests on ARM hosts
