# Change ownership

Ownership exists to keep parallel work from colliding. It is a review boundary, not a claim that one person forever owns the design.

| Area | Paths |
|---|---|
| Contracts | `crates/winlane-contracts`, `docs/contracts` |
| Machine control | `crates/winlane-core`, `crates/winlane-ipc` |
| Provisioning | `crates/winlane-provision`, provisioning fixtures |
| Terminal view | `crates/winlane-view`, terminal fixtures |
| Guest control and evidence | `crates/winlane-guest`, `crates/winlane-evidence` |
| Product shell | `crates/winlane-cli`, `crates/winlane-mcp` |

Rules:

- Contract changes land alone before implementation changes that depend on them.
- Large binary artifacts never enter git.
- Tests use per-run temporary directories and unique machine names.
- `Cargo.lock` changes are reviewed against the dependency that caused them.
- No subsystem may duplicate another subsystem's type because the shared contract felt inconvenient.
