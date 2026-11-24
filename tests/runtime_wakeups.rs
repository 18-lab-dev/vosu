//! Real stable decoding/reduction/B1 publication without process or socket noise.
#![cfg(feature = "test-harness")]
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use vosu_osu::bridge_b1::{B1Event, B1Inputs, B1Publisher, B1SessionKey};
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
use vosu_sys::process::{MemoryError, MemoryRegion, ProcessBitness, ProcessMemory, ReadRequest};

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
struct Sink {
    inputs: watch::Sender<B1Inputs>,
    latest: Mutex<Option<B1Event>>,
}
impl vosu_surface::LegacySink<B1Event, B1Inputs> for Sink {
    fn send(&self, event: B1Event) {
        *self.latest.lock().unwrap() = Some(event);
    }
    fn inputs(&self) -> watch::Receiver<B1Inputs> {
        self.inputs.subscribe()
    }
}
struct Publisher {
    bridge: B1Publisher,
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
        self.bridge.publish(snapshot, delivery)?;
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
                bridge: B1Publisher::new(
                    B1SessionKey {
                        pid: target.pid,
                        generation: 1,
                    },
                    Arc::new(Sink {
                        inputs: watch::channel(B1Inputs::default()).0,
                        latest: Mutex::new(None),
                    }),
                ),
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
            let first_polls = worker.reducer_poll_count();
            let (last_count, last_wakes, last_time) = receiver
                .recv_timeout(Duration::from_secs(10))
                .expect("sample publication deadline");
            let last_polls = worker.reducer_poll_count();
            runtime
                .block_on(async {
                    tokio::time::timeout(Duration::from_secs(5), worker.shutdown()).await
                })
                .expect("shutdown deadline")
                .unwrap();
            let captures = last_count - first_count;
            let wakeups = last_wakes - first_wakes;
            let polls = last_polls - first_polls;
            eprintln!(
                "workers={workers} precise={precise_ms}ms captures={captures} runtime_wakes={wakeups} reducer_polls={polls} seconds={:.3} wakes/capture={:.3}",
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
    bridge: B1Publisher,
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
        self.bridge.publish(snapshot, delivery)?;
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
                bridge: B1Publisher::new(
                    B1SessionKey {
                        pid: target.pid,
                        generation: 1,
                    },
                    Arc::new(Sink {
                        inputs: watch::channel(B1Inputs::default()).0,
                        latest: Mutex::new(None),
                    }),
                ),
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
