# Evidence contract

Evidence is append-only. Events are ordered by sequence number, not by wall clock.

Each event has:

- `sequence`: monotonically increasing integer
- `unix_ms`: capture time in milliseconds since the Unix epoch
- `machine_id`: optional machine identity
- `kind`: stable lowercase event name such as `machine_started`
- `payload`: JSON object specific to the event

Frame blobs, transferred files, and installer logs are stored by content hash. Events link to those blobs instead of embedding large payloads.

Required first-pass event kinds:

- `machine_created`
- `machine_started`
- `frame_captured`
- `input_sent`
- `checkpoint_created`
- `checkpoint_restored`
- `file_imported`
- `file_exported`
- `process_started`
- `process_exited`
- `network_policy_changed`
- `error`

The evidence log can contain secrets visible on the guest screen or in command arguments. Treat exports as sensitive artifacts.
