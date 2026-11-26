use super::*;

mod helpers;
/// Level batching: one native batch per dependency level.
mod levels;
mod native;
mod outstanding;
mod stages;
mod sync;

/// See `tests::zero_alloc` for the D17 M1 zero-allocation proof (split
/// into its own file so this module stays under the maintained-file line
/// limit, `docs/architecture/source-map.md`).
mod zero_alloc;

/// See `tests::tests_prop` for the F2 `execute`/`execute_bytes`
/// differential property test (split out for the same reason as
/// `zero_alloc`: `docs/architecture/source-map.md`'s maintained-file
/// line limit).
mod tests_prop;

/// See `tests::edge_cases` for the dead-target/ref-op-width precedence
/// and `READ_PLAN_MAX_BYTES` cap tests (split for the same reason as
/// `zero_alloc`).
mod edge_cases;
