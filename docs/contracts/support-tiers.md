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
- Ghostty, WezTerm, iTerm2, sixel, and unreleased-tmux Kitty rendering variants
- higher-rate guest-side capture

Experimental means the code exists and has run once. It does not mean the path is supportable.

## Out of scope for the first release

- GPU acceleration and gaming
- shared clipboard
- shared host folders
- deterministic replay of wall-clock GUI timing
- x64 Windows guests on ARM hosts
