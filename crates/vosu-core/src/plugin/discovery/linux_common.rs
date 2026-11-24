//! Linux `ProcessRecord` mapping shared between the push-event-backed
//! [`super::LinuxSource`] (`linux_source`, gated on the
//! `linux-process-events` feature) and the plain-polling fallback a
//! `--no-default-features` Linux build uses instead
//! (`src/runtime/discovery.rs::PollingLinuxSource`, P3) — kept here,
//! gated only on `target_os = "linux"`, so both reach the same mapping
//! instead of each keeping its own copy.

use std::ffi::OsString;

use vosu_plugin_api::Bitness;
use vosu_sys::process::{LinuxProcessMetadata, ProcessBitness};

use crate::plugin::host::ProcessRecord;

fn map_bitness(bitness: ProcessBitness) -> Bitness {
    match bitness {
        ProcessBitness::X86 => Bitness::X86,
        ProcessBitness::X64 => Bitness::X64,
    }
}

/// `ProcessRecord::command_line`'s own "joined argv" contract
/// (`crate::plugin::host::ProcessRecord`), space-joined to match
/// `MatcherEngine`'s `command_line_patterns` full-match semantics against
/// a single string (`matcher.rs`'s own doc comment).
fn join_command_line(parts: &[OsString]) -> String {
    parts
        .iter()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

#[must_use]
pub fn map_metadata(metadata: LinuxProcessMetadata) -> ProcessRecord {
    let argv = Some(
        metadata
            .command_line
            .iter()
            .map(|part| part.to_string_lossy().into_owned())
            .collect(),
    );
    ProcessRecord {
        pid: metadata.pid,
        name: metadata.name.to_string_lossy().into_owned(),
        path: metadata.executable.to_string_lossy().into_owned(),
        command_line: join_command_line(&metadata.command_line),
        bitness: map_bitness(metadata.bitness),
        start_time: metadata.start_time,
        argv,
    }
}
