//! Shared configuration for every `prop_*` property test in the workspace.
//!
//! All `prop_*` tests should run with [`vosu_proptest_config`] so case
//! counts, RNG seeding and failure persistence stay consistent: a fixed seed
//! for deterministic CI runs, `PROPTEST_CASES` honoured through proptest's
//! own environment handling, and a `VOSU_PROPTEST_RANDOM=1` escape hatch for
//! the nightly workflow to explore with a fresh seed and a much larger case
//! count.

use proptest::prelude::ProptestConfig;
use proptest::test_runner::{FileFailurePersistence, RngSeed};

/// Arbitrary but fixed seed bytes, chosen once and never changed: changing
/// them would silently stop reproducing previously-shrunk regressions.
const FIXED_SEED: u64 = 0x766f_7375_5f70_726f;

/// Config shared by every `prop_*` test.
///
/// `failure_persistence` deliberately uses `Direct` rather than the default
/// `SourceParallel`: `SourceParallel` locates the persistence file by
/// walking up from the compiled source path to find `lib.rs`, which is
/// fragile for property tests pulled into their parent module with
/// `include!` (net/updater) versus declared as an ordinary child module
/// (server/runtime). `Direct` always resolves the same crate-root-relative
/// file regardless of which pattern a given target uses, so `cargo test
/// --lib` reliably persists and reloads regressions.
pub fn vosu_proptest_config() -> ProptestConfig {
    let rng_seed = if std::env::var("VOSU_PROPTEST_RANDOM").as_deref() == Ok("1") {
        ProptestConfig::default().rng_seed
    } else {
        RngSeed::Fixed(FIXED_SEED)
    };
    ProptestConfig {
        failure_persistence: Some(Box::new(FileFailurePersistence::Direct(
            "proptest-regressions/lib-prop-failures.txt",
        ))),
        rng_seed,
        ..ProptestConfig::default()
    }
}
