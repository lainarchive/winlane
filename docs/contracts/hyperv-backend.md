# Hyper-V backend architecture

Status: experimental, unvalidated lifecycle command-construction groundwork exists in `winlane-core::HyperVBackend`. It is not wired into the control daemon, the workspace is not portable to Windows yet, and there is no host validation. The current supported backend remains Linux x86_64 QEMU/KVM.

## Decision

Build a native Hyper-V backend as a second host virtualization backend rather than treating Windows support as a Linux/KVM port. Use QEMU's Windows Hypervisor Platform (WHPX) accelerator only as a possible compatibility experiment, not as the primary architecture.

Rationale:

- WHPX keeps QEMU, QMP, virtio-serial QGA, qcow2, and much of the current backend model. That is the smaller migration path, but it remains QEMU-shaped rather than native Hyper-V management and does not answer the requested Hyper-V backend lane. QEMU documents WHPX as a Windows accelerator requiring the Windows Hypervisor Platform feature: <https://www.qemu.org/docs/master/system/whpx.html>.
- Native Hyper-V provides the Windows management surface directly: Generation 2 VMs, VHDX disks, checkpoints, integration services, PowerShell Direct, and virtualization WMI. It requires a new backend implementation and a new guest control channel, but it is the architecture that matches Windows-host operation.

This decision does not change licensing. Managed cloud Windows development machines are development/test infrastructure, not a reusable free Windows license or backend. The Windows 11 Enterprise Evaluation ISO is evaluation media with Microsoft license terms, not perpetual production licensing. Users remain responsible for licensed or evaluation guest media.

## Existing seams in winlane

The narrow reusable seams are already mostly contract-shaped:

| Surface | Current seam | Hyper-V impact |
|---|---|---|
| Machine identity | `MachineHandle.backend`, `state_dir`, `MachineSpec` | `backend` can distinguish `qemu` and `hyperv`; state layout must remain backend-private. |
| Lifecycle | `MachineBackend` contract trait | Suitable shape, but the control daemon currently stores/dispatches concrete `QemuBackend` instead of a backend enum or trait object. |
| Frames | `FrameSource` contract trait and `FrameRef` artifact hash | Reusable if Hyper-V frame bytes are normalized into the same immutable artifact model. |
| Hardware input | `InputSink`/`ControlEvent` | Reusable semantically; backend must map keyboard/mouse/scroll onto Hyper-V WMI devices. |
| Guest semantic control | `GuestControlPlane` concept plus current QGA implementation | Not reusable as-is because QGA is QEMU-specific. Needs a Hyper-V guest control implementation. |
| Evidence | append-only event kinds and content-addressed blobs | Reusable unchanged; evidence must not claim identical hypervisor internals across backends. |
| Checkpoints | opaque `CheckpointRef` | Reusable only as an opaque backend-local reference. qcow2 snapshot metadata is QEMU-specific and must not be reused for Hyper-V. |
| Disk images | `base_image_path` currently means validated golden qcow2 | Hyper-V needs VHDX. A conversion/migration path may be added later, but the contract must not silently treat qcow2 and VHDX as interchangeable. |

The largest structural gap is not the contract traits; it is composition. `winlane-ipc` constructs and serializes access to `QemuBackend` directly, while guest operations separately assume a QGA Unix socket path. A second backend needs a narrow dispatch point that selects backend behavior from trusted machine records, without exposing arbitrary backend names or paths to clients.

## Hyper-V primitives

Official Microsoft interfaces relevant to the smallest vertical slice:

- VM lifecycle and disk: Hyper-V PowerShell module such as `New-VM`, `Start-VM`, `Stop-VM`, `Get-VM`, `Remove-VM`, `New-VHD`, and related cmdlets. `New-VM` supports Generation 2 VMs and existing or new VHDX paths: <https://learn.microsoft.com/en-us/powershell/module/hyper-v/new-vm>.
- Firmware and TPM: Generation 2 firmware is managed through `Set-VMFirmware`, including Secure Boot; virtual TPM is managed through `Enable-VMTPM`: <https://learn.microsoft.com/en-us/powershell/module/hyper-v/set-vmfirmware>, <https://learn.microsoft.com/en-us/powershell/module/hyper-v/enable-vmtpm>.
- Checkpoints: Hyper-V standard checkpoints capture virtual-machine and memory state; production checkpoints use VSS/file-system freeze and do not capture memory state. winlane running-checkpoint semantics require standard checkpoints unless semantics are intentionally split: <https://learn.microsoft.com/en-us/windows-server/virtualization/hyper-v/checkpoints>.
- PowerShell Direct: `Invoke-Command`/`Enter-PSSession` can run PowerShell in a local running Windows VM regardless of network configuration, and persistent sessions support `Copy-Item`. It requires Hyper-V administrator context and valid guest credentials: <https://learn.microsoft.com/en-us/windows-server/virtualization/hyper-v/powershell-direct>.
- Hyper-V sockets: Windows supports custom host/guest integration services over `AF_HYPERV` sockets without network configuration. The data stream stays on the host; it is data-stream-only and not intended for block/video transfer: <https://learn.microsoft.com/en-us/windows-server/virtualization/hyper-v/make-integration-service>.
- Frame capture: virtualization WMI `Msvm_VirtualSystemManagementService.GetVirtualSystemThumbnailImage` returns raw RGB 565 image data for a requested width/height and can target the current VM or a snapshot. Access can be restricted by UAC and requires host Administrator context: <https://learn.microsoft.com/en-us/windows/win32/hyperv_v2/getvirtualsystemthumbnailimage-msvm-virtualsystemmanagementservice>.
- Hardware input: virtualization WMI exposes `Msvm_Keyboard.TypeText`/`TypeKey` and `Msvm_SyntheticMouse.SetAbsolutePosition`/`ClickButton`/`SetScrollPosition`: <https://learn.microsoft.com/en-us/windows/win32/hyperv_v2/typetext-msvm-keyboard>, <https://learn.microsoft.com/en-us/windows/win32/hyperv_v2/setabsoluteposition-msvm-syntheticmouse>.
- Enhanced Session Mode uses RDP through VMConnect for interactive viewing/device sharing. It is user/session-oriented and not a replacement for deterministic agent control or artifact evidence: <https://learn.microsoft.com/en-us/windows-server/virtualization/hyper-v/enhanced-session-mode>.

## The QGA/QMP replacement problem

QMP and QGA are QEMU-specific. PowerShell Direct is useful but is not a complete replacement:

- It can replace part of guest process execution and file transfer: invoke PowerShell in the guest and copy files through persistent sessions.
- It requires valid guest credentials and Hyper-V administrator context. That is a provisioning/bootstrap dependency, not an always-available hypervisor channel.
- It is PowerShell-remoting-shaped, not a streaming binary RPC protocol with bounded request/response framing.
- It does not provide hardware frame capture, keyboard scan/key events, absolute pointer control, or checkpoint control.
- UI Automation must run against the interactive Windows desktop session. Simply executing PowerShell through a noninteractive management channel is not sufficient evidence that UI Automation sees the same desktop the frame capture shows.

Therefore winlane needs its own small guest control service for Hyper-V. PowerShell Direct should bootstrap that service, not become the permanent semantic control plane.

Proposed control split:

1. **Hypervisor plane:** host-side Rust backend invokes local PowerShell/WMI/COM APIs for VM lifecycle, checkpoints, frame capture, and hardware input. This plane replaces QMP.
2. **Bootstrap plane:** provisioning creates a dedicated local guest account/credential and installs a winlane guest service. PowerShell Direct is the bootstrap and recovery path for service installation, process execution, and initial file placement.
3. **Guest control plane:** a minimal winlane guest service connects over a registered Hyper-V socket service ID. It provides newline/length-bounded binary-safe requests for process execution, file import/export, UI Automation tree/invoke/set-text, readiness, and diagnostics. This replaces QGA.
4. **Interactive UI lane:** the guest service launches UI Automation operations in the provisioned interactive session using an explicit scheduled-task/session mechanism equivalent in intent to the current QEMU path. This must be validated on a real host because session isolation is behaviorally important.

Hyper-V sockets are the preferred guest channel because they do not depend on guest networking and are explicitly supported for custom integration services. They do not remove the need for authentication/authorization design: service IDs, VM identity, host registry registration, local permissions, malformed input limits, and guest-service update compatibility need explicit validation before this becomes a trust boundary.

## Smallest vertical slice

The first implementation slice should be deliberately smaller than full parity:

1. Windows-host-only `hyperv` backend behind the existing lifecycle/frame/input contracts.
2. Generation 2 VM from an existing validated golden VHDX; no Hyper-V provisioning pipeline yet.
3. Lifecycle: define/list/start/stop/status/destroy through Hyper-V PowerShell/WMI.
4. Disk: create a copy-on-write differencing VHDX for runtime machines; clones are exported/flattened independent VHDX copies. Golden image is never mutated.
5. Checkpoints: standard Hyper-V checkpoints for running-machine memory/device semantics. Keep winlane's conservative stopped-only deletion rule initially. Record backend-specific checkpoint metadata under the opaque `CheckpointRef`; do not reuse qcow2 snapshot fields.
6. Frames: WMI thumbnail capture converted into the same immutable frame artifact and SHA-256 evidence model. Validate that requested full-resolution captures are exact, current, and not scaled placeholder images.
7. Input: WMI keyboard and synthetic mouse/scroll mapping. Validate ASCII typing, key chords, absolute click/move, and scroll against the interactive desktop.
8. Guest readiness/control: initially bootstrap with PowerShell Direct for a narrow readiness probe and process execution. File/UI semantics wait for the winlane guest service rather than pretending PowerShell Direct is equivalent.
9. Evidence: preserve current append-only events. Record backend capability/version facts without embedding credentials or host secrets.

Explicitly out of the first slice: Windows unattended provisioning, QEMU disk conversion tooling, networking automation beyond an existing host-selected switch, clones/restores across hosts, production checkpoints, Enhanced Session Mode automation, multi-user interactive sessions, and live migration.

## Current lifecycle groundwork boundary

`winlane-core::HyperVBackend` now contains only the conservative lifecycle core: create/define, start, stop, status, list, and destroy through PowerShell Hyper-V cmdlets, using a differencing VHDX over an existing golden VHDX. It is compile-safe on Linux but every lifecycle operation fails closed at runtime unless it is running on Windows. The implementation is deliberately not wired into the Unix-socket control daemon yet: the daemon transport itself is Linux-specific, so backend selection there would be untestable scaffolding rather than a usable Windows vertical slice.

The first implementation accepts only isolated networking and removes VM network adapters rather than pretending NAT semantics exist without host-switch automation. It requires a golden VHDX, rejects ISO provisioning, uses Generation 2 with Secure Boot, and configures a local key protector before enabling vTPM when `spec.tpm` is set. The persisted effective disk size is the backing VHDX virtual size rounded up to whole GiB; explicit requests larger than the backing disk fail closed because differencing-disk expansion is not yet validated. Clone, checkpoint, frame, hardware-input, and guest-control operations return explicit unsupported errors rather than fake success.

Portability required before this can become host-capable:

1. Split the control transport so Unix sockets remain the Linux implementation while Windows receives a supported named-pipe or equivalent local authenticated transport.
2. Gate Unix-only QEMU process, socket, and permission code so Windows can compile the shared contracts, core dispatch, IPC framing, and CLI surface.
3. Add a narrow backend enum in the control daemon that selects QEMU for existing `qemu` records and Hyper-V for `hyperv` records, while creation chooses only the supported host backend unless an explicit validated configuration says otherwise.
4. Keep Hyper-V frame/input/checkpoint operations fail-closed until WMI thumbnail/input and standard-checkpoint behavior are validated on the real Windows host.
5. Only after those steps compile and pass on both platforms should `HyperVBackend` be wired into daemon creation and real host validation begin.

## Implementation seam preparation

No speculative backend interface should be added yet. The safe preparation order is:

1. Keep contracts stable; document backend-specific semantics first.
2. When implementation starts on a real Windows host, introduce a narrow internal backend enum/dispatch in the control daemon for `qemu` and `hyperv` machine records. Do not expose arbitrary command execution or paths through IPC.
3. Move only truly QEMU-specific assumptions behind the QEMU backend: qcow2 backing chains, QMP sockets, QGA sockets, swtpm/OVMF state, qemu-img snapshots, and QEMU process liveness.
4. Keep evidence, artifact hashing, terminal rendering, MCP schemas, and machine reference resolution backend-neutral.
5. Add compile-time platform separation only where needed. Linux must keep building and passing the existing suite even before Hyper-V functionality exists.

## Validation required on real Windows Hyper-V hosts

None of the following can be validated from the current Linux worker sessions:

- Windows edition/feature matrix: Hyper-V availability generally requires Windows Pro/Enterprise/Education or Windows Server; exact supported versions and required optional features must be verified on the target host.
- Administrator/UAC requirements for WMI thumbnail, keyboard, mouse, checkpoint, and VM lifecycle operations.
- Generation 2 Secure Boot/vTPM configuration and credential/key-protector behavior.
- Full-resolution `GetVirtualSystemThumbnailImage` fidelity, latency, format conversion, scaling behavior, and correctness while locked, at logon, during resolution changes, or with no interactive user.
- WMI keyboard/mouse/scroll coverage and timing relative to the current QEMU hardware input matrix.
- Standard checkpoint memory/device consistency, restore behavior, deletion ordering, disk merge behavior, and interaction with differencing disks.
- PowerShell Direct credential storage, noninteractive session behavior, file-copy semantics, and whether it can reliably bootstrap the guest service.
- Hyper-V socket registration, service-ID permissions, host/guest reconnection, concurrent control operations, backpressure, malformed input handling, and service upgrade behavior.
- VHDX backing/flattening behavior, clone independence, performance, and space use.
- Real Windows 11 guest behavior end-to-end under Hyper-V, including UI Automation in the visible interactive session.

Until those pass, README and MCP/docs must continue to say Linux x86_64 QEMU/KVM is the only supported host backend.
