//! Immutable raw-memory publication contracts.
//!
//! These types are the ownership boundary between the two memory-reader workers
//! and the reducer. They deliberately retain each [`crate::process::MemoryRead`]
//! verbatim: a failed, pending, unchanged, or unsupported read is not converted
//! to a default value. Conditional reads use `None` only when the source loop did
//! not execute that branch; `Some(Ok(ReadStatus::…))` means the branch executed.
//!
//! Source correspondence:
//! - stable regular/precise control flow: `packages/tosu/src/instances/osuInstance.ts:26-251`;
//! - lazer regular/precise control flow: `packages/tosu/src/instances/lazerInstance.ts:164-375`;
//! - raw method unions: `packages/tosu/src/memory/types.ts` and
//!   `packages/tosu/src/memory/index.ts:46-63`.

mod builders;
mod models;

pub use builders::{RawPreciseSnapshotBuilder, RawRegularSnapshotBuilder};
pub use models::{
    ConditionalRead, InstanceEpoch, PreciseCaptureContext, PreciseSequenceError,
    RawPreciseSnapshot, RawRegularSnapshot, RegularCaptureContext,
};

#[cfg(test)]
mod tests;
