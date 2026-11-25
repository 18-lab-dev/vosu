//! Real stable decoding/reduction/ABI-bus publication without live process noise.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};
use tokio::sync::watch;
#[path = "adapter_fixture/mod.rs"]
mod adapter_fixture;
use vosu_osu::capture::{CaptureTarget, finish_state_worker};
use vosu_osu_model::{enums::ClientType, launch::InstanceSettings};
use vosu_osu_reader::{
    memory::{ReaderSynchronization, StableMemory},
    process::ProcessHandle,
};
use vosu_osu_state::{
    Delivery, PollIntervals, ReaderSettings, SnapshotPublisher, StateBundleSnapshot,
    StateWorkerError, SubscriptionDemand, SubscriptionPolling,
};
use vosu_plugin_api::memory::{
    MemoryError, MemoryRegion, ProcessBitness, ProcessMemory, ReadRequest,
};

#[path = "../benches/support/stable_fixture.rs"]
mod stable_fixture;

struct Memory {
    bytes: Vec<u8>,
    last_precise: Arc<Mutex<Option<Instant>>>,
}
impl ProcessMemory for Memory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let bytes = address
            .checked_add(dst.len())
            .and_then(|end| self.bytes.get(address..end))
            .ok_or_else(|| MemoryError::Backend {
                operation: "fixture read",
                message: "outside populated fixture".into(),
            })?;
        dst.copy_from_slice(bytes);
        if std::thread::current().name() == Some("vosu-precise-reader") {
            *self.last_precise.lock().unwrap() = Some(Instant::now());
        }
        Ok(())
    }
    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        for read in reads {
            self.read_exact(read.address, read.dst)?;
        }
        Ok(())
    }
    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(vec![MemoryRegion {
            address: 0,
            size: self.bytes.len(),
        }])
    }
}
struct Publisher {
    bus: Arc<vosu_osu::fixtures::Publisher>,
    publications: AtomicUsize,
    samples: mpsc::SyncSender<(usize, usize, Instant)>,
    wakes: Arc<AtomicUsize>,
    end: usize,
}
impl SnapshotPublisher<Arc<StateBundleSnapshot>> for Publisher {
    fn publish(
        &self,
        snapshot: Arc<StateBundleSnapshot>,
        delivery: Delivery,
    ) -> Result<(), StateWorkerError> {
        self.bus.publish(snapshot, delivery)?;
        let count = self.publications.fetch_add(1, Ordering::Relaxed) + 1;
        if count == 20 || count == self.end {
            self.samples
                .try_send((count, self.wakes.load(Ordering::Relaxed), Instant::now()))
                .expect("bounded milestone channel");
        }
        Ok(())
    }
}

#[test]
fn stable_capture_has_no_periodic_runtime_wakes() {
    for workers in [2, 1] {
        for precise_ms in [10, 100] {
            let mut fixture = adapter_fixture::Fixture::new(ClientType::Stable);
            let wakes = Arc::new(AtomicUsize::new(0));
            let unpark_wakes = Arc::clone(&wakes);
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(workers)
                .enable_all()
                .on_thread_unpark(move || {
                    unpark_wakes.fetch_add(1, Ordering::Relaxed);
                })
                .build()
                .unwrap();
            let mut bytes = stable_fixture::bytes();
            let original = ProcessHandle::new(
                Arc::new(Memory {
                    bytes: bytes.clone(),
                    last_precise: Arc::default(),
                }),
                ProcessBitness::X86,
            );
            stable_fixture::assert_capture(&mut StableMemory::new(
                original,
                stable_fixture::patterns(),
            ));
            // Preserve the populated graph but put the client in the idle menu.
            let status = stable_fixture::patterns().status_ptr;
            let status = u32::from_le_bytes(bytes[status..status + 4].try_into().unwrap()) as usize;
            bytes[status..status + 4].copy_from_slice(&0u32.to_le_bytes());
            let process = ProcessHandle::new(
                Arc::new(Memory {
                    bytes,
                    last_precise: Arc::default(),
                }),
                ProcessBitness::X86,
            );
            let memory = || StableMemory::new(process.clone(), stable_fixture::patterns());
            let target = CaptureTarget {
                pid: 910_600_123,
                generation: 1,
                client: ClientType::Stable,
                process: process.clone(),
                root: Default::default(),
                settings: InstanceSettings {
                    tournament_spectator: false,
                    tournament_ipc_id: None,
                    devserver_endpoint: None,
                },
                is_tournament_manager: false,
                lazer_offsets: None,
            };
            let (_settings, settings) = watch::channel(ReaderSettings {
                calculate_pp: false,
                ..ReaderSettings::default()
            });
            let (_intervals, intervals) = watch::channel(PollIntervals {
                // The slow-precise case isolates precise wakes from asynchronous
                // regular calculation completions; the 10ms case keeps live settings.
                regular: if precise_ms == 10 {
                    Duration::from_millis(150)
                } else {
                    Duration::from_secs(60)
                },
                precise: Duration::from_millis(precise_ms),
                subscription: Some(SubscriptionPolling {
                    interval: Duration::from_millis(precise_ms),
                    demand: SubscriptionDemand {
                        precise: true,
                        ..SubscriptionDemand::default()
                    },
                }),
            });
            let (samples, receiver) = mpsc::sync_channel(2);
            let publisher = Arc::new(Publisher {
                bus: fixture.attach(target.settings.clone(), false, true),
                publications: AtomicUsize::new(0),
                samples,
                wakes,
                end: 70,
            });
            let worker = {
                let _enter = runtime.enter();
                finish_state_worker(
                    &target,
                    settings,
                    intervals.into(),
                    publisher,
                    Arc::new(Mutex::new(ReaderSynchronization::default())),
                    memory(),
                    memory(),
                )
                .unwrap()
            };
            let (first_count, first_wakes, first_time) = receiver
                .recv_timeout(Duration::from_secs(10))
                .expect("warmup publication deadline");
            #[cfg(feature = "test-harness")]
            let first_polls = worker.reducer_poll_count();
            let (last_count, last_wakes, last_time) = receiver
                .recv_timeout(Duration::from_secs(10))
                .expect("sample publication deadline");
            #[cfg(feature = "test-harness")]
            let last_polls = worker.reducer_poll_count();
            runtime
                .block_on(async {
                    tokio::time::timeout(Duration::from_secs(5), worker.shutdown()).await
                })
                .expect("shutdown deadline")
                .unwrap();
            let captures = last_count - first_count;
            let wakeups = last_wakes - first_wakes;
            #[cfg(feature = "test-harness")]
            eprintln!("reducer_polls={}", last_polls - first_polls);
            eprintln!(
                "workers={workers} precise={precise_ms}ms captures={captures} runtime_wakes={wakeups} seconds={:.3} wakes/capture={:.3}",
                last_time.duration_since(first_time).as_secs_f64(),
                wakeups as f64 / captures as f64
            );
            if workers == 1 {
                assert!(
                    wakeups <= captures + 8,
                    "one reducer wake per capture, with bounded scheduling margin: {wakeups}/{captures}"
                );
            }
        }
    }
}

struct TimedPublisher {
    bus: Arc<vosu_osu::fixtures::Publisher>,
    last_read: Arc<Mutex<Option<Instant>>>,
    latencies: Mutex<Vec<Duration>>,
    completed: mpsc::SyncSender<()>,
}
impl SnapshotPublisher<Arc<StateBundleSnapshot>> for TimedPublisher {
    fn publish(
        &self,
        snapshot: Arc<StateBundleSnapshot>,
        delivery: Delivery,
    ) -> Result<(), StateWorkerError> {
        self.bus.publish(snapshot, delivery)?;
        if delivery == Delivery::Precise {
            let start = self.last_read.lock().unwrap().expect("precise memory read");
            let mut latencies = self.latencies.lock().unwrap();
            if latencies.len() < 120 {
                latencies.push(start.elapsed());
                if latencies.len() == 120 {
                    self.completed
                        .try_send(())
                        .expect("one bounded completion per session");
                }
            }
        }
        Ok(())
    }
}

#[test]
#[ignore = "manual paired scheduler latency measurement"]
fn tournament_eight_session_publication_latency() {
    let mut results = Vec::new();
    for workers in [2, 1] {
        let mut fixture = adapter_fixture::Fixture::new(ClientType::Stable);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            .enable_all()
            .build()
            .unwrap();
        let mut sessions = Vec::new();
        let (completed, completion) = mpsc::sync_channel(8);
        for index in 0..8 {
            let last_read = Arc::default();
            let process = ProcessHandle::new(
                Arc::new(Memory {
                    bytes: stable_fixture::bytes(),
                    last_precise: Arc::clone(&last_read),
                }),
                ProcessBitness::X86,
            );
            let memory = || StableMemory::new(process.clone(), stable_fixture::patterns());
            let target = CaptureTarget {
                pid: 910_600_140 + index,
                generation: 1,
                client: ClientType::Stable,
                process: process.clone(),
                root: Default::default(),
                settings: InstanceSettings {
                    tournament_spectator: true,
                    tournament_ipc_id: Some(f64::from(index)),
                    devserver_endpoint: None,
                },
                is_tournament_manager: false,
                lazer_offsets: None,
            };
            let (settings_tx, settings) = watch::channel(ReaderSettings::default());
            let (interval_tx, intervals) = watch::channel(PollIntervals {
                regular: Duration::from_millis(150),
                precise: Duration::from_millis(10),
                subscription: Some(SubscriptionPolling {
                    interval: Duration::from_millis(10),
                    demand: SubscriptionDemand {
                        precise: true,
                        ..SubscriptionDemand::default()
                    },
                }),
            });
            let publisher = Arc::new(TimedPublisher {
                bus: fixture.attach(target.settings.clone(), false, index == 0),
                last_read,
                latencies: Mutex::new(Vec::with_capacity(120)),
                completed: completed.clone(),
            });
            let worker = {
                let _enter = runtime.enter();
                finish_state_worker(
                    &target,
                    settings,
                    intervals.into(),
                    publisher.clone(),
                    Arc::new(Mutex::new(ReaderSynchronization::default())),
                    memory(),
                    memory(),
                )
                .unwrap()
            };
            sessions.push((worker, publisher, settings_tx, interval_tx));
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        for _ in 0..8 {
            completion
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("all session publication deadlines");
        }
        let mut p99s = Vec::new();
        for (index, (worker, publisher, _settings, _intervals)) in sessions.into_iter().enumerate()
        {
            runtime
                .block_on(async {
                    tokio::time::timeout(Duration::from_secs(5), worker.shutdown()).await
                })
                .expect("session shutdown deadline")
                .unwrap();
            let mut measured = publisher.latencies.lock().unwrap()[20..].to_vec();
            measured.sort_unstable();
            let p50 = measured[49];
            let p99 = measured[98];
            p99s.push(p99);
            eprintln!(
                "workers={workers} session={index} read-to-publish p50={:.3}us p99={:.3}us",
                p50.as_secs_f64() * 1e6,
                p99.as_secs_f64() * 1e6
            );
        }
        results.push(p99s);
    }
    for (index, (two, one)) in results[0].iter().zip(&results[1]).enumerate() {
        assert!(
            *one <= *two + Duration::from_millis(10),
            "single-worker p99 adds less than one precise tick for session {index}"
        );
    }
}

#[test]
fn api_v1_idle_hub_creates_no_runtime_task() {
    use vosu::server::api_v1::subscriptions::ApiV1SubscriptionHub;
    use vosu_core::api::SessionTarget;
    let id = "test.x1.wakeups";
    let mut fixture = adapter_fixture::Fixture::new(ClientType::Stable);
    let (handle, _) = fixture.h.publisher(id, 992002);
    let source = fixture.h.host.api_state_source();
    let before = fixture.h.runtime.metrics().num_alive_tasks();
    let hub = ApiV1SubscriptionHub::new(source.clone(), Duration::from_millis(16));
    assert_eq!(
        fixture.h.runtime.metrics().num_alive_tasks(),
        before,
        "an idle API hub must not spawn a periodic producer"
    );
    for n in 0..128 {
        adapter_fixture::host::publish(
            id,
            handle,
            &[("/score", vosu_plugin_api::PublishValueRef::I64(1700 + n))],
        );
    }
    assert_eq!(
        source
            .read(
                &SessionTarget::Active,
                &vosu_core::api::ApiPlan::new(&[format!("/{id}/score")]).unwrap()
            )
            .unwrap()
            .revision,
        0
    );
    assert_eq!(
        fixture.h.runtime.metrics().num_alive_tasks(),
        before,
        "unobserved publications must not schedule API delivery work"
    );
    // Positive control: a real transport client must spawn its establishment task.
    let (_connection, _receiver) = {
        let _enter = fixture.h.runtime.enter();
        hub.connect(SessionTarget::Active).unwrap()
    };
    assert!(
        fixture.h.runtime.metrics().num_alive_tasks() > before,
        "task instrumentation must observe a real API connection"
    );
    hub.shutdown();
}

#[path = "../benches/support/adapter/mod.rs"]
#[expect(
    dead_code,
    unused_imports,
    reason = "The wakeup contract uses only the rig's providers and publisher"
)]
mod adapter;

/// Idle production adapter hubs park without a timer: commits with no client
/// wake no runtime thread, so no cadence clock is ever armed. A connected
/// client (positive control) arms the 1 ms cadence and wakes the runtime.
#[test]
fn idle_adapter_hubs_arm_no_timer_and_wake_nothing() {
    use vosu::server::websocket::{Schema, WebSocketHub};
    let rig = adapter::Rig::new();
    let wakes = Arc::new(AtomicUsize::new(0));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .on_thread_unpark({
            let wakes = Arc::clone(&wakes);
            move || {
                wakes.fetch_add(1, Ordering::Relaxed);
            }
        })
        .build()
        .unwrap();
    let hubs = [
        Schema::V1,
        Schema::StreamCompanion,
        Schema::V2,
        Schema::V2Precise,
    ]
    .map(|schema| {
        let (provider, counts) = rig.provider(schema, Duration::from_millis(1));
        let hub = WebSocketHub::new(schema, provider);
        let run = runtime.spawn({
            let hub = hub.clone();
            async move { hub.run().await }
        });
        (hub, run, counts)
    });
    // Each run task subscribes and parks; a window far longer than the
    // cadence would expose any armed timer.
    let window = |duration| {
        let (_keep, never) = mpsc::sync_channel::<()>(1);
        assert!(never.recv_timeout(duration).is_err());
    };
    window(Duration::from_millis(50));
    let before = wakes.load(Ordering::Relaxed);
    for n in 0..128 {
        rig.publish(
            adapter::input::snapshot(1_000.0 + f64::from(n), 7),
            vosu_osu_state::Delivery::All,
        );
    }
    window(Duration::from_millis(100));
    assert_eq!(
        wakes.load(Ordering::Relaxed),
        before,
        "idle hubs neither poll a cadence nor wake on commits"
    );
    for (_, _, counts) in &hubs {
        assert_eq!(
            counts.work(),
            Default::default(),
            "no capture, view or encoding"
        );
    }
    // Positive control: one real client holding a frame arms the cadence.
    let (connection, mut receiver) = {
        let _enter = runtime.enter();
        hubs[2].0.connect()
    };
    runtime
        .block_on(adapter::bounded(receiver.recv()))
        .expect("first frame");
    let armed = wakes.load(Ordering::Relaxed);
    // The 1 ms cadence fires at the OS timer granularity: 116 wakes in 250 ms
    // on Linux, but 15 on native Windows (~15.6 ms default resolution), so the
    // former 100 ms window saw at most ~6 there. 250 ms clears five on both.
    window(Duration::from_millis(250));
    let ticks = wakes.load(Ordering::Relaxed) - armed;
    assert!(
        ticks > 5,
        "instrumentation observes the cadence timer of a real client ({ticks} wakes)"
    );
    drop((connection, receiver));
    for (hub, run, _) in hubs {
        hub.shutdown();
        runtime
            .block_on(adapter::bounded(run))
            .expect("hub run task");
    }
}
