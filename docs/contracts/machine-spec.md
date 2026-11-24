# Machine contract

`MachineSpec` is the immutable input used to create a machine. It is deliberately small: name, architecture, memory, CPU count, disk size, install ISO, network mode, TPM, and boot mode.

Rules:

- Names are 1–48 ASCII letters, digits, `-`, or `_`.
- The default network mode is `isolated`.
- Windows 11 x64 uses UEFI and TPM.
- Legacy BIOS is x86_64-only and is not part of the first supported path.
- Machine state lives outside git. Nothing large or secret-bearing belongs in the repository.

`MachineHandle` is the backend-owned identity returned after creation. Callers do not assume that the state directory layout is stable across backends.

`CheckpointRef` describes a restorable machine state. A checkpoint includes disk state plus enough firmware and TPM metadata to avoid a machine that boots differently after restore.

`base_image_path` optionally names a validated golden qcow2. Runtime machines must use copy-on-write backing files; the golden image is never mutated. `guest_agent` requests a host-only QEMU guest-agent channel for semantic and file control.
