//! Read-only benchmark of production native capture adapters, not publication.
use serde::Serialize;
use std::{
    error::Error,
    hint::black_box,
    io,
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio_util::sync::CancellationToken;
use vosu::{
    calculation::{default_calculated_mods, state_backend::LiveStateCalculationBackend},
    memory::{
        LazerMemory, LazerPattern, ReaderSynchronization, StableMemory, StablePatterns,
        SynchronizedMemoryReader,
        lazer_loader::{GenerationFuture, LazerOffsetLoader, OffsetGenerator},
        lazer_version,
        snapshot::{RawPreciseSnapshot, RawRegularSnapshot, RegularCaptureContext},
    },
    model::enums::{ClientType, GameState},
    process::{ProcessHandle, ReadStatus},
    state::{
        PreciseSnapshotReader, ReadContext, RegularCapturePlan, RegularCaptureRequest,
        RegularSnapshotReader, SnapshotTransition, SourcePreciseReader, SourceRegularReader,
        StateBundle,
    },
};
type Result<T, E = Box<dyn Error>> = std::result::Result<T, E>;
fn fail(message: impl Into<String>) -> Box<dyn Error> {
    io::Error::other(message.into()).into()
}
struct Args {
    pid: u32,
    client: ClientType,
    warmup: usize,
    cycles: usize,
}
impl Args {
    fn parse(mut values: impl Iterator<Item = String>) -> Result<Self> {
        let (mut pid, mut client, mut warmup, mut cycles) = (None, None, 100, 1000);
        while let Some(flag) = values.next() {
            let value = values
                .next()
                .ok_or_else(|| fail(format!("missing value for {flag}")))?;
            match flag.as_str() {
                "--pid" => pid = Some(value.parse::<u32>()?),
                "--client" => {
                    client = Some(match value.as_str() {
                        "lazer" => ClientType::Lazer,
                        "stable" => ClientType::Stable,
                        _ => return Err(fail("client must be lazer or stable")),
                    })
                }
                "--warmup" => warmup = value.parse()?,
                "--cycles" => cycles = value.parse()?,
                "--samples" if value == "9" => {}
                _ => {
                    return Err(fail(format!(
                        "unknown option or unsupported sample count: {flag} {value}"
                    )));
                }
            }
        }
        if cycles == 0 {
            return Err(fail("cycles must be positive"));
        }
        Ok(Self {
            pid: pid
                .filter(|pid| *pid != 0)
                .ok_or_else(|| fail("--pid must be positive"))?,
            client: client.ok_or_else(|| fail("--client is required"))?,
            warmup,
            cycles,
        })
    }
}
struct CacheOnly;
impl OffsetGenerator for CacheOnly {
    fn generate<'a>(
        &'a self,
        _: u32,
        _: &'a str,
        _: &'a CancellationToken,
    ) -> GenerationFuture<'a> {
        Box::pin(async {
            Err(io::Error::other(
                "run the release daemon once against this client to populate the offsets cache",
            ))
        })
    }
}
#[derive(Serialize)]
struct Measurement {
    warmup: usize,
    samples: usize,
    cycles_per_sample: usize,
    median_ns: f64,
    min_ns: f64,
    max_ns: f64,
    reads_per_cycle: Option<f64>,
    bytes_per_cycle: Option<f64>,
    errors: usize,
}
fn measure(args: &Args, mut cycle: impl FnMut() -> bool) -> Measurement {
    let mut errors = 0;
    for _ in 0..args.warmup {
        errors += usize::from(cycle());
    }
    let mut times = [0.0_f64; 9];
    for time in &mut times {
        let start = Instant::now();
        for _ in 0..args.cycles {
            errors += usize::from(black_box(cycle()));
        }
        *time = start.elapsed().as_nanos() as f64 / args.cycles as f64;
    }
    times.sort_by(f64::total_cmp);
    Measurement {
        warmup: args.warmup,
        samples: 9,
        cycles_per_sample: args.cycles,
        median_ns: times[4],
        min_ns: times[0],
        max_ns: times[8],
        reads_per_cycle: None,
        bytes_per_cycle: None,
        errors,
    }
}
fn regular_failed(s: &RawRegularSnapshot) -> bool {
    s.global().is_err()
        || [
            s.menu().map(std::result::Result::is_err),
            s.settings().map(std::result::Result::is_err),
            s.user().map(std::result::Result::is_err),
            s.mp3_length().map(std::result::Result::is_err),
            s.result_screen().map(std::result::Result::is_err),
            s.gameplay().map(std::result::Result::is_err),
            s.audio_velocity_base().map(std::result::Result::is_err),
            s.leaderboard().map(std::result::Result::is_err),
            s.ranked_play().map(std::result::Result::is_err),
            s.tourney().map(std::result::Result::is_err),
            s.tourney_chat().map(std::result::Result::is_err),
            s.tourney_user().map(std::result::Result::is_err),
            s.lazer_spectating().map(std::result::Result::is_err),
            s.room().map(std::result::Result::is_err),
        ]
        .into_iter()
        .flatten()
        .any(|failed| failed)
}
fn precise_failed(s: &RawPreciseSnapshot) -> bool {
    s.global().is_err()
        || s.key_overlay().is_some_and(std::result::Result::is_err)
        || s.hit_errors().is_some_and(std::result::Result::is_err)
}
#[cfg(windows)]
fn open(pid: u32) -> Result<ProcessHandle> {
    Ok(vosu::process::open_process(pid)?)
}
#[cfg(target_os = "linux")]
fn open(pid: u32) -> Result<ProcessHandle> {
    Ok(ProcessHandle::new(
        Arc::new(vosu::process::open_process(pid)?),
        vosu::process::process_bitness(pid)?,
    ))
}
fn run<R: SynchronizedMemoryReader + 'static>(
    args: &Args,
    regular_memory: R,
    precise_memory: R,
    version: String,
    start: Instant,
) -> Result<()> {
    let synchronization = Arc::new(Mutex::new(ReaderSynchronization::default()));
    let mut regular =
        SourceRegularReader::new(args.client, regular_memory, synchronization.clone());
    let (_keys_tx, keys_rx) = tokio::sync::watch::channel(true);
    let mut precise = SourcePreciseReader::new(precise_memory, keys_rx);
    let mut context = ReadContext {
        source: args.client,
        instance_generation: 1,
        reset_epoch: 0,
        status: GameState::Menu.value() as i64,
        play_time: 0.0,
        mode: 0,
        mods: default_calculated_mods(),
        committed_hit_error_index: 0,
        accepted_precise_sequence: 0,
        latest_precise_global_sequence: 0,
    };
    let mut request = RegularCaptureRequest {
        context: RegularCaptureContext::new(args.client, 1, 0, 1),
        plan: RegularCapturePlan::default(),
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut bundle = StateBundle::new_with_root(
        args.client,
        1,
        lazer_version::process_executable_root(args.pid)?,
        Arc::new(LiveStateCalculationBackend::new()),
    );
    bundle.set_calculate_pp(false);
    // Let the production reducer select the steady-state capture plan before timing.
    // The first capture uses the default menu plan, so a stable client already in
    // gameplay reads the menu-only audio velocity pointer and fails the way the
    // daemon's first tick would; only the plan-selected final capture must be clean.
    for sequence in 1..=3 {
        request.context = RegularCaptureContext::new(args.client, 1, context.reset_epoch, sequence);
        let snapshot = regular.capture_regular(&request)?;
        if sequence == 3 && regular_failed(&snapshot) {
            return Err(fail(format!(
                "setup capture contains memory errors: {snapshot:?}"
            )));
        }
        if !matches!(snapshot.global(), Ok(ReadStatus::Ready(_))) {
            return Err(fail("client global state is not ready"));
        }
        let transition = rt.block_on(bundle.apply_regular(snapshot))?;
        context = transition.context;
        request.plan = transition.next_capture;
    }
    let state_start = context.status;
    let setup_ns = start.elapsed().as_nanos();
    let regular_result = measure(args, || match regular.capture_regular(&request) {
        Ok(snapshot) => {
            let failed = regular_failed(&snapshot);
            // Carry the same status/time inputs the worker refreshes each tick.
            if let Ok(ReadStatus::Ready(global)) = snapshot.global() {
                request.plan.status = global.status;
                request.plan.gameplay_time = global.play_time;
                context.status = global.status;
                context.play_time = global.play_time;
            }
            black_box(snapshot);
            failed
        }
        Err(_) => true,
    });
    let sync = *synchronization
        .lock()
        .map_err(|_| fail("synchronization poisoned"))?;
    let mut sequence = 0;
    let precise_result = measure(args, || {
        sequence += 1;
        match precise.capture_precise(&context, sequence, sync) {
            Ok(snapshot) => {
                let failed = precise_failed(&snapshot);
                if let Ok(ReadStatus::Ready(global)) = snapshot.global() {
                    context.status = global.status;
                    context.play_time = global.time;
                }
                if !failed {
                    context.committed_hit_error_index = snapshot.ending_index();
                }
                black_box(snapshot);
                failed
            }
            Err(_) => true,
        }
    });
    let end = regular.capture_regular(&request)?;
    let state_end = match end.global() {
        Ok(ReadStatus::Ready(global)) => global.status,
        _ => return Err(fail("end state unavailable")),
    };
    println!("Production capture adapters; setup excluded; native read counters unavailable.");
    println!(
        "{}",
        serde_json::json!({"product":"vosu", "client": if args.client == ClientType::Lazer {"lazer"} else {"stable"}, "pid":args.pid, "client_version":version, "state":format!("{state_start} -> {state_end}"), "regular":regular_result, "precise":precise_result, "setup_ns":setup_ns})
    );
    Ok(())
}
fn main() {
    if let Err(error) = execute() {
        eprintln!("live_reader_bench: {error}");
        std::process::exit(1);
    }
}
fn execute() -> Result<()> {
    let args = Args::parse(std::env::args().skip(1))?;
    let start = Instant::now();
    let process = open(args.pid)?;
    match args.client {
        ClientType::Stable => {
            if lazer_version::discover_for_process(args.pid).is_ok() {
                return Err(fail(
                    "--client stable does not match the lazer installation",
                ));
            }
            let patterns = match StablePatterns::resolve(&process)? {
                ReadStatus::Ready(value) => value,
                _ => return Err(fail("stable patterns not ready: PID may not be osu!stable")),
            };
            let mut reader = StableMemory::new(process.clone(), patterns);
            let version = match vosu::memory::MemoryReader::settings(&mut reader)? {
                ReadStatus::Ready(settings) => settings
                    .get("client.version")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| "unknown".into()),
                _ => "unknown".into(),
            };
            run(
                &args,
                reader,
                StableMemory::new(process, patterns),
                version,
                start,
            )
        }
        ClientType::Lazer => {
            let version = lazer_version::discover_for_process(args.pid)?;
            let cache = vosu::platform::Paths::system_cache_root()?.join("offsets");
            let asset = vosu::assets::DaemonAssets::get("offsets.json")
                .ok_or_else(|| fail("embedded offsets missing"))?;
            let loader = LazerOffsetLoader::new(
                CacheOnly,
                cache,
                bytes::Bytes::copy_from_slice(&asset.data),
            );
            let path = loader.cache_path(&version)?;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let offsets = rt
                .block_on(loader.resolve(args.pid, &version, &CancellationToken::new()))
                .map_err(|error| fail(format!("{error}; checked {}", path.display())))?;
            let pattern = LazerPattern::source_default();
            let mut regular = LazerMemory::new(process.clone(), offsets.clone(), pattern.clone());
            if !regular.resolve_game_base()? {
                return Err(fail("lazer game base not ready"));
            }
            run(
                &args,
                regular,
                LazerMemory::new(process, offsets, pattern),
                version,
                start,
            )
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn args(extra: &[&str]) -> Result<Args> {
        Args::parse(
            ["--pid", "123", "--client", "lazer"]
                .into_iter()
                .chain(extra.iter().copied())
                .map(str::to_owned),
        )
    }
    #[test]
    fn validates_arguments() {
        assert!(args(&["--cycles", "0"]).is_err());
        assert!(args(&["--samples", "8"]).is_err());
        assert!(args(&["--unknown", "1"]).is_err());
        assert_eq!(args(&[]).unwrap().cycles, 1000);
    }
    #[test]
    fn retains_failed_cycles() {
        let args = args(&["--warmup", "2", "--cycles", "3"]).unwrap();
        let measured = measure(&args, || true);
        assert_eq!(measured.errors, 29);
        assert_eq!(measured.samples, 9);
        assert!(measured.median_ns >= measured.min_ns && measured.median_ns <= measured.max_ns);
    }
}
