//! X1's `plan_memory/*_process` cases (M5 D17 contract, "Process-runtime
//! osu! data"): the same stable and lazer captures as the `_plan` cases,
//! with the reader in a real `vosu plugin-host` child
//! (`examples/brokered_reads_plugin/x1.rs`) submitting through
//! `PlanProcessMemory`, the real relay and ring, core's real ingress,
//! broker and executor. Core executes each synthetic session's plans
//! against the same fixture memory the `_plan` cases read, through the
//! `bench-harness` session-memory hook. Required data, not a bundled gate.
//!
//! Before any timing, each case must decode to the same digest as its
//! `_plan` case, and the per-capture census is printed as
//! `METRIC plan_memory_<case>_process_<count>_{min,max,mean}` lines:
//! `submissions`, `stages`, `reads`, `read_bytes` (counted from each
//! submitted plan), `wire_frames`, `control_frames`, `submit_frames`,
//! `result_frames` (counted from the plugin host's own rings), `scans`
//! (local region walks, never brokered here) and `other_sends`.

use std::sync::Arc;
use std::time::Duration;

use vosu_osu_reader::memory::lazer_offsets::LazerOffsets;
use vosu_plugin_api::Bitness;
use vosu_plugin_api::memory::{ProcessBitness, ProcessMemory};

use crate::lazer_fixture::Fixture;
use crate::plan_process_cases::Case;
use crate::production::protocol::Fields;
use crate::production::{Instance, ProductionHost};

/// Captures per census.
const CENSUS_CAPTURES: u64 = 16;

/// A synthetic session: `(pid, process name, bitness)`. No real process
/// has these pids; core reads them only through the hook.
type Session = (u32, &'static str, Bitness);

const STABLE: Session = (0x7FF0_0001, "vosu-x1-stable", Bitness::X86);
const LAZER: Session = (0x7FF0_0002, "vosu-x1-lazer", Bitness::X64);
const ZEROS: Session = (0x7FF0_0003, "vosu-x1-zeros", Bitness::X64);

fn process_bitness(bitness: Bitness) -> ProcessBitness {
    match bitness {
        Bitness::X86 => ProcessBitness::X86,
        Bitness::X64 => ProcessBitness::X64,
    }
}

/// The running plugin host serving every `_process` case.
pub struct ProcessCases<'h> {
    instance: Instance<'h>,
    _lazer: Arc<Fixture>,
}

impl<'h> ProcessCases<'h> {
    /// Hooks the fixtures, starts the plugin host, attaches the sessions,
    /// checks every case's digest against `expected` (each `_plan` case's
    /// own) and prints the census.
    pub fn start(host: &'h ProductionHost, expected: &[(Case, String)]) -> Self {
        let lazer = Arc::new(Fixture::active_lazer(
            &LazerOffsets::embedded().expect("embedded lazer offsets"),
        ));
        let fixtures: [(Session, Arc<dyn ProcessMemory>); 3] = [
            (
                STABLE,
                Arc::new(crate::SliceMemory(crate::stable_fixture::bytes())),
            ),
            (LAZER, lazer.clone()),
            (ZEROS, Arc::new(crate::SliceMemory(vec![0; 64 * 1024]))),
        ];
        for ((pid, _, bitness), memory) in &fixtures {
            host.hook(*pid, Arc::clone(memory), process_bitness(*bitness));
        }
        let loaded = host.package(
            "bench.x1.plan",
            &fixtures.each_ref().map(|((_, name, _), _)| *name),
        );
        let (instance, _) = host.start(&loaded);
        for ((pid, name, bitness), _) in &fixtures {
            host.offer(*pid, name, *bitness);
            instance.wait_attached(*pid);
        }
        for (case, expected) in expected {
            let mut setup = Fields::new()
                .with("command", "x1-setup")
                .with("case", case.name());
            match case {
                Case::StablePrecise | Case::StableRegularFields => setup.set("pid", STABLE.0),
                Case::LazerPrecise => {
                    setup.set("pid", LAZER.0);
                    setup.set("game_base", format_args!("{:x}", lazer.game_base));
                    setup.set("player", format_args!("{:x}", lazer.player));
                }
                Case::LazerRegularFields => setup.set("pid", ZEROS.0),
            }
            let digest = instance.command(&setup);
            assert_eq!(
                digest.text("digest"),
                expected,
                "plan_memory/{}_process decodes differently from its _plan case",
                case.name()
            );
            let census = instance.command(
                &Fields::new()
                    .with("command", "x1-census")
                    .with("case", case.name())
                    .with("captures", CENSUS_CAPTURES),
            );
            let prefix = format!("plan_memory_{}_process", case.name());
            for count in [
                "submissions",
                "stages",
                "reads",
                "read_bytes",
                "wire_frames",
                "control_frames",
                "submit_frames",
                "result_frames",
                "scans",
                "other_sends",
            ] {
                println!(
                    "METRIC {prefix}_{count}_min={}",
                    census.number(&format!("{count}_min"))
                );
                println!(
                    "METRIC {prefix}_{count}_max={}",
                    census.number(&format!("{count}_max"))
                );
                println!(
                    "METRIC {prefix}_{count}_mean={:.2}",
                    census.number(&format!("{count}_total")) as f64 / CENSUS_CAPTURES as f64
                );
            }
            assert_eq!(
                census.number("submit_frames_total"),
                census.number("submissions_total"),
                "{prefix}: every counted plan must be one plan frame on the ring"
            );
            assert!(
                census.number("submissions_total") > 0,
                "{prefix}: the capture submitted nothing"
            );
        }
        Self {
            instance,
            _lazer: lazer,
        }
    }

    /// One Criterion batch: `iterations` captures timed in the plugin host.
    pub fn batch(&self, case: Case, iterations: u64) -> Duration {
        let reply = self.instance.command(
            &Fields::new()
                .with("command", "x1-batch")
                .with("case", case.name())
                .with("iterations", iterations),
        );
        Duration::from_nanos(reply.number("elapsed_ns"))
    }
}
