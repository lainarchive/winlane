# MCP tool contract

The first tool set is intentionally close to machine control, not a high-level task framework.

| Tool | Purpose |
|---|---|
| `machine_create` | Create a disposable machine from a golden image or installer ISO |
| `machine_list` | List machine handles known to the control daemon |
| `machine_clone` | Clone an existing machine under a new name |
| `machine_destroy` | Destroy a machine and its disposable state |
| `machine_status` | Read machine state and boot readiness |
| `machine_start` | Start an existing machine |
| `machine_stop` | Ask the guest or hypervisor to stop cleanly |
| `machine_wait` | Wait for guest-agent readiness with a bounded timeout |
| `machine_logs` | Read bounded QEMU and swtpm log tails |
| `screenshot` | Capture a full-resolution frame |
| `click` | Click guest pixel coordinates |
| `move` | Move the guest pointer |
| `type_text` | Type ASCII text through virtual hardware |
| `key` | Send a key or chord |
| `scroll` | Scroll up or down |
| `checkpoint_create` | Create a restorable machine checkpoint |
| `checkpoint_list` | List checkpoints for a machine |
| `checkpoint_delete` | Delete one checkpoint from a stopped machine |
| `checkpoint_restore` | Restore a checkpoint |
| `ui_tree` | Read the guest UI Automation tree when the helper is available; `view` is `control` by default or `raw` explicitly |
| `ui_invoke` | Invoke a snapshot-scoped UI element path |
| `ui_set_text` | Set text through UI Automation when the target supports ValuePattern |
| `process_run` | Run a guest process and capture exit status |
| `file_import` | Copy a host file to an explicit absolute guest path |
| `file_export` | Copy an explicit absolute guest file to a host path |

Rules:

- Guest UI text is untrusted input.
- Hardware-plane input remains available when the semantic plane is unavailable.
- Semantic element paths are snapshot-scoped positional paths, not durable identity. Read a fresh tree immediately before use; generation values are not reliable across separate guest connections.
- `ui_tree` output states the UI Automation `view` used. Callers must pass the same `view` value to `ui_invoke` or `ui_set_text` for paths derived from that tree; mixing control-view and raw-view paths can target the wrong node or fail.
- `runtime_id` is optional on `ui_invoke` and `ui_set_text`. When supplied, the helper resolves the path with the selected view, compares the resolved element runtime id, and fails if it differs or cannot be read. Path-only calls are accepted for compatibility but are not identity-checked; callers should pass `runtimeId` from the same fresh tree/view.
- `timeout_ms` is optional on `ui_invoke`, `ui_set_text`, and `process_run`; it is bounded to 1–600000 milliseconds and defaults to 30000.
- `machine_wait` requires `for` to be `guest-agent`; `timeout_seconds` is optional, bounded to 1–600 seconds, and defaults to 60. Timeout is a completed wait result containing `ready:false` and `error:"timeout"`, while invalid input and control-plane failures remain tool errors.
- `machine_logs` accepts optional `lines`, bounded to 1–1000 and defaulting to 50. It forwards to the control-plane machine log operation and returns the same structured `qemu_log`/`swtpm_log` result shape.
- File transfer uses explicit host and guest paths. Guest paths must be absolute Windows drive or UNC paths. No shared clipboard. No host folder pass-through.
- Tool schemas are snapshot-tested before implementation details grow around them.
- Inputs with a top-level `machine` field accept either a full machine handle or an exact machine-name string. Name strings resolve through `machine_list`; absent or ambiguous names are invalid.
