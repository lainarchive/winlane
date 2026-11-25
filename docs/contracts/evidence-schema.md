# Evidence contract

Evidence is append-only. Events are ordered by sequence number, not by wall clock.

Each event has:

- `sequence`: monotonically increasing integer
- `unix_ms`: capture time in milliseconds since the Unix epoch
- `machine_id`: optional machine identity
- `kind`: stable lowercase event name such as `machine_started`
- `payload`: JSON object specific to the event

Large MCP `process_run` stdout/stderr and `ui_tree` result payloads are currently stored by content hash. Events link to those blobs instead of embedding those large payloads. Frame artifacts, transferred file contents, and installer logs are not yet uniformly content-addressed; do not infer blob references except through the explicit verification convention below.

The MCP recorder currently emits operation-specific kinds including `machine_created`, `machine_cloned`, `machine_destroyed`, `machine_started`, `machine_stopped`, `machine_status`, `frame_captured`, `input_sent`, `ui_tree_read`, `process_exited`, `file_imported`, `file_exported`, `checkpoint_created`, `checkpoint_deleted`, and `checkpoint_restored`. Kind validation checks shape rather than restricting events to a closed list; `error` and future operation-specific kinds may also appear.

The evidence log can contain secrets visible on the guest screen or in command arguments. Treat exports as sensitive artifacts.

## Verification scope

`winlane evidence verify --evidence-dir PATH` validates `events.jsonl` structure, event kind/payload shape, and contiguous zero-based sequence numbers. Because first-pass event kinds do not yet use a uniform blob-reference schema, verification uses one explicit convention: every string-valued payload object field named `blob`, found recursively, is treated as a content-addressed blob reference. Those references must be lowercase SHA-256 hex, exist under `blobs/`, and stream-hash to the referenced digest. Values under `sha256` or other field names are metadata hashes and are not interpreted as blob references. Unreferenced files in `blobs/` are not verification failures.
