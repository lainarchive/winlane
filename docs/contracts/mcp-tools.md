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
| `screenshot` | Capture a full-resolution frame |
| `click` | Click guest pixel coordinates |
| `move` | Move the guest pointer |
| `type_text` | Type ASCII text through virtual hardware |
| `key` | Send a key or chord |
| `scroll` | Scroll up or down |
| `checkpoint_create` | Create a restorable machine checkpoint |
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
- Semantic element references expire when the UI tree generation changes.
- `ui_tree` output states the UI Automation `view` used. Callers must pass the same `view` value to `ui_invoke` or `ui_set_text` for paths derived from that tree; mixing control-view and raw-view paths can target the wrong node or fail.
- File transfer uses explicit host and guest paths. Guest paths must be absolute Windows drive or UNC paths. No shared clipboard. No host folder pass-through.
- Tool schemas are snapshot-tested before implementation details grow around them.
- Inputs with a top-level `machine` field accept either a full machine handle or an exact machine-name string. Name strings resolve through `machine_list`; absent or ambiguous names are invalid.
