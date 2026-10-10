# Make live parity an optional diagnostic

- Status: Accepted
- Date: 2026-10-10

## Context

`tools/live-parity/` compares vosu with a pinned upstream reference while a
real osu! client is driven from menu to active play, pause, result, and exit.
Clean strict captures for Windows stable, Windows lazer, and native Linux lazer
are recorded for 1.0 in [verification](../testing/verification.md). Each
capture needs a graphical session, a pinned reference build, and a prepared
client profile on a shared VM. Many recorded attempts failed on capture
acquisition or attestation rather than on vosu payloads. Requiring a capture
for every state-timing change made routine changes wait on that campaign.

## Decision

1. Live parity is an optional maintainer diagnostic. It is not a required
   check before merging, pushing, or releasing.
2. State, schema, and timing changes are gated by targeted Rust contracts:
   the affected unit tests, `state_api_compat`, `server_compat`,
   `performance_invariants`, and native process checks as applicable.
3. Keep the tool, its unit tests, and its pinned evidence. Run a capture when
   investigating a suspected divergence from upstream, and record the result
   with its exact source and artifact identities.

## Consequences

Passing checks no longer show that live output matches upstream for a
specific client build. A divergence that targeted fixtures do not model can
reach a release until a user report or an optional capture finds it. Historical
captures remain evidence for their recorded source generations only.

## References

- [Testing and verification](../testing/verification.md)
- [Compatibility and calculation workflow](../workflows/compatibility-and-calculation.md)
