use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use tokio::sync::watch;
use vosu::api::{
    ApiError, ApiRuntimeSettings, CombinedSnapshot, ManagerInstanceSnapshot, ManagerSnapshot,
};
use vosu::calculation::beatmap::{
    BeatmapCache, BeatmapEngine, BeatmapError, BeatmapInput, EngineOutput, EngineRequest, ModSet,
    Ruleset as BeatmapRuleset, ScoringMode,
};
use vosu::calculation::engine::{LiveDifficultyConfig, LiveLazerEngine, LiveScoreState};
use vosu::instance::InstanceSettings;
use vosu::memory::lazer_offsets::LazerOffsets;
use vosu::memory::{LazerMemory, LazerPattern, MemoryReader, StableMemory, StablePatterns};
use vosu::model::enums::ClientType;
use vosu::model::states::{HitErrorArray, KeypressButton};
use vosu::process::{
    MemoryError, MemoryRegion, Pattern, ProcessBitness, ProcessHandle, ProcessMemory, ReadRequest,
};
use vosu::server::v2_subscription::{
    V2SubscriptionHub, V2SubscriptionOutbound, V2SubscriptionProvider, V2SubscriptionSnapshot,
};
use vosu::server::websocket::{
    Filter, Outbound, Schema, StateProvider, StateSnapshot, WebSocketHub,
};
use vosu::state::{PreciseState, RegularState, StateBundleSnapshot};

const MAP: &[u8] = br#"osu file format v14

[General]
Mode:0

[Difficulty]
HPDrainRate:5
CircleSize:4
OverallDifficulty:8
ApproachRate:9
SliderMultiplier:1.4
SliderTickRate:1

[TimingPoints]
0,500,4,2,1,80,1,0

[HitObjects]
64,192,1000,1,0,0:0:0:0:
128,192,1400,1,0,0:0:0:0:
256,192,1800,2,0,B|384:192,1,140
384,192,2400,1,0,0:0:0:0:
448,192,3000,8,0,3500
"#;

#[derive(Clone)]
struct SliceMemory {
    base: usize,
    bytes: Arc<[u8]>,
}

impl ProcessMemory for SliceMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let start = address
            .checked_sub(self.base)
            .ok_or_else(|| memory_error("address before fixture"))?;
        let end = start
            .checked_add(dst.len())
            .ok_or_else(|| memory_error("address overflow"))?;
        let source = self
            .bytes
            .get(start..end)
            .ok_or_else(|| memory_error("address outside fixture"))?;
        dst.copy_from_slice(source);
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
            address: self.base,
            size: self.bytes.len(),
        }])
    }
}

fn memory_error(message: &'static str) -> MemoryError {
    MemoryError::Backend {
        operation: "benchmark fixture",
        message: message.into(),
    }
}

fn process(bytes: Vec<u8>, base: usize, bitness: ProcessBitness) -> ProcessHandle {
    ProcessHandle::new(
        Arc::new(SliceMemory {
            base,
            bytes: bytes.into(),
        }),
        bitness,
    )
}

fn stable_patterns() -> StablePatterns {
    StablePatterns {
        base_addr: 0,
        play_time_addr: 0,
        chat_checker_ptr: 0,
        skin_data_addr: 0,
        settings_class_addr: 0,
        configuration_addr: 0,
        bindings_addr: 0,
        rulesets_addr: 0,
        can_run_slowly_addr: 0,
        status_ptr: 0,
        menu_mods_ptr: 0,
        get_audio_length_ptr: 0,
        user_profile_ptr: 0,
        raw_login_status_ptr: 0,
        spectating_user_ptr: 0,
        game_time_ptr: 0,
    }
}

fn regular_state() -> RegularState {
    RegularState {
        global: Default::default(),
        settings: Default::default(),
        beatmap: Default::default(),
        menu: Default::default(),
        bass_density: Default::default(),
        gameplay: Default::default(),
        result_screen: Default::default(),
        tourney: Default::default(),
        user: Default::default(),
        lazer_multi_spectating: Default::default(),
        ranked_play: Default::default(),
        room: Default::default(),
    }
}

fn precise_state() -> PreciseState {
    PreciseState {
        status: 0,
        play_time: 0.0,
        mode: 3,
        key_overlay: Arc::new(Vec::new()),
        keypresses: Arc::new(
            (1..=18)
                .map(|index| KeypressButton {
                    name: Arc::from(format!("K{index}")),
                    is_pressed: index % 2 == 0,
                    count: index,
                    pressed_at: (index % 2 == 0).then_some(1_000.0),
                    released_at: None,
                })
                .collect(),
        ),
        hit_errors: Arc::new(HitErrorArray::default()),
        total_hit_errors: 0.0,
        committed_hit_error_index: 0,
        unstable_rate: 0.0,
    }
}

fn api_snapshot() -> CombinedSnapshot {
    let state = Arc::new(StateBundleSnapshot {
        source: ClientType::Stable,
        generation: 1,
        reset_epoch: 0,
        regular: Arc::new(regular_state()),
        precise: Arc::new(precise_state()),
    });
    let instance = ManagerInstanceSnapshot::new(
        7,
        InstanceSettings {
            tournament_spectator: false,
            tournament_ipc_id: None,
            devserver_endpoint: None,
        },
        false,
        state,
    );
    CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::new(
        1,
        Some(7),
        true,
        ApiRuntimeSettings::default(),
        [instance],
    )))
}

fn bench_batch_scan(c: &mut Criterion) {
    let base = 0x1000;
    let mut bytes = vec![0_u8; 2 * 1024 * 1024];
    for (offset, marker) in [(128, 0x11), (1_048_570, 0x22), (1_700_000, 0x33)] {
        bytes[offset..offset + 5].copy_from_slice(&[0xDE, 0xAD, marker, 0xBE, 0xEF]);
    }
    let handle = process(bytes, base, ProcessBitness::X64);
    let patterns = [
        Pattern::parse("DE AD ?? BE EF", false).unwrap(),
        Pattern::parse("DE AD 22 BE EF", false).unwrap(),
        Pattern::parse("DE AD ?? BE EF", true).unwrap(),
    ];
    let mut group = c.benchmark_group("batch_scan");
    group.throughput(Throughput::Bytes(2 * 1024 * 1024));
    group.bench_function("three_patterns_2mib", |b| {
        b.iter(|| black_box(handle.scan_batch(black_box(&patterns)).unwrap()));
    });
    group.finish();
}

fn bench_snapshot_reads(c: &mut Criterion) {
    let fixture = vec![0_u8; 64 * 1024];
    let mut stable = StableMemory::new(
        process(fixture.clone(), 0, ProcessBitness::X86),
        stable_patterns(),
    );
    let offsets = Arc::new(LazerOffsets::embedded().expect("embedded offsets"));
    let mut lazer = LazerMemory::new(
        process(fixture, 0, ProcessBitness::X64),
        offsets,
        LazerPattern::source_default(),
    );
    lazer.set_resolved_state(0x1000, 0x2000);
    let mut group = c.benchmark_group("raw_snapshot_reads");
    group.bench_function("stable_regular_fields", |b| {
        b.iter(|| {
            let _ = black_box(stable.global());
            let _ = black_box(stable.menu(""));
            let _ = black_box(stable.gameplay(0.0, None));
        })
    });
    group.bench_function("lazer_regular_fields", |b| {
        b.iter(|| {
            let _ = black_box(lazer.global());
            let _ = black_box(lazer.menu(""));
            let _ = black_box(lazer.gameplay(0.0, None));
        })
    });
    group.finish();
}

struct CacheEngine {
    calculations: Arc<AtomicUsize>,
}

impl BeatmapEngine for CacheEngine {
    fn calculate(&self, _request: EngineRequest<'_>) -> Result<EngineOutput, BeatmapError> {
        self.calculations.fetch_add(1, Ordering::Relaxed);
        Ok(EngineOutput::default())
    }
}

fn bench_map_calculation(c: &mut Criterion) {
    let engine = LiveLazerEngine;
    let parsed = engine.parse(MAP).expect("benchmark map parses");
    let config = LiveDifficultyConfig::default();
    let score = LiveScoreState {
        accuracy: 1.0,
        max_combo: 5,
        n300: 5,
        ..Default::default()
    };
    let calculations = Arc::new(AtomicUsize::new(0));
    let mut cache = BeatmapCache::new(CacheEngine {
        calculations: Arc::clone(&calculations),
    });
    let map_text = std::str::from_utf8(MAP).expect("benchmark map is UTF-8");
    let mods = ModSet::new("nomod", Vec::new(), 1.0).unwrap();
    let input = || BeatmapInput::Content {
        content: map_text,
        identity_path: None,
    };
    let cached = cache
        .load(input(), BeatmapRuleset::Osu, &mods, ScoringMode::Lazer)
        .unwrap();
    assert_eq!(cache.parse_count(), 1);
    assert_eq!(calculations.load(Ordering::Relaxed), 1);
    let mut group = c.benchmark_group("map_calculation");
    group.throughput(Throughput::Elements(5));
    group.bench_function("parse", |b| {
        b.iter(|| black_box(engine.parse(black_box(MAP)).unwrap()))
    });
    group.bench_function("full_difficulty_performance", |b| {
        b.iter(|| {
            black_box(engine.difficulty(&parsed, &config).unwrap());
            black_box(engine.performance(&parsed, &config, score).unwrap());
        })
    });
    group.bench_function("gradual_all_objects", |b| {
        b.iter(|| {
            let mut gradual = engine.gradual_difficulty(&parsed, &config).unwrap();
            while gradual.advance() {
                black_box(gradual.current());
            }
        })
    });
    group.bench_function("unchanged_cache_key", |b| {
        b.iter(|| {
            let reused = cache
                .load(input(), BeatmapRuleset::Osu, &mods, ScoringMode::Lazer)
                .unwrap();
            assert!(Arc::ptr_eq(&cached, &reused));
            black_box(reused)
        })
    });
    group.finish();
}

fn bench_serializers(c: &mut Criterion) {
    let mut cold = c.benchmark_group("serializers_cold");
    cold.bench_function("v1", |b| {
        b.iter_batched(
            api_snapshot,
            |snapshot| black_box(vosu::api::v1::encode(&snapshot).unwrap()),
            BatchSize::SmallInput,
        )
    });
    cold.bench_function("v2", |b| {
        b.iter_batched(
            api_snapshot,
            |snapshot| black_box(vosu::api::v2::encode(&snapshot).unwrap()),
            BatchSize::SmallInput,
        )
    });
    cold.bench_function("v2_precise", |b| {
        b.iter_batched(
            api_snapshot,
            |snapshot| black_box(vosu::api::precise::encode(&snapshot).unwrap()),
            BatchSize::SmallInput,
        )
    });
    cold.bench_function("stream_companion", |b| {
        b.iter_batched(
            api_snapshot,
            |snapshot| black_box(vosu::api::sc::encode(&snapshot).unwrap()),
            BatchSize::SmallInput,
        )
    });
    cold.finish();

    let cached = api_snapshot();
    let _ = vosu::api::v1::encode(&cached).unwrap();
    let _ = vosu::api::v2::encode(&cached).unwrap();
    let _ = vosu::api::precise::encode(&cached).unwrap();
    let _ = vosu::api::sc::encode(&cached).unwrap();
    let mut cache = c.benchmark_group("serialization_cache");
    cache.bench_function("v1_bytes_clone", |b| {
        b.iter(|| black_box(vosu::api::v1::encode(&cached).unwrap()))
    });
    cache.bench_function("v2_bytes_clone", |b| {
        b.iter(|| black_box(vosu::api::v2::encode(&cached).unwrap()))
    });
    cache.bench_function("v2_precise_bytes_clone", |b| {
        b.iter(|| black_box(vosu::api::precise::encode(&cached).unwrap()))
    });
    cache.bench_function("stream_companion_bytes_clone", |b| {
        b.iter(|| black_box(vosu::api::sc::encode(&cached).unwrap()))
    });
    cache.finish();
}

struct PublicationSnapshot {
    generation: u64,
    encodes: Arc<AtomicUsize>,
}

impl StateSnapshot for PublicationSnapshot {
    fn generation(&self) -> u64 {
        self.generation
    }
    fn encode_unfiltered(&self) -> Result<Bytes, String> {
        self.encodes.fetch_add(1, Ordering::Relaxed);
        Ok(Bytes::from(self.generation.to_string()))
    }
    fn encode_filtered(&self, _filters: &[Filter]) -> Result<Bytes, String> {
        self.encode_unfiltered()
    }
}

struct PublicationProvider {
    generation: AtomicU64,
    encodes: Arc<AtomicUsize>,
    event: watch::Sender<u64>,
}

impl PublicationProvider {
    fn new() -> Self {
        let (event, _) = watch::channel(0);
        Self {
            generation: AtomicU64::new(0),
            encodes: Arc::new(AtomicUsize::new(0)),
            event,
        }
    }
    fn publish(&self) -> u64 {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.event.send_replace(generation);
        generation
    }
}

impl StateProvider for PublicationProvider {
    fn poll_rate(&self, _schema: Schema) -> Duration {
        Duration::from_millis(1)
    }
    fn subscribe(&self, _schema: Schema) -> watch::Receiver<u64> {
        self.event.subscribe()
    }
    fn capture(&self, _schema: Schema) -> Option<Arc<dyn StateSnapshot>> {
        Some(Arc::new(PublicationSnapshot {
            generation: self.generation.load(Ordering::Acquire),
            encodes: Arc::clone(&self.encodes),
        }))
    }
}

fn bench_publication(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut group = c.benchmark_group("publication");
    for clients in [1_usize, 100, 1_000] {
        let provider = Arc::new(PublicationProvider::new());
        let hub = WebSocketHub::new(Schema::V2, provider.clone());
        let mut connections = Vec::with_capacity(clients);
        let mut receivers = Vec::with_capacity(clients);
        for _ in 0..clients {
            let (connection, receiver) = hub.connect();
            connections.push(connection);
            receivers.push(receiver);
        }
        let runner = runtime.spawn({
            let hub = hub.clone();
            async move { hub.run().await }
        });
        runtime.block_on(async {
            for receiver in &mut receivers {
                let _ = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                    .await
                    .unwrap();
            }
            for connection in &connections {
                connection.write_completed();
            }
        });
        group.throughput(Throughput::Elements(clients as u64));
        group.bench_with_input(
            BenchmarkId::new("unfiltered_clients", clients),
            &clients,
            |b, _| {
                b.iter(|| {
                    runtime.block_on(async {
                        let expected = provider.publish().to_string();
                        for receiver in &mut receivers {
                            match receiver.recv().await {
                                Some(Outbound::Text(bytes)) => {
                                    assert_eq!(bytes, expected.as_bytes())
                                }
                                frame => panic!("unexpected publication frame: {frame:?}"),
                            }
                        }
                        for connection in &connections {
                            connection.write_completed();
                        }
                    })
                });
            },
        );
        hub.shutdown();
        runtime.block_on(runner).unwrap();
    }
    group.finish();
}

const SUBSCRIPTION_PLAN_VALUES: usize = 1_000;

struct SubscriptionSnapshot {
    revision: u64,
    document_builds: Arc<AtomicUsize>,
}

impl V2SubscriptionSnapshot for SubscriptionSnapshot {
    fn revision(&self) -> u64 {
        self.revision
    }

    fn document(&self) -> Result<Option<serde_json::Value>, ApiError> {
        self.document_builds.fetch_add(1, Ordering::Relaxed);
        let clients: Vec<_> = (0..SUBSCRIPTION_PLAN_VALUES)
            .map(|index| serde_json::Value::from(index as u64))
            .collect();
        Ok(Some(serde_json::json!({
            "revision": self.revision,
            "clients": clients,
        })))
    }
}

struct SubscriptionProvider {
    revision: AtomicU64,
    document_builds: Arc<AtomicUsize>,
    event: watch::Sender<u64>,
}

impl SubscriptionProvider {
    fn new() -> Self {
        let (event, _) = watch::channel(0);
        Self {
            revision: AtomicU64::new(0),
            document_builds: Arc::new(AtomicUsize::new(0)),
            event,
        }
    }

    fn publish(&self) {
        let revision = self.revision.fetch_add(1, Ordering::AcqRel) + 1;
        self.event.send_replace(revision);
    }
}

impl V2SubscriptionProvider for SubscriptionProvider {
    fn subscribe(&self) -> watch::Receiver<u64> {
        self.event.subscribe()
    }

    fn capture(&self) -> Arc<dyn V2SubscriptionSnapshot> {
        Arc::new(SubscriptionSnapshot {
            revision: self.revision.load(Ordering::Acquire),
            document_builds: Arc::clone(&self.document_builds),
        })
    }
}

fn bench_v2_subscription(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut group = c.benchmark_group("v2_subscription");

    for plan_kind in ["shared_plan", "distinct_plans"] {
        for clients in [1_usize, 100, 1_000] {
            let provider = Arc::new(SubscriptionProvider::new());
            let hub = V2SubscriptionHub::new(provider.clone(), Duration::ZERO);
            let mut connections = Vec::with_capacity(clients);
            let mut receivers = Vec::with_capacity(clients);
            for index in 0..clients {
                let (connection, receiver) = hub.connect();
                let pointers = if plan_kind == "shared_plan" {
                    vec!["/revision".to_owned(), "/clients/0".to_owned()]
                } else {
                    vec![format!("/clients/{index}"), "/revision".to_owned()]
                };
                connection.subscribe(pointers).unwrap();
                connections.push(connection);
                receivers.push(receiver);
            }

            let runner = runtime.spawn({
                let hub = hub.clone();
                async move { hub.run().await }
            });
            runtime.block_on(async {
                for receiver in &mut receivers {
                    assert!(matches!(
                        receiver.recv().await,
                        Some(V2SubscriptionOutbound::Text(_))
                    ));
                }
                assert_eq!(provider.document_builds.load(Ordering::Relaxed), 1);

                provider.publish();
                tokio::time::sleep(Duration::from_millis(1)).await;
                assert_eq!(provider.document_builds.load(Ordering::Relaxed), 1);

                for connection in &connections {
                    connection.write_completed();
                }
                for receiver in &mut receivers {
                    assert!(matches!(
                        receiver.recv().await,
                        Some(V2SubscriptionOutbound::Text(_))
                    ));
                }
                assert_eq!(provider.document_builds.load(Ordering::Relaxed), 2);
                for connection in &connections {
                    connection.write_completed();
                }
            });

            group.throughput(Throughput::Elements(clients as u64));
            group.bench_with_input(BenchmarkId::new(plan_kind, clients), &clients, |b, _| {
                b.iter(|| {
                    runtime.block_on(async {
                        let before = provider.document_builds.load(Ordering::Relaxed);
                        provider.publish();
                        for receiver in &mut receivers {
                            assert!(matches!(
                                receiver.recv().await,
                                Some(V2SubscriptionOutbound::Text(_))
                            ));
                        }
                        assert_eq!(provider.document_builds.load(Ordering::Relaxed), before + 1);
                        for connection in &connections {
                            connection.write_completed();
                        }
                    })
                });
            });
            hub.shutdown();
            runtime.block_on(runner).unwrap();
        }
    }
    group.finish();
}

#[cfg(target_os = "windows")]
#[repr(align(4096))]
struct BenchmarkPage([u8; 4096]);

#[cfg(target_os = "windows")]
fn bench_windows_process_memory(c: &mut Criterion) {
    let backing = black_box(BenchmarkPage([0x5a_u8; 4096]));
    let address = black_box(backing.0.as_ptr() as usize);
    let memory = vosu::process::open_process(std::process::id()).expect("open benchmark process");
    let mut group = c.benchmark_group("windows_process_memory");
    group.throughput(Throughput::Bytes(64));

    group.bench_function("read_exact_64_bytes", |b| {
        b.iter(|| {
            let mut destination = [0_u8; 64];
            black_box(memory.read_exact(black_box(address), black_box(&mut destination)))
                .expect("read benchmark process");
            black_box(destination)
        });
    });

    group.bench_function("read_many_single_64_bytes", |b| {
        b.iter(|| {
            let mut destination = [0_u8; 64];
            {
                let mut requests = [ReadRequest::new(
                    black_box(address),
                    black_box(&mut destination),
                )];
                black_box(memory.read_many(black_box(&mut requests)))
                    .expect("read benchmark process");
            }
            black_box(destination)
        });
    });

    group.bench_function("read_many_contiguous_8x8_bytes", |b| {
        b.iter(|| {
            let mut destinations = [[0_u8; 8]; 8];
            {
                let [first, second, third, fourth, fifth, sixth, seventh, eighth] =
                    &mut destinations;
                let mut requests = [
                    ReadRequest::new(black_box(address), black_box(first)),
                    ReadRequest::new(black_box(address + 8), black_box(second)),
                    ReadRequest::new(black_box(address + 16), black_box(third)),
                    ReadRequest::new(black_box(address + 24), black_box(fourth)),
                    ReadRequest::new(black_box(address + 32), black_box(fifth)),
                    ReadRequest::new(black_box(address + 40), black_box(sixth)),
                    ReadRequest::new(black_box(address + 48), black_box(seventh)),
                    ReadRequest::new(black_box(address + 56), black_box(eighth)),
                ];
                black_box(memory.read_many(black_box(&mut requests)))
                    .expect("read benchmark process");
            }
            black_box(destinations)
        });
    });

    group.bench_function("read_many_gapped_8x8_bytes", |b| {
        b.iter(|| {
            let mut destinations = [[0_u8; 8]; 8];
            {
                let [first, second, third, fourth, fifth, sixth, seventh, eighth] =
                    &mut destinations;
                let mut requests = [
                    ReadRequest::new(black_box(address), black_box(first)),
                    ReadRequest::new(black_box(address + 64), black_box(second)),
                    ReadRequest::new(black_box(address + 128), black_box(third)),
                    ReadRequest::new(black_box(address + 192), black_box(fourth)),
                    ReadRequest::new(black_box(address + 256), black_box(fifth)),
                    ReadRequest::new(black_box(address + 320), black_box(sixth)),
                    ReadRequest::new(black_box(address + 384), black_box(seventh)),
                    ReadRequest::new(black_box(address + 448), black_box(eighth)),
                ];
                black_box(memory.read_many(black_box(&mut requests)))
                    .expect("read benchmark process");
            }
            black_box(destinations)
        });
    });

    group.bench_function("read_many_wide_8x8_bytes", |b| {
        b.iter(|| {
            let mut destinations = [[0_u8; 8]; 8];
            {
                let [first, second, third, fourth, fifth, sixth, seventh, eighth] =
                    &mut destinations;
                let mut requests = [
                    ReadRequest::new(black_box(address + 256), black_box(first)),
                    ReadRequest::new(black_box(address + 736), black_box(second)),
                    ReadRequest::new(black_box(address + 1_216), black_box(third)),
                    ReadRequest::new(black_box(address + 1_696), black_box(fourth)),
                    ReadRequest::new(black_box(address + 2_176), black_box(fifth)),
                    ReadRequest::new(black_box(address + 2_656), black_box(sixth)),
                    ReadRequest::new(black_box(address + 3_136), black_box(seventh)),
                    ReadRequest::new(black_box(address + 3_616), black_box(eighth)),
                ];
                black_box(memory.read_many(black_box(&mut requests)))
                    .expect("read benchmark process");
            }
            black_box(destinations)
        });
    });

    group.finish();
    black_box(backing);
}

#[cfg(target_os = "linux")]
fn bench_linux_process_memory(c: &mut Criterion) {
    let source = black_box([0x5a_u8; 64]);
    let address = source.as_ptr() as usize;
    let memory = vosu::process::open_process(std::process::id()).expect("open benchmark process");
    let mut group = c.benchmark_group("linux_process_memory");
    group.throughput(Throughput::Bytes(source.len() as u64));
    group.bench_function("current_process_64_bytes", |b| {
        b.iter(|| {
            let mut destination = [0_u8; 64];
            memory
                .read_exact(black_box(address), black_box(&mut destination))
                .expect("read benchmark process");
            black_box(destination)
        });
    });
    group.finish();
}

fn hot_paths(c: &mut Criterion) {
    bench_batch_scan(c);
    bench_snapshot_reads(c);
    bench_map_calculation(c);
    #[cfg(target_os = "linux")]
    bench_linux_process_memory(c);
    #[cfg(target_os = "windows")]
    bench_windows_process_memory(c);
    bench_serializers(c);
    bench_publication(c);
    bench_v2_subscription(c);
}

criterion_group!(benches, hot_paths);
criterion_main!(benches);
