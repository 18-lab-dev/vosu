//! Real worker-thread scheduling with the same offline reader/transition harness
//! as `vosu-osu-state`'s own cadence tests (`crates/vosu-osu-state/src/tests/worker.rs`),
//! now exercising the real `V2SubscriptionHub`/`ApiError` from the root crate.
//!
//! This benchmark cannot live in `vosu-osu-state`: `V2SubscriptionHub` and
//! `ApiError` are root-crate types, and `vosu` already depends on
//! `vosu-osu-state` (M0 split, implementation-plan.md section 4), so the
//! reverse dependency would cycle. Rather than duplicate the reader/transition
//! fixtures the in-crate cadence tests already define privately, it reuses
//! them through `vosu-osu-state`'s `test-harness`-gated `test_fixtures`
//! module; the crate-level `#![cfg]` below compiles this whole integration
//! test out (to zero tests) unless the root crate's `test-harness` feature
//! is active and the target is Linux (the only OS with the CPU-time clock
//! this benchmark samples), exactly as the original in-crate test required.
//!
//! It is a `tests/` integration test rather than a `src/server/v2_subscription`
//! unit test so it carries none of its own line-count cost against
//! `scripts/file-length-baseline.txt`'s entry for that already-at-limit file.
#![cfg(all(feature = "test-harness", target_os = "linux"))]

use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use vosu_proptest::bounded;

use tokio::sync::watch;
use tokio::time::sleep;

use vosu::api::ApiError;
use vosu::server::v2_subscription::{
    V2SubscriptionHub, V2SubscriptionProvider, V2SubscriptionSnapshot,
};
use vosu_osu_model::enums::ClientType;
use vosu_osu_reader::memory::ReaderSynchronization;
use vosu_osu_state::test_fixtures::{
    DeliveryRecorder, FakePrecise, RecordingTransition, TimedRegular, context, lock,
};
use vosu_osu_state::{
    PollIntervalUpdates, ReaderSettings, RegularCapturePlan, SystemWorkerClock,
    spawn_state_worker_with_clock,
};

/// CPU is this process's consumed CPU, not wall-clock wait.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "five-second CPU/wakeup samples; run alone in a release test binary"]
async fn subscription_cadence_cpu_and_wakeups() {
    #[derive(Clone)]
    struct FixtureDocument(watch::Receiver<u64>);
    impl V2SubscriptionSnapshot for FixtureDocument {
        fn revision(&self) -> u64 {
            *self.0.borrow()
        }
        fn document(&self, _pointers: &[&[String]]) -> Result<Option<serde_json::Value>, ApiError> {
            Ok(Some(serde_json::json!({
                "client": "lazer", "settings": {"mode": {"number": 0}},
                "folders": {"skin": null}, "beatmap": {"checksum": "fixture"}, "keys": {"all": []}
            })))
        }
    }
    impl V2SubscriptionProvider for FixtureDocument {
        fn subscribe(&self) -> watch::Receiver<u64> {
            self.0.clone()
        }
        fn capture(&self) -> Arc<dyn V2SubscriptionSnapshot> {
            Arc::new(self.clone())
        }
    }
    fn cpu_seconds() -> f64 {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        assert_eq!(
            // SAFETY: time is valid writable storage and the process CPU clock is
            // independent of any user's application or device.
            unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut time) },
            0
        );
        time.tv_sec as f64 + time.tv_nsec as f64 / 1_000_000_000.0
    }
    for (name, paths) in [
        ("none", vec![]),
        (
            "slow",
            vec![
                "/settings/mode",
                "/client",
                "/folders/skin",
                "/beatmap/checksum",
            ],
        ),
        ("precise-play", vec!["/keys"]),
        ("whole-document", vec![""]),
    ] {
        let captures = Arc::new(Mutex::new(Vec::new()));
        let precise = Arc::new(Mutex::new(Vec::new()));
        let (_events, events) = watch::channel(1);
        let hub =
            V2SubscriptionHub::new(Arc::new(FixtureDocument(events)), Duration::from_millis(1));
        let runner = tokio::spawn({
            let hub = hub.clone();
            async move { bounded!(hub.run()) }
        });
        let connection = if paths.is_empty() {
            None
        } else {
            let (connection, mut receiver) = hub.connect();
            connection
                .subscribe(paths.into_iter().map(str::to_owned).collect())
                .unwrap();
            bounded!(receiver.recv()).expect("initial subscription frame");
            connection.write_completed();
            Some((connection, receiver))
        };
        let (_config, config) = watch::channel(ReaderSettings::default());
        let intervals = PollIntervalUpdates::Config {
            config,
            demand: hub.demand(),
        };
        let worker = spawn_state_worker_with_clock(
            TimedRegular {
                captures: Arc::clone(&captures),
            },
            FakePrecise {
                captures: Arc::clone(&precise),
                corrupt_first_mode: false,
            },
            RecordingTransition {
                context: context(ClientType::Lazer),
                regular: Arc::new(AtomicUsize::new(0)),
                precise_sequences: Arc::new(Mutex::new(Vec::new())),
            },
            Arc::new(DeliveryRecorder(Arc::new(Mutex::new(Vec::new())))),
            context(ClientType::Lazer),
            RegularCapturePlan::default(),
            intervals,
            Arc::new(Mutex::new(ReaderSynchronization::default())),
            Arc::new(SystemWorkerClock::default()),
        );
        bounded!(sleep(Duration::from_millis(250)));
        let before = (lock(&captures).len(), lock(&precise).len());
        let cpu_start = cpu_seconds();
        let start = std::time::Instant::now();
        bounded!(sleep(Duration::from_secs(5)));
        let elapsed = start.elapsed().as_secs_f64();
        let cpu = cpu_seconds() - cpu_start;
        let after = (lock(&captures).len(), lock(&precise).len());
        bounded!(worker.shutdown()).unwrap();
        drop(connection);
        hub.shutdown();
        bounded!(runner).unwrap();
        println!(
            "cadence={name} seconds={elapsed:.6} regular_hz={:.3} precise_hz={:.3} cpu_seconds={cpu:.6} cpu_percent={:.3}",
            (after.0 - before.0) as f64 / elapsed,
            (after.1 - before.1) as f64 / elapsed,
            cpu / elapsed * 100.0
        );
    }
}
