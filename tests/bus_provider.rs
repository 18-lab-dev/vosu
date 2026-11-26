//! `vosu_core::server::bus_provider::BusStateProvider` with the real bundled
//! adapters: one shared latest-only encoding per schema across HTTP and
//! WebSocket, connection-owned demand and adapter-owned derived work. Lives in
//! the root because core never depends on a bundled adapter.
use bytes::Bytes;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;
use vosu_core::server::bus_provider::{BusStateProvider, identity};
use vosu_core::server::routes::{CachedJson, StateJsonProvider, StateRoute};
use vosu_core::server::websocket::{Outbound, Schema, StateProvider, StateSnapshot, WebSocketHub};
use vosu_plugin_api::Selector;
use vosu_surface::filter::Filter;
use vosu_surface::projection::{Projection, ProjectionError, ProjectionView};
use vosu_surface::requester::BundledRequester;
use vosu_surface::{files::*, resources::*, routes::*, state::*};

#[path = "../crates/vosu-adapter-upstream/src/tests/fixture.rs"]
mod fixture;

type Adapter = dyn Projection<Filter, Bytes>;
type View = dyn ProjectionView<Filter, Bytes>;

/// The provider's current snapshot through its public transport contract
/// (`StateProvider::capture` answers only its own schema).
trait CurrentSnapshot {
    fn snapshot(&self) -> Option<Arc<dyn StateSnapshot>>;
}
impl CurrentSnapshot for BusStateProvider {
    fn snapshot(&self) -> Option<Arc<dyn StateSnapshot>> {
        [
            Schema::V1,
            Schema::V2,
            Schema::V2Precise,
            Schema::StreamCompanion,
        ]
        .into_iter()
        .find_map(|schema| self.capture(schema))
    }
}

struct Requester {
    image: Mutex<CapturedSelection>,
    events: watch::Sender<Arc<SurfaceUpdate>>,
    captures: AtomicUsize,
    leases: Arc<AtomicUsize>,
    selections: Arc<Mutex<Vec<Vec<Selector>>>>,
    current_selectors: Arc<Mutex<Vec<Selector>>>,
    resource: Mutex<Option<Arc<[u8]>>>,
    opened: watch::Sender<u64>,
    resource_gate: Arc<tokio::sync::Notify>,
}
struct Released(Arc<AtomicUsize>, Arc<Mutex<Vec<Selector>>>);
impl Drop for Released {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
        self.1.lock().unwrap().clear();
    }
}
impl Requester {
    fn new(image: CapturedSelection) -> Arc<Self> {
        Arc::new(Self {
            events: watch::channel(Arc::new(SurfaceUpdate {
                session: None,
                revision: 0,
                values: vec![],
            }))
            .0,
            image: Mutex::new(image),
            captures: AtomicUsize::new(0),
            leases: Arc::default(),
            selections: Arc::default(),
            current_selectors: Arc::default(),
            resource: Mutex::new(None),
            opened: watch::channel(0).0,
            resource_gate: Arc::default(),
        })
    }
    fn publish(&self, image: CapturedSelection) {
        *self.image.lock().unwrap() = image;
        self.events
            .send_modify(|update| Arc::make_mut(update).revision += 1);
    }
}
impl FilesRequester for Requester {
    fn declared_root(&self, _: &IdentifiedSession, _: &str) -> Result<RootLease, FileError> {
        Err(FileError::PermissionDenied)
    }
    fn open_calculation_path(&self, _: &IdentifiedSession, _: &str) -> Operation<'_, OpenedFile> {
        Box::pin(async { Err(FileError::PermissionDenied) })
    }
}
impl ResourceRequester for Requester {
    fn open_published_resource(
        &self,
        _: &IdentifiedSession,
        token: &str,
    ) -> Operation<'_, PublishedResource> {
        let bytes = self.resource.lock().unwrap().clone();
        let token = token.to_owned();
        Box::pin(async move {
            let bytes = bytes.ok_or(FileError::NotFound)?;
            self.opened.send_modify(|count| *count += 1);
            Ok(PublishedResource {
                identity: token,
                media_type: "application/x-osu",
                file: OpenedFile::new(
                    FileMetadata {
                        size: bytes.len() as u64,
                        modified: None,
                        identity: Arc::from(b"chart".as_slice()),
                    },
                    ChartBytes {
                        bytes,
                        gate: self.resource_gate.clone(),
                    },
                ),
            })
        })
    }
}
struct ChartBytes {
    bytes: Arc<[u8]>,
    gate: Arc<tokio::sync::Notify>,
}
impl FileReader for ChartBytes {
    fn read(&self, offset: u64, len: u32) -> Operation<'_, Vec<u8>> {
        Box::pin(async move {
            tokio::time::timeout(Duration::from_secs(2), self.gate.notified())
                .await
                .map_err(|_| FileError::Io)?;
            let start = offset as usize;
            Ok(self.bytes[start..(start + len as usize).min(self.bytes.len())].to_vec())
        })
    }
}
impl BundledRequester for Requester {
    fn capture(&self, _: SessionTarget, _: &[Selector]) -> Result<CapturedSelection, SurfaceError> {
        self.captures.fetch_add(1, Ordering::SeqCst);
        Ok(self.image.lock().unwrap().clone())
    }
    fn capture_roster(&self, _: &str) -> Result<IdentifiedRoster, SurfaceError> {
        Ok(self.image.lock().unwrap().roster.clone())
    }
    fn subscribe(
        &self,
        _: SessionTarget,
        selectors: Vec<Selector>,
    ) -> Result<SurfaceSubscription, SurfaceError> {
        self.leases.fetch_add(1, Ordering::SeqCst);
        *self.current_selectors.lock().unwrap() = selectors.clone();
        self.selections.lock().unwrap().push(selectors);
        let selections = self.selections.clone();
        let current = self.current_selectors.clone();
        Ok(SurfaceSubscription::new(
            self.events.subscribe(),
            Released(self.leases.clone(), self.current_selectors.clone()),
        )
        .with_replacement(move |selectors| {
            *current.lock().unwrap() = selectors.clone();
            selections.lock().unwrap().push(selectors);
            Ok(())
        }))
    }
    fn register_routes(
        &self,
        _: Vec<(RouteDescriptor, Arc<dyn RouteProvider>)>,
    ) -> Result<RegistrationLease, RegistrationError> {
        Err(RegistrationError::UnknownRequester)
    }
}
struct Counted {
    inner: Arc<Adapter>,
    decodes: Arc<AtomicUsize>,
    encodes: Arc<AtomicUsize>,
    fail: bool,
}
struct CountedView {
    inner: Arc<View>,
    encodes: Arc<AtomicUsize>,
    fail: bool,
}
impl Projection<Filter, Bytes> for Counted {
    fn observes_field(&self, entry: &vosu_plugin_api::StateEntry, field: &str) -> bool {
        self.inner.observes_field(entry, field)
    }
    fn selectors(&self) -> Vec<Selector> {
        self.inner.selectors()
    }
    fn demand_selectors(&self, filters: &[Filter]) -> Vec<Selector> {
        self.inner.demand_selectors(filters)
    }
    fn decode(&self, capture: CapturedSelection) -> Result<Arc<View>, ProjectionError> {
        self.decodes.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(CountedView {
            inner: self.inner.decode(capture)?,
            encodes: self.encodes.clone(),
            fail: self.fail,
        }))
    }
}
impl ProjectionView<Filter, Bytes> for CountedView {
    fn encode(&self, filters: &[Filter]) -> Result<Bytes, String> {
        self.encodes.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            Err("fixture encoding failure".into())
        } else {
            self.inner.encode(filters)
        }
    }
}
fn setup(fail: bool) -> (Arc<Requester>, Arc<BusStateProvider>, Arc<Counted>) {
    let adapter = Arc::new(Counted {
        inner: Arc::new(vosu_adapter_upstream::provider::Regular),
        decodes: Arc::default(),
        encodes: Arc::default(),
        fail,
    });
    let requester = Requester::new(fixture::capture(&fixture::source(), &adapter.selectors()));
    let provider = Arc::new(BusStateProvider::new(
        Schema::V2,
        requester.clone(),
        adapter.clone(),
        Duration::from_secs(3600),
    ));
    (requester, provider, adapter)
}
async fn ready(provider: &BusStateProvider) -> Bytes {
    match provider.cached_json(StateRoute::V2).await {
        CachedJson::Ready(bytes) => bytes,
        _ => panic!("expected ready production bytes"),
    }
}
async fn frame(receiver: &mut vosu_core::server::websocket::OutboundReceiver) -> Bytes {
    match tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
        .unwrap()
    {
        Outbound::Text(bytes) => bytes,
        other => panic!("unexpected {other:?}"),
    }
}
async fn stop(hub: &WebSocketHub, run: tokio::task::JoinHandle<()>) {
    hub.shutdown();
    tokio::time::timeout(Duration::from_secs(2), run)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn production_http_and_one_hundred_plain_clients_share_one_encoding() {
    let (requester, provider, counts) = setup(false);
    let http = ready(&provider).await;
    assert_eq!(requester.leases.load(Ordering::SeqCst), 0);
    let hub = WebSocketHub::new(Schema::V2, provider);
    let mut clients: Vec<_> = (0..100).map(|_| hub.connect()).collect();
    let run = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    for (_, receiver) in &mut clients {
        let bytes = frame(receiver).await;
        assert_eq!(bytes.as_ptr(), http.as_ptr());
        assert_eq!(bytes, http);
    }
    assert_eq!(counts.decodes.load(Ordering::SeqCst), 1);
    assert_eq!(counts.encodes.load(Ordering::SeqCst), 1);
    assert_eq!(requester.leases.load(Ordering::SeqCst), 1);
    drop(clients);
    assert_eq!(requester.leases.load(Ordering::SeqCst), 0);
    stop(&hub, run).await;
}
#[tokio::test]
async fn production_failing_unfiltered_result_is_shared_with_http() {
    let (_, provider, counts) = setup(true);
    assert!(matches!(
        provider.cached_json(StateRoute::V2).await,
        CachedJson::Failed { .. }
    ));
    let snapshot = provider.capture(Schema::V2).unwrap();
    for _ in 0..100 {
        assert_eq!(
            snapshot.encode_unfiltered().unwrap_err(),
            "fixture encoding failure"
        );
    }
    assert_eq!(counts.decodes.load(Ordering::SeqCst), 1);
    assert_eq!(counts.encodes.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn production_regular_cache_survives_precise_only_namespace_revision() {
    let (requester, provider, counts) = setup(false);
    let first = ready(&provider).await;
    let mut image = requester.image.lock().unwrap().clone();
    image.revision += 1;
    image.namespace_revisions = Arc::from([("vosu.osu".into(), 999)]);
    requester.publish(image);
    let second = ready(&provider).await;
    assert_eq!(first.as_ptr(), second.as_ptr());
    assert_eq!(counts.decodes.load(Ordering::SeqCst), 1);
    assert_eq!(counts.encodes.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn production_focus_generation_prevents_numeric_revision_collision() {
    let (requester, provider, counts) = setup(false);
    let first = provider.capture(Schema::V2).unwrap();
    let mut image = requester.image.lock().unwrap().clone();
    image.session.as_mut().unwrap().generation += 1;
    image.focus_revision += 1;
    fixture::replace(&mut image, "/play/score", serde_json::json!(654321.25));
    requester.publish(image);
    let second = provider.capture(Schema::V2).unwrap();
    assert_ne!(first.generation(), second.generation());
    assert_ne!(
        first.encode_unfiltered().unwrap(),
        second.encode_unfiltered().unwrap()
    );
    assert_eq!(counts.decodes.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn production_slow_writer_coalesces_one_thousand_updates_until_ack() {
    let (requester, provider, counts) = setup(false);
    let hub = WebSocketHub::new(Schema::V2, provider);
    let (connection, mut receiver) = hub.connect();
    let run = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    let first = frame(&mut receiver).await;
    for score in 1..=1000 {
        let mut image = requester.image.lock().unwrap().clone();
        fixture::replace(
            &mut image,
            "/play/score",
            serde_json::json!(f64::from(score)),
        );
        requester.publish(image);
    }
    assert!(receiver.try_recv().is_none());
    assert_eq!(counts.encodes.load(Ordering::SeqCst), 1);
    connection.write_completed();
    let latest = frame(&mut receiver).await;
    assert_ne!(first, latest);
    let document: serde_json::Value = serde_json::from_slice(&latest).unwrap();
    assert_eq!(document["play"]["score"], 1000);
    assert_eq!(counts.encodes.load(Ordering::SeqCst), 2);
    assert!(receiver.try_recv().is_none());
    stop(&hub, run).await;
}
#[tokio::test]
async fn idle_hub_never_captures_decodes_encodes_or_registers_demand() {
    let (requester, provider, counts) = setup(false);
    let hub = WebSocketHub::new(Schema::V2, provider);
    let run = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    for _ in 0..10 {
        requester
            .events
            .send_modify(|u| Arc::make_mut(u).revision += 1);
        tokio::task::yield_now().await;
    }
    assert_eq!(requester.captures.load(Ordering::SeqCst), 0);
    assert_eq!(requester.leases.load(Ordering::SeqCst), 0);
    assert_eq!(counts.decodes.load(Ordering::SeqCst), 0);
    assert_eq!(counts.encodes.load(Ordering::SeqCst), 0);
    stop(&hub, run).await;
}
#[tokio::test]
async fn production_precise_filter_replacement_releases_histories_not_other_clients() {
    let adapter = Arc::new(vosu_adapter_upstream::provider::Precise);
    let requester = Requester::new(fixture::capture(&fixture::source(), &adapter.selectors()));
    let provider = Arc::new(BusStateProvider::new(
        Schema::V2Precise,
        requester.clone(),
        adapter,
        Duration::from_secs(1),
    ));
    let hub = WebSocketHub::new(Schema::V2Precise, provider);
    let (a, ra) = hub.connect();
    let (_b, rb) = hub.connect();
    a.set_filters(vec![Filter::Field("keys".into())]);
    assert_eq!(requester.leases.load(Ordering::SeqCst), 1);
    assert!(
        requester
            .current_selectors
            .lock()
            .unwrap()
            .iter()
            .any(|s| s.pointer == "/play/hitErrors/all")
    );
    drop(rb);
    let selections = requester.selections.lock().unwrap();
    assert!(
        selections[0]
            .iter()
            .any(|s| s.pointer == "/play/hitErrors/all")
    );
    assert!(
        selections
            .last()
            .unwrap()
            .iter()
            .any(|s| s.pointer == "/keys/overlay")
    );
    assert!(
        !selections
            .last()
            .unwrap()
            .iter()
            .any(|s| s.pointer == "/play/hitErrors/all")
    );
    drop(selections);
    drop(ra);
    assert_eq!(requester.leases.load(Ordering::SeqCst), 0);
    assert!(requester.current_selectors.lock().unwrap().is_empty());
}

#[tokio::test]
async fn gosumemory_and_companion_providers_share_http_and_ws_bytes() {
    let source = fixture::source();
    let gosu: Arc<Adapter> = Arc::new(vosu_adapter_gosumemory::provider::Provider);
    let requester = Requester::new(fixture::capture(&source, &gosu.selectors()));
    let provider = Arc::new(BusStateProvider::new(
        Schema::V1,
        requester.clone(),
        gosu,
        Duration::from_secs(3600),
    ));
    let http = match provider.cached_json(StateRoute::V1).await {
        CachedJson::Ready(bytes) => bytes,
        _ => panic!("gosuMemory must be ready"),
    };
    let hub = WebSocketHub::new(Schema::V1, provider);
    let (_connection, mut receiver) = hub.connect();
    let run = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    assert_eq!(frame(&mut receiver).await.as_ptr(), http.as_ptr());
    stop(&hub, run).await;

    let requester = Requester::new(fixture::capture(
        &source,
        &vosu_adapter_streamcompanion::view::selectors(),
    ));
    let sc = Arc::new(vosu_adapter_streamcompanion::provider::Provider::new(
        requester.clone(),
    ));
    let provider = Arc::new(BusStateProvider::new(
        Schema::StreamCompanion,
        requester.clone(),
        sc.clone(),
        Duration::from_secs(3600),
    ));
    let http = match provider.cached_json(StateRoute::StreamCompanion).await {
        CachedJson::Ready(bytes) => bytes,
        _ => panic!("streamCompanion must be ready"),
    };
    let hub = WebSocketHub::new(Schema::StreamCompanion, provider);
    let (_connection, mut receiver) = hub.connect();
    let run = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    assert_eq!(frame(&mut receiver).await.as_ptr(), http.as_ptr());
    stop(&hub, run).await;
    tokio::time::timeout(Duration::from_secs(2), sc.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(requester.leases.load(Ordering::SeqCst), 0);
}

fn chart_requester() -> Arc<Requester> {
    let mut capture = fixture::capture(
        &fixture::source(),
        &vosu_adapter_streamcompanion::view::selectors(),
    );
    let entry = Arc::make_mut(&mut capture.values)
        .iter_mut()
        .find(|e| e.ns == "vosu.chart" && e.pointer == "/resources/file")
        .unwrap();
    entry.value = Some(
        ciborium::Value::serialized(&serde_json::json!({"url":"/api/v1/resources/test-chart"}))
            .unwrap(),
    );
    let requester = Requester::new(capture);
    *requester.resource.lock().unwrap() = Some(Arc::from(b"osu file format v14\n\n[General]\nMode:0\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:8\nApproachRate:9\nSliderMultiplier:1.4\nSliderTickRate:1\n\n[TimingPoints]\n0,500,4,2,1,80,1,0\n\n[HitObjects]\n64,64,1000,1,0,0:0:0:0:\n448,64,1200,1,0,0:0:0:0:\n64,320,1400,1,0,0:0:0:0:\n".as_slice()));
    requester
}

#[tokio::test]
async fn sc_derived_completion_invalidates_pending_bytes_without_bus_change() {
    let requester = chart_requester();
    let mut opened = requester.opened.subscribe();
    let sc = Arc::new(vosu_adapter_streamcompanion::provider::Provider::new(
        requester.clone(),
    ));
    let provider = Arc::new(BusStateProvider::new(
        Schema::StreamCompanion,
        requester.clone(),
        sc.clone(),
        Duration::from_secs(3600),
    ));
    let hub = WebSocketHub::new(Schema::StreamCompanion, provider.clone());
    let (connection, mut receiver) = hub.connect();
    let run = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    let pending = frame(&mut receiver).await;
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&pending).unwrap()["maxCombo"],
        0
    );
    tokio::time::timeout(Duration::from_secs(2), opened.changed())
        .await
        .unwrap()
        .unwrap();
    // Cache the pending generation again before completion. The worker's
    // completion notice has the same numeric value as its selection notice.
    assert_eq!(
        provider
            .snapshot()
            .unwrap()
            .encode_unfiltered()
            .unwrap()
            .as_ptr(),
        pending.as_ptr()
    );
    let mut completed_notice = sc.changes().unwrap();
    requester.resource_gate.notify_one();
    tokio::time::timeout(Duration::from_secs(2), completed_notice.changed())
        .await
        .unwrap()
        .unwrap();
    connection.write_completed();
    let completed = frame(&mut receiver).await;
    let document: serde_json::Value = serde_json::from_slice(&completed).unwrap();
    assert_eq!(document["maxCombo"], 3);
    assert_eq!(document["mapKiaiPoints"], serde_json::json!([]));
    let http = match provider.cached_json(StateRoute::StreamCompanion).await {
        CachedJson::Ready(bytes) => bytes,
        _ => panic!("completed chart is ready"),
    };
    assert_eq!(completed.as_ptr(), http.as_ptr());
    assert_eq!(sc.counts().parses, 1);
    drop(receiver);
    assert_eq!(requester.leases.load(Ordering::SeqCst), 0);
    stop(&hub, run).await;
    let counts = tokio::time::timeout(Duration::from_secs(2), sc.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(counts.parses, 1);
    assert_eq!(counts.joins, 1);
}

#[tokio::test]
async fn sc_key_only_consumer_never_opens_or_parses_chart() {
    let requester = chart_requester();
    let sc = Arc::new(vosu_adapter_streamcompanion::provider::Provider::new(
        requester.clone(),
    ));
    let provider = Arc::new(BusStateProvider::new(
        Schema::StreamCompanion,
        requester.clone(),
        sc.clone(),
        Duration::from_secs(3600),
    ));
    let hub = WebSocketHub::new(Schema::StreamCompanion, provider);
    let (connection, mut receiver) = hub.connect();
    connection.set_filters(vec![Filter::Field("keyOverlay".into())]);
    let run = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    let payload: serde_json::Value = serde_json::from_slice(&frame(&mut receiver).await).unwrap();
    assert!(payload["keyOverlay"].is_string());
    assert_eq!(sc.counts().starts, 0);
    assert_eq!(sc.counts().opens, 0);
    assert_eq!(sc.counts().parses, 0);
    {
        let selected = requester.selections.lock().unwrap();
        assert!(
            !selected
                .last()
                .unwrap()
                .iter()
                .any(|s| s.ns == "vosu.chart")
        );
        assert!(
            !selected
                .last()
                .unwrap()
                .iter()
                .any(|s| s.pointer == "/play/hitErrors/all")
        );
    }
    stop(&hub, run).await;
    tokio::time::timeout(Duration::from_secs(2), sc.shutdown())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn sc_failed_chart_is_not_retried_by_unrelated_precise_ticks() {
    let requester = chart_requester();
    *requester.resource.lock().unwrap() = None;
    let sc = vosu_adapter_streamcompanion::provider::Provider::new(requester.clone());
    let mut changes = sc.changes().unwrap();
    let retained = sc.retain(&[]);
    let image = requester.image.lock().unwrap().clone();
    assert!(sc.decode(image.clone()).is_ok());
    tokio::time::timeout(Duration::from_secs(2), async {
        while sc.counts().opens == 0 {
            changes.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    for tick in 1..=20 {
        let mut image = image.clone();
        fixture::replace(
            &mut image,
            "/play/preciseUnstableRate",
            serde_json::json!(f64::from(tick)),
        );
        assert!(sc.decode(image).is_ok());
    }
    assert_eq!(sc.counts().opens, 1);
    assert_eq!(sc.counts().parses, 0);
    drop(retained);
    let reacquired = sc.retain(&[]);
    tokio::time::timeout(Duration::from_secs(2), async {
        while sc.counts().opens < 2 {
            changes.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(sc.counts().opens, 2);
    drop(reacquired);
    tokio::time::timeout(Duration::from_secs(2), sc.shutdown())
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn tournament_regular_cache_identity_ignores_only_unemitted_precise_facts() {
    let entry = |count: i64, rate: f64, score: f64| {
        vosu_plugin_api::StateEntry {
        ns: "vosu.osu".into(), pointer: "/tourney".into(), error: None,
        value: Some(ciborium::Value::serialized(&serde_json::json!({"clients":[{"keysOverlay":{"k1":{"isPressed":true,"count":count}},"play":{"preciseUnstableRate":rate,"score":score}}]})).unwrap()),
    }
    };
    let before = vec![entry(11, 22.0, 33.0)];
    let precise = vec![entry(44, 55.0, 33.0)];
    let regular = vec![entry(44, 55.0, 66.0)];
    for adapter in [
        Arc::new(vosu_adapter_upstream::provider::Regular) as Arc<Adapter>,
        Arc::new(vosu_adapter_gosumemory::provider::Provider) as Arc<Adapter>,
    ] {
        assert!(identity::same_entries(&before, &precise, |entry, field| {
            adapter.observes_field(entry, field)
        }));
        assert!(!identity::same_entries(
            &before,
            &regular,
            |entry, field| adapter.observes_field(entry, field)
        ));
    }
}

#[test]
fn precise_tournament_cache_ignores_regular_updates_but_keeps_each_key_slot() {
    let adapter = Arc::new(vosu_adapter_upstream::provider::Precise);
    let mut image = fixture::capture(&fixture::source(), &adapter.selectors());
    let tourney = |score, key| serde_json::json!({"scoreVisible":true,"clients":[{"ipcId":3.0,"keysOverlay":{"k1":{"isPressed":true,"count":key},"k2":null,"m1":null,"m2":null},"play":{"score":score,"hitErrors":{"all":[1.0]}}}]});
    fixture::replace(&mut image, "/tourney", tourney(42.0, 11));
    let requester = Requester::new(image.clone());
    let provider = BusStateProvider::new(
        Schema::V2Precise,
        requester.clone(),
        adapter,
        Duration::from_secs(1),
    );
    let first = provider.snapshot().unwrap().encode_unfiltered().unwrap();
    fixture::replace(&mut image, "/tourney", tourney(84.0, 11));
    requester.publish(image.clone());
    let regular = provider.snapshot().unwrap().encode_unfiltered().unwrap();
    assert_eq!(first.as_ptr(), regular.as_ptr());
    fixture::replace(&mut image, "/tourney", tourney(84.0, 22));
    requester.publish(image);
    let precise = provider.snapshot().unwrap().encode_unfiltered().unwrap();
    assert_ne!(first, precise);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&precise).unwrap()["tourney"][0]["keys"]["k1"]
            ["count"],
        22
    );
}

#[tokio::test(start_paused = true)]
async fn union_disjoint_filters_preserve_legacy_tick_cadence_not_output_diffs() {
    let (requester, _, counts) = setup(false);
    let provider = Arc::new(BusStateProvider::new(
        Schema::V2,
        requester.clone(),
        counts,
        Duration::from_secs(1),
    ));
    let hub = WebSocketHub::new(Schema::V2, provider);
    let (a, mut ra) = hub.connect();
    let (b, mut rb) = hub.connect();
    a.set_filters(vec![Filter::Field("game".into())]);
    b.set_filters(vec![Filter::Nested {
        field: "play".into(),
        keys: vec![Filter::Field("score".into())],
    }]);
    let selectors = requester.current_selectors.lock().unwrap().clone();
    assert_eq!(selectors.len(), 2);
    assert!(selectors.iter().any(|s| s.pointer == "/session/paused"));
    assert!(selectors.iter().any(|s| s.pointer == "/play/score"));
    let run = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    let a_first = frame(&mut ra).await;
    let b_first = frame(&mut rb).await;
    a.write_completed();
    b.write_completed();
    let mut image = requester.image.lock().unwrap().clone();
    fixture::replace(&mut image, "/play/score", serde_json::json!(888888.5));
    requester.publish(image);
    tokio::task::yield_now().await;
    assert!(ra.try_recv().is_none());
    assert!(rb.try_recv().is_none());
    tokio::time::advance(Duration::from_secs(1)).await;
    // The 1.x hub sends a state frame each cadence, even if a client's filtered
    // output is unchanged. A has exactly its initial frame plus this one tick.
    let a_second = frame(&mut ra).await;
    let b_second = frame(&mut rb).await;
    assert_eq!(a_second, a_first);
    assert_ne!(b_second, b_first);
    assert!(ra.try_recv().is_none());
    assert!(rb.try_recv().is_none());
    drop(ra);
    assert_eq!(requester.leases.load(Ordering::SeqCst), 1);
    assert_eq!(requester.current_selectors.lock().unwrap().len(), 1);
    drop(rb);
    assert_eq!(requester.leases.load(Ordering::SeqCst), 0);
    assert!(requester.current_selectors.lock().unwrap().is_empty());
    stop(&hub, run).await;
}

#[tokio::test]
async fn production_absent_session_is_not_ready_and_creates_no_demand() {
    let (requester, provider, counts) = setup(false);
    requester.image.lock().unwrap().session = None;
    assert!(matches!(
        provider.cached_json(StateRoute::V2).await,
        CachedJson::NotReady
    ));
    assert_eq!(requester.leases.load(Ordering::SeqCst), 0);
    assert_eq!(counts.encodes.load(Ordering::SeqCst), 0);
}
