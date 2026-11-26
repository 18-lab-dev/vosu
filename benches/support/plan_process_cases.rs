//! The `plan_memory` capture cases, shared verbatim by
//! `benches/plan_memory_bench.rs` (its `_native`/`_plan` cases, in core)
//! and the production bench plugin (`examples/brokered_reads_plugin.rs`,
//! its `_process` cases in a real `vosu plugin-host` child), so every
//! transport runs exactly the same reader calls and their outputs can be
//! compared by digest. Each function is one Criterion iteration.

use std::fmt::Debug;

use sha2::{Digest, Sha256};
use vosu_osu_reader::memory::{
    LazerMemory, MemoryReader, ReaderSynchronization, StableMemory, SynchronizedMemoryReader,
};
use vosu_plugin_api::memory::ReadStatus;

use crate::stable_fixture::{FIRST_OBJECT_TIME, PLAY_TIME};

/// The four capture cases measured through each transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Case {
    StablePrecise,
    LazerPrecise,
    StableRegularFields,
    LazerRegularFields,
}

impl Case {
    pub const ALL: [Self; 4] = [
        Self::StablePrecise,
        Self::LazerPrecise,
        Self::StableRegularFields,
        Self::LazerRegularFields,
    ];

    /// The case stem: `plan_memory/<name>_{native,plan,process}`.
    pub fn name(self) -> &'static str {
        match self {
            Self::StablePrecise => "stable_precise",
            Self::LazerPrecise => "lazer_precise",
            Self::StableRegularFields => "stable_regular_fields",
            Self::LazerRegularFields => "lazer_regular_fields",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|case| case.name() == name)
    }
}

/// `stable_precise_*`: one precise global read.
pub fn stable_precise(reader: &mut StableMemory) -> Result<impl Debug + use<>, String> {
    reader
        .global_precise()
        .map_err(|error| format!("stable precise global: {error}"))
}

/// `stable_regular_fields`: the regular global, menu and gameplay fields.
pub fn stable_regular_fields(reader: &mut StableMemory) -> impl Debug + use<> {
    (
        reader.global(),
        reader.menu(""),
        reader.gameplay(PLAY_TIME, FIRST_OBJECT_TIME),
    )
}

/// `lazer_regular_fields`: the regular fields of a resolved lazer reader.
pub fn lazer_regular_fields(reader: &mut LazerMemory) -> impl Debug + use<> {
    (reader.global(), reader.menu(""), reader.gameplay(0.0, None))
}

/// `lazer_precise_*`: one populated precise cycle (synchronization, global,
/// 18-key overlay, hit errors), asserting the populated fixture decoded.
pub fn lazer_precise(
    reader: &mut LazerMemory,
    synchronization: ReaderSynchronization,
) -> Result<impl Debug + use<>, String> {
    reader.synchronize_precise(synchronization, 3);
    let global = reader
        .global_precise()
        .map_err(|error| format!("lazer precise global: {error}"))?;
    let keys = reader
        .key_overlay(3)
        .map_err(|error| format!("lazer precise keys: {error}"))?;
    let errors = reader
        .hit_errors(0)
        .map_err(|error| format!("lazer precise hit errors: {error}"))?;
    match (&global, &keys, &errors) {
        (ReadStatus::Ready(global), ReadStatus::Ready(keys), ReadStatus::Ready(errors)) => {
            assert_eq!(global.time, 322.0);
            assert_eq!(keys.len(), 18);
            assert_eq!(errors.index, 0);
        }
        _ => panic!("populated lazer precise fixture must be ready"),
    }
    Ok((global, keys, errors))
}

/// The SHA-256 of a capture's `Debug` text: equal digests mean equivalent
/// outputs across transports.
pub fn digest(capture: &impl Debug) -> String {
    Sha256::digest(format!("{capture:?}").as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
