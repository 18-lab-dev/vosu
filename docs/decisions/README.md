# Architecture decision records

These records document load-bearing choices visible in the current vosu implementation. They explain the constraints the code enforces; they do not reconstruct motives or dates that are not present in the repository.

## Records

| ADR | Status | Decision |
| --- | --- | --- |
| [0001](0001-single-daemon-multi-instance-lifecycle.md) | Accepted | One daemon owns a deterministic multi-instance lifecycle |
| [0002](0002-immutable-latest-only-snapshots.md) | Accepted | Publish immutable, latest-only API snapshots |
| [0003](0003-process-memory-platform-boundary.md) | Accepted | Keep process-memory access behind a platform boundary |
| [0004](0004-separate-calculation-contracts.md) | Accepted | Keep calculation contracts and engines separate |
| [0005](0005-root-confined-file-serving.md) | Accepted | Serve files through root-confined retained handles |
| [0006](0006-transactional-self-update.md) | Accepted | Perform self-updates as durable transactions |
| [0007](0007-vosu-clean-cutover.md) | Accepted | Adopt vosu as the sole product identity with a one-time state migration |
| [0008](0008-external-in-game-overlays.md) | Accepted | Keep in-game overlays outside vosu |

See the system context in [Architecture](../architecture.md) and the measured hot paths in [Benchmarks](../benchmarks.md).

## Adding or changing a decision

1. Add the next unused zero-padded number; never renumber an accepted record.
2. Record one decision with **Status**, **Context**, **Decision**, **Consequences**, and **Alternatives considered**. Link the source that currently enforces it and related documentation.
3. Describe verified constraints and trade-offs, not an inferred chronology. Do not add a date unless repository evidence establishes it.
4. Add the record to the table above in numeric order.
5. To replace a decision, add a new ADR, mark the old one `Superseded by ADR NNNN` with a link to that record, and link back from the replacement. Do not rewrite the old decision to match the new design.

Use `Proposed` while a decision is under review, `Accepted` while it governs the implementation, and `Superseded` only with a replacement link.
