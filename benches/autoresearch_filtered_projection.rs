use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;
use tokio::sync::watch;
use vosu::api::ApiSchema;
use vosu::api::{ApiRuntimeSettings, CombinedSnapshot, ManagerInstanceSnapshot, ManagerSnapshot};
use vosu::instance::InstanceSettings;
use vosu::model::{
    enums::ClientType,
    states::{HitErrorArray, KeypressButton},
};
use vosu::runtime::{ProductionSnapshotEncoder, SnapshotEncoder};
use vosu::server::websocket::{
    Filter, Outbound, OutboundReceiver, Schema, StateProvider, StateSnapshot, WebSocketHub,
};
use vosu::state::{PreciseState, RegularState, StateBundleSnapshot};

pub const SCHEMAS: [(Schema, ApiSchema, &str); 4] = [
    (Schema::V1, ApiSchema::V1, "v1"),
    (Schema::V2, ApiSchema::V2, "v2"),
    (Schema::V2Precise, ApiSchema::V2Precise, "v2_precise"),
    (
        Schema::StreamCompanion,
        ApiSchema::StreamCompanion,
        "stream_companion",
    ),
];

// Explicit frozen copy of hot_paths' single stable focused-instance fixture.
// The 100-element history supplies present, distinct canonical array-index filters.
pub fn fixture() -> CombinedSnapshot {
    fixture_offset(0)
}
pub fn fixture_offset(offset: usize) -> CombinedSnapshot {
    let regular = RegularState {
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
    };
    let precise = PreciseState {
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
        hit_errors: Arc::new(HitErrorArray {
            all: (0..100).map(|i| (i + offset) as f64).collect(),
            press: vec![-2.0, 3.0],
            release: vec![4.0],
        }),
        total_hit_errors: 0.0,
        committed_hit_error_index: 0,
        unstable_rate: 0.0,
    };
    let state = Arc::new(StateBundleSnapshot {
        source: ClientType::Stable,
        generation: 1,
        reset_epoch: 0,
        regular: Arc::new(regular),
        precise: Arc::new(precise),
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

pub fn nested(field: &str, keys: Vec<Filter>) -> Filter {
    Filter::Nested {
        field: field.into(),
        keys,
    }
}

// Ordered list: unique client i selects exactly the canonical index i, 0..99.
pub fn filters(schema: Schema, index: usize) -> Vec<Filter> {
    let selected = vec![Filter::Field(index.to_string())];
    match schema {
        Schema::V1 => vec![
            nested("menu", vec![Filter::Field("state".into())]),
            nested(
                "gameplay",
                vec![nested("hits", vec![nested("hitErrorArray", selected)])],
            ),
        ],
        Schema::V2 => vec![
            nested("state", vec![Filter::Field("number".into())]),
            nested("play", vec![nested("hitErrorArray", selected)]),
        ],
        Schema::V2Precise => vec![
            nested(
                "keys",
                vec![nested("k1", vec![Filter::Field("count".into())])],
            ),
            nested("hitErrors", selected),
        ],
        Schema::StreamCompanion => vec![
            Filter::Field("status".into()),
            nested("hitErrors", selected),
        ],
    }
}

// Independent frozen reference, not another call through the code under test.
pub fn reference(schema: Schema, index: usize) -> Bytes {
    reference_offset(schema, index, 0)
}
pub fn reference_offset(schema: Schema, index: usize, offset: usize) -> Bytes {
    let value = index + offset;
    let array = format!("{{\"{index}\":{value}}}");
    Bytes::from(match schema {
        Schema::V1 => format!(
            "{{\"menu\":{{\"state\":0}},\"gameplay\":{{\"hits\":{{\"hitErrorArray\":{array}}}}}}}"
        ),
        Schema::V2 => {
            format!("{{\"state\":{{\"number\":0}},\"play\":{{\"hitErrorArray\":{array}}}}}")
        }
        Schema::V2Precise => {
            format!("{{\"keys\":{{\"k1\":{{\"count\":0}}}},\"hitErrors\":{array}}}")
        }
        Schema::StreamCompanion => format!("{{\"status\":1,\"hitErrors\":{array}}}"),
    })
}

pub struct Provider {
    pub generation: AtomicU64,
    event: watch::Sender<u64>,
    snapshots: [Arc<CombinedSnapshot>; 3],
}
impl Default for Provider {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider {
    pub fn new() -> Self {
        let (event, _) = watch::channel(0);
        Self {
            generation: AtomicU64::new(0),
            event,
            snapshots: [0, 100, 200].map(|offset| Arc::new(fixture_offset(offset))),
        }
    }
    pub fn publish(&self) -> u64 {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.event.send_replace(generation);
        generation
    }
    pub fn notify_unchanged(&self) {
        self.event
            .send_replace(self.generation.load(Ordering::Acquire));
    }
}
struct Snapshot {
    generation: u64,
    snapshot: Arc<CombinedSnapshot>,
    schema: ApiSchema,
}
impl StateSnapshot for Snapshot {
    fn generation(&self) -> u64 {
        self.generation
    }
    fn encode_unfiltered(&self) -> Result<Bytes, String> {
        self.encode_filtered(&[])
    }
    fn encode_filtered(&self, filters: &[Filter]) -> Result<Bytes, String> {
        ProductionSnapshotEncoder
            .encode(&self.snapshot, self.schema, filters)
            .map_err(|e| e.to_string())
    }
}
impl StateProvider for Provider {
    fn poll_rate(&self, _: Schema) -> Duration {
        // Zero (including 1 ns) never yields in the production hub. Paused Tokio
        // time auto-advances this benchmark-only cadence with no wall-clock delay.
        Duration::from_millis(1)
    }
    fn subscribe(&self, _: Schema) -> watch::Receiver<u64> {
        self.event.subscribe()
    }
    fn capture(&self, schema: Schema) -> Option<Arc<dyn StateSnapshot>> {
        let schema = SCHEMAS.iter().find(|s| s.0 == schema).unwrap().1;
        let generation = self.generation.load(Ordering::Acquire);
        Some(Arc::new(Snapshot {
            generation,
            snapshot: self.snapshots[(generation % 3) as usize].clone(),
            schema,
        }))
    }
}
pub async fn receive(receiver: &mut OutboundReceiver) -> Bytes {
    match tokio::time::timeout(Duration::from_secs(1), receiver.recv())
        .await
        .expect("bounded receive")
    {
        Some(Outbound::Text(bytes)) => bytes,
        frame => panic!("unexpected frame: {frame:?}"),
    }
}

fn bench(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap();
    let mut group = c.benchmark_group("filtered_projection");
    // Keep Criterion CLI sampling and timing controls effective for paired runs.
    group.nresamples(10_000);
    for (schema, api_schema, name) in SCHEMAS {
        for unique in [false, true] {
            for clients in [1, 10, 100] {
                let provider = Arc::new(Provider::new());
                let hub = WebSocketHub::new(schema, provider.clone());
                let plans: Vec<_> = (0..clients)
                    .map(|i| filters(schema, if unique { i } else { 0 }))
                    .collect();
                let expected: Vec<Vec<_>> = [0, 100, 200]
                    .into_iter()
                    .map(|offset| {
                        (0..clients)
                            .map(|i| reference_offset(schema, if unique { i } else { 0 }, offset))
                            .collect()
                    })
                    .collect();
                for (snapshot, expected) in provider.snapshots.iter().zip(&expected) {
                    for (plan, bytes) in plans.iter().zip(expected) {
                        assert_eq!(
                            ProductionSnapshotEncoder
                                .encode(snapshot, api_schema, plan)
                                .unwrap(),
                            *bytes
                        );
                    }
                }
                let (connections, mut receivers): (Vec<_>, Vec<_>) = plans
                    .into_iter()
                    .map(|plan| {
                        let (connection, receiver) = hub.connect();
                        connection.set_filters(plan);
                        (connection, receiver)
                    })
                    .unzip();
                let runner = runtime.spawn({
                    let hub = hub.clone();
                    async move { hub.run().await }
                });
                runtime.block_on(async {
                    for (receiver, expected) in receivers.iter_mut().zip(&expected[0]) {
                        assert_eq!(receive(receiver).await, *expected);
                    }
                    for connection in &connections {
                        connection.write_completed();
                    }
                });
                let variant = if unique { "unique" } else { "identical" };
                group.bench_with_input(
                    BenchmarkId::new(format!("{name}_{variant}"), clients),
                    &clients,
                    |b, _| {
                        b.iter(|| {
                            runtime.block_on(async {
                                let before = provider.generation.load(Ordering::Acquire);
                                let generation = provider.publish();
                                assert_eq!(generation, before + 1);
                                for (receiver, expected) in receivers
                                    .iter_mut()
                                    .zip(&expected[(generation % 3) as usize])
                                {
                                    assert_eq!(receive(receiver).await, *expected);
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
                drop(connections);
                assert_eq!(hub.client_count(), 0);
            }
        }
    }
    group.finish();
    let snapshot = fixture();
    let mut encoder = c.benchmark_group("filtered_encoder");
    encoder.nresamples(10_000);
    for (schema, api_schema, name) in SCHEMAS {
        for unique in [false, true] {
            let plans: Vec<_> = (0..100)
                .map(|i| filters(schema, if unique { i } else { 0 }))
                .collect();
            for (i, plan) in plans.iter().enumerate() {
                assert_eq!(
                    ProductionSnapshotEncoder
                        .encode(&snapshot, api_schema, plan)
                        .unwrap(),
                    reference(schema, if unique { i } else { 0 })
                );
            }
            encoder.bench_function(
                format!("{name}/{}_100", if unique { "unique" } else { "identical" }),
                |b| {
                    b.iter(|| {
                        for plan in &plans {
                            black_box(
                                ProductionSnapshotEncoder
                                    .encode(&snapshot, api_schema, plan)
                                    .unwrap(),
                            );
                        }
                    });
                },
            );
        }
    }
    encoder.finish();
}
criterion_group! {
    name = benches;
    config = Criterion::default().without_plots();
    targets = bench
}
criterion_main!(benches);
