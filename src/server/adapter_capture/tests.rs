use super::*;
use crate::server::bus_provider::BusStateProvider;
use bytes::Bytes;
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst},
};
use std::time::Duration;
use tokio::sync::watch;
use vosu_core::server::routes::{StateJsonProvider, StateRoute};
use vosu_plugin_api::{CborValue, Selector, StateEntry};
use vosu_surface::filter::Filter;
use vosu_surface::{
    files::{FileError, FilesRequester, OpenedFile, Operation, RootLease},
    projection::{Projection, ProjectionError, ProjectionView},
    requester::BundledRequester,
    resources::{PublishedResource, ResourceRequester},
    routes::{RegistrationError, RegistrationLease, RouteDescriptor, RouteProvider},
    state::{IdentifiedRoster, SessionTarget, SurfaceError, SurfaceSubscription},
};

/// A bus whose image may move on immediately after each capture returns, so
/// any second capture by the provider or harness would observe a newer value.
struct Bus {
    image: Mutex<CapturedSelection>,
    drift: bool,
    captures: AtomicUsize,
    subscriptions: AtomicUsize,
}
impl Bus {
    fn new(image: CapturedSelection, drift: bool) -> Arc<Self> {
        Arc::new(Self {
            image: Mutex::new(image),
            drift,
            captures: AtomicUsize::new(0),
            subscriptions: AtomicUsize::new(0),
        })
    }
}
impl FilesRequester for Bus {
    fn declared_root(&self, _: &IdentifiedSession, _: &str) -> Result<RootLease, FileError> {
        Err(FileError::PermissionDenied)
    }
    fn open_calculation_path(&self, _: &IdentifiedSession, _: &str) -> Operation<'_, OpenedFile> {
        Box::pin(async { Err(FileError::PermissionDenied) })
    }
}
impl ResourceRequester for Bus {
    fn open_published_resource(
        &self,
        _: &IdentifiedSession,
        _: &str,
    ) -> Operation<'_, PublishedResource> {
        Box::pin(async { Err(FileError::NotFound) })
    }
}
impl BundledRequester for Bus {
    fn capture(&self, _: SessionTarget, _: &[Selector]) -> Result<CapturedSelection, SurfaceError> {
        self.captures.fetch_add(1, SeqCst);
        let mut image = self.image.lock().unwrap();
        let captured = image.clone();
        if self.drift {
            let next = i64::try_from(score(&image) + 1).unwrap();
            *image = with_value(&image, CborValue::Integer(next.into()));
            image.revision += 1;
        }
        Ok(captured)
    }
    fn capture_roster(&self, _: &str) -> Result<IdentifiedRoster, SurfaceError> {
        Ok(self.image.lock().unwrap().roster.clone())
    }
    fn subscribe(
        &self,
        _: SessionTarget,
        _: Vec<Selector>,
    ) -> Result<SurfaceSubscription, SurfaceError> {
        self.subscriptions.fetch_add(1, SeqCst);
        Err(SurfaceError::UnknownReader)
    }
    fn register_routes(
        &self,
        _: Vec<(RouteDescriptor, Arc<dyn RouteProvider>)>,
    ) -> Result<RegistrationLease, RegistrationError> {
        Err(RegistrationError::UnknownRequester)
    }
}

/// Records every capture it decodes and echoes the score it decoded.
#[derive(Default)]
struct Echo {
    decoded: Mutex<Vec<CapturedSelection>>,
    encodes: Arc<AtomicUsize>,
    fail: bool,
    derived: Option<watch::Sender<u64>>,
    pending: AtomicBool,
}
struct EchoView {
    score: i128,
    revision: u64,
    fail: bool,
    encodes: Arc<AtomicUsize>,
}
impl Projection<Filter, Bytes> for Echo {
    fn selectors(&self) -> Vec<Selector> {
        vec![Selector {
            ns: "vosu.osu".into(),
            pointer: "/play/score".into(),
        }]
    }
    fn demand_selectors(&self, _: &[Filter]) -> Vec<Selector> {
        self.selectors()
    }
    fn decode(
        &self,
        capture: CapturedSelection,
    ) -> Result<Arc<dyn ProjectionView<Filter, Bytes>>, ProjectionError> {
        self.decoded.lock().unwrap().push(capture.clone());
        Ok(Arc::new(EchoView {
            score: score(&capture),
            revision: self.revision(),
            fail: self.fail,
            encodes: self.encodes.clone(),
        }))
    }
    fn revision(&self) -> u64 {
        self.derived.as_ref().map_or(0, |derived| *derived.borrow())
    }
    fn changes(&self) -> Option<watch::Receiver<u64>> {
        self.derived.as_ref().map(watch::Sender::subscribe)
    }
    fn pending(&self) -> bool {
        self.pending.load(SeqCst)
    }
}
impl ProjectionView<Filter, Bytes> for EchoView {
    fn revision(&self) -> u64 {
        self.revision
    }
    fn encode(&self, _: &[Filter]) -> Result<Bytes, String> {
        self.encodes.fetch_add(1, SeqCst);
        if self.fail {
            return Err("echo encoding failure".into());
        }
        Ok(Bytes::from(format!("{{\"score\":{}}}", self.score)))
    }
}

fn score(capture: &CapturedSelection) -> i128 {
    match capture.values[0].value {
        Some(CborValue::Integer(score)) => i128::from(score),
        _ => -1,
    }
}
fn session(id: &str, instance: u64, foreground: Option<bool>) -> IdentifiedSession {
    IdentifiedSession {
        session: id.into(),
        generation: 1,
        owner: "vosu.osu".into(),
        owner_instance: instance,
        game: "osu".into(),
        variant: "lazer".into(),
        foreground,
    }
}
fn entry(pointer: &str, value: Option<CborValue>) -> StateEntry {
    StateEntry {
        ns: "vosu.osu".into(),
        pointer: pointer.into(),
        value,
        error: None,
    }
}
fn image(value: CborValue) -> CapturedSelection {
    let selected = session("host-session-7f3a", 4_242_424_242, Some(true));
    CapturedSelection {
        session: Some(selected.clone()),
        focus_revision: 3,
        roster: IdentifiedRoster {
            revision: 5,
            sessions: Arc::from([selected]),
        },
        revision: 9,
        namespace_revisions: Arc::from([("vosu.osu".to_owned(), 9)]),
        values: Arc::from([entry("/play/score", Some(value))]),
    }
}
fn with_value(capture: &CapturedSelection, value: CborValue) -> CapturedSelection {
    let mut next = capture.clone();
    Arc::make_mut(&mut next.values)[0].value = Some(value);
    next
}
fn provider(bus: &Arc<Bus>, adapter: &Arc<Echo>) -> BusStateProvider {
    BusStateProvider::new(
        Schema::V2,
        bus.clone(),
        adapter.clone(),
        Duration::from_secs(3600),
    )
}
fn sample(capture: CapturedSelection) -> AdapterCaptureSample {
    AdapterCaptureSample {
        schema: Schema::V2,
        provider_generation: 1,
        capture,
        derived_revision: 0,
        derived: DerivedStatus::None,
        http: CachedJson::Ready(Bytes::from_static(b"{}")),
    }
}
fn envelope(sample: &AdapterCaptureSample) -> Value {
    serde_json::from_slice(&sample.envelope().unwrap()).unwrap()
}
fn cbor(envelope: &Value, index: usize) -> Vec<u8> {
    STANDARD
        .decode(
            envelope["entries"][index]["value_cbor_b64"]
                .as_str()
                .unwrap(),
        )
        .unwrap()
}
fn keys(value: &Value) -> Vec<&str> {
    value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect()
}

#[test]
fn adapter_capture_is_the_decoded_selection_when_the_bus_moves_after_it() {
    let bus = Bus::new(image(CborValue::Integer(1.into())), true);
    let adapter = Arc::new(Echo::default());
    let provider = provider(&bus, &adapter);
    let first = provider.capture_for_harness().unwrap();
    // One capture, and the dump is exactly the selection the adapter decoded,
    // not the newer image the bus moved to right after that capture.
    assert_eq!(bus.captures.load(SeqCst), 1);
    assert_eq!(
        *adapter.decoded.lock().unwrap(),
        vec![first.capture.clone()]
    );
    assert_eq!(score(&first.capture), 1);
    assert_eq!(score(&bus.image.lock().unwrap()), 2);
    assert_eq!(
        first.http,
        CachedJson::Ready(Bytes::from_static(br#"{"score":1}"#))
    );
    assert_eq!(cbor(&envelope(&first), 0), [0x01]);

    let second = provider.capture_for_harness().unwrap();
    assert_eq!(bus.captures.load(SeqCst), 2);
    assert_eq!(score(&second.capture), 2);
    assert_eq!(second.provider_generation, first.provider_generation + 1);
    assert_eq!(
        second.http,
        CachedJson::Ready(Bytes::from_static(br#"{"score":2}"#))
    );
    assert_ne!(envelope(&first)["digest"], envelope(&second)["digest"]);
}

#[tokio::test]
async fn adapter_capture_sandwich_shares_the_route_snapshot_and_bytes() {
    let bus = Bus::new(image(CborValue::Integer(7.into())), false);
    let adapter = Arc::new(Echo::default());
    let provider = provider(&bus, &adapter);
    let before = provider.capture_for_harness().unwrap();
    let CachedJson::Ready(route) = provider.cached_json(StateRoute::V2).await else {
        panic!("route is ready");
    };
    let after = provider.capture_for_harness().unwrap();
    let CachedJson::Ready(dumped) = &after.http else {
        panic!("dump is ready");
    };
    // The route and both dumps are one snapshot: one decode, one encode, the
    // same retained bytes, generation and selection digest.
    assert_eq!(dumped.as_ptr(), route.as_ptr());
    assert_eq!(before.http, after.http);
    assert_eq!(before.provider_generation, after.provider_generation);
    assert_eq!(envelope(&before)["digest"], envelope(&after)["digest"]);
    assert_eq!(adapter.decoded.lock().unwrap().len(), 1);
    assert_eq!(adapter.encodes.load(SeqCst), 1);
    assert_eq!(bus.captures.load(SeqCst), 3);
    assert_eq!(bus.subscriptions.load(SeqCst), 0, "no sampling demand");
}

#[tokio::test]
async fn adapter_capture_not_ready_is_the_production_status_and_body() {
    let mut idle = image(CborValue::Integer(1.into()));
    idle.session = None;
    let bus = Bus::new(idle, false);
    let adapter = Arc::new(Echo::default());
    let provider = provider(&bus, &adapter);
    let dump = provider.capture_for_harness().unwrap();
    assert_eq!(dump.http, CachedJson::NotReady);
    assert_eq!(
        provider.cached_json(StateRoute::V2).await,
        CachedJson::NotReady
    );
    let envelope = envelope(&dump);
    // The legacy transport envelope, never the typed source-oracle string.
    assert_eq!(
        envelope["http"],
        json!({"kind": "not_ready", "status": 500,
               "body_utf8": "{\"error\":\"osu is not ready/running\"}"})
    );
    assert_eq!(envelope["session"], Value::Null);
    assert_eq!(adapter.encodes.load(SeqCst), 0);
    assert_eq!(bus.subscriptions.load(SeqCst), 0);
}

#[test]
fn adapter_capture_failed_encoding_is_the_production_failure() {
    let bus = Bus::new(image(CborValue::Integer(1.into())), false);
    let adapter = Arc::new(Echo {
        fail: true,
        ..Echo::default()
    });
    let dump = provider(&bus, &adapter).capture_for_harness().unwrap();
    assert_eq!(
        envelope(&dump)["http"],
        json!({"kind": "failed", "status": 500,
               "body_utf8": "{\"error\":\"echo encoding failure\"}"})
    );
}

#[test]
fn adapter_capture_requester_failure_is_an_error_not_a_sample() {
    struct Broken;
    impl FilesRequester for Broken {
        fn declared_root(&self, _: &IdentifiedSession, _: &str) -> Result<RootLease, FileError> {
            Err(FileError::PermissionDenied)
        }
        fn open_calculation_path(
            &self,
            _: &IdentifiedSession,
            _: &str,
        ) -> Operation<'_, OpenedFile> {
            Box::pin(async { Err(FileError::PermissionDenied) })
        }
    }
    impl ResourceRequester for Broken {
        fn open_published_resource(
            &self,
            _: &IdentifiedSession,
            _: &str,
        ) -> Operation<'_, PublishedResource> {
            Box::pin(async { Err(FileError::NotFound) })
        }
    }
    impl BundledRequester for Broken {
        fn capture(
            &self,
            _: SessionTarget,
            _: &[Selector],
        ) -> Result<CapturedSelection, SurfaceError> {
            Err(SurfaceError::UnknownReader)
        }
        fn capture_roster(&self, _: &str) -> Result<IdentifiedRoster, SurfaceError> {
            Err(SurfaceError::UnknownReader)
        }
        fn subscribe(
            &self,
            _: SessionTarget,
            _: Vec<Selector>,
        ) -> Result<SurfaceSubscription, SurfaceError> {
            Err(SurfaceError::UnknownReader)
        }
        fn register_routes(
            &self,
            _: Vec<(RouteDescriptor, Arc<dyn RouteProvider>)>,
        ) -> Result<RegistrationLease, RegistrationError> {
            Err(RegistrationError::UnknownRequester)
        }
    }
    let adapter = Arc::new(Echo::default());
    let provider = BusStateProvider::new(
        Schema::V2,
        Arc::new(Broken),
        adapter.clone(),
        Duration::from_secs(3600),
    );
    assert!(provider.capture_for_harness().is_err());
    assert!(adapter.decoded.lock().unwrap().is_empty());
}

#[test]
fn adapter_capture_reports_derived_status_of_its_snapshot() {
    let bus = Bus::new(image(CborValue::Integer(1.into())), false);
    let none = Arc::new(Echo::default());
    let dump = provider(&bus, &none).capture_for_harness().unwrap();
    assert_eq!(dump.derived, DerivedStatus::None);
    assert_eq!(
        envelope(&dump)["derived"],
        json!({"revision": 0, "status": "none"})
    );

    let derived = Arc::new(Echo {
        derived: Some(watch::channel(4).0),
        ..Echo::default()
    });
    let provider = provider(&bus, &derived);
    let ready = provider.capture_for_harness().unwrap();
    assert_eq!(
        (ready.derived, ready.derived_revision),
        (DerivedStatus::Ready, 4)
    );
    derived.pending.store(true, SeqCst);
    let pending = provider.capture_for_harness().unwrap();
    assert_eq!(pending.derived, DerivedStatus::Pending);
    assert_eq!(pending.provider_generation, ready.provider_generation);
    derived.pending.store(false, SeqCst);
    // Derived work completed: the next dump is a fresh snapshot that has it.
    derived.derived.as_ref().unwrap().send_replace(5);
    let refreshed = provider.capture_for_harness().unwrap();
    assert_eq!(
        (refreshed.derived, refreshed.derived_revision),
        (DerivedStatus::Ready, 5)
    );
    assert_eq!(refreshed.provider_generation, ready.provider_generation + 1);
}

#[test]
fn adapter_capture_cbor_integer_and_float_stay_distinct() {
    let values = [
        CborValue::Integer(1.into()),
        CborValue::Float(1.0),
        CborValue::Float(0.0),
        CborValue::Float(-0.0),
        CborValue::Null,
    ];
    let envelopes: Vec<_> = values
        .iter()
        .map(|value| envelope(&sample(image(value.clone()))))
        .collect();
    for (value, envelope) in values.iter().zip(&envelopes) {
        let bytes = cbor(envelope, 0);
        assert_eq!(bytes, vosu_plugin_api::encode(value).unwrap());
        let decoded: CborValue = vosu_plugin_api::decode(&bytes).unwrap();
        match (value, &decoded) {
            (CborValue::Float(expected), CborValue::Float(actual)) => {
                assert_eq!(expected.to_bits(), actual.to_bits());
            }
            _ => assert_eq!(&decoded, value),
        }
    }
    for (index, left) in envelopes.iter().enumerate() {
        for right in &envelopes[index + 1..] {
            assert_ne!(left["entries"], right["entries"]);
            assert_ne!(left["digest"], right["digest"]);
        }
    }
}

#[test]
fn adapter_capture_null_value_and_absent_value_stay_distinct() {
    let mut capture = image(CborValue::Null);
    let mut values = capture.values.to_vec();
    values.push(StateEntry {
        error: Some(vosu_plugin_api::ErrorInfo {
            code: "unavailable".into(),
            message: "not published".into(),
        }),
        ..entry("/play/combo", None)
    });
    capture.values = Arc::from(values);
    let envelope = envelope(&sample(capture));
    assert_eq!(cbor(&envelope, 0), [0xf6]);
    assert_eq!(envelope["entries"][0]["error"], Value::Null);
    assert_eq!(envelope["entries"][1]["value_cbor_b64"], Value::Null);
    assert_eq!(
        envelope["entries"][1]["error"],
        json!({"code": "unavailable", "message": "not published"})
    );
    for entry in envelope["entries"].as_array().unwrap() {
        assert_eq!(keys(entry), ["ns", "pointer", "value_cbor_b64", "error"]);
    }
}

#[test]
fn adapter_capture_session_ordinals_preserve_equality_and_roster_order() {
    let first = session("host-session-aaaa", 9_000_000_001, Some(false));
    let second = session("host-session-bbbb", 9_000_000_002, None);
    let mut again = first.clone();
    again.generation = 2;
    let mut capture = image(CborValue::Integer(1.into()));
    capture.roster.sessions = Arc::from([first, second.clone(), again]);
    capture.session = Some(second);
    let out = envelope(&sample(capture.clone()));
    let roster = out["roster"]["sessions"].as_array().unwrap();
    let ordinals: Vec<_> = roster.iter().map(|s| s["ordinal"].clone()).collect();
    let instances: Vec<_> = roster.iter().map(|s| s["owner_instance"].clone()).collect();
    // Equal ids share an ordinal; distinct ids never do; order is the roster's.
    assert_eq!(ordinals, [json!(0), json!(1), json!(0)]);
    assert_eq!(instances, [json!(0), json!(1), json!(0)]);
    assert_eq!(roster[2]["generation"], 2);
    assert_eq!(
        out["session"],
        json!({"ordinal": 1, "generation": 1, "owner": "vosu.osu", "owner_instance": 1,
               "game": "osu", "variant": "lazer", "foreground": null})
    );
    assert_eq!(roster[0]["foreground"], false);

    // A selected session missing from the roster gets the next fresh ordinal.
    capture.session = Some(session("host-session-cccc", 9_000_000_003, Some(true)));
    let envelope = envelope(&sample(capture));
    assert_eq!(envelope["session"]["ordinal"], 2);
    assert_eq!(envelope["session"]["owner_instance"], 2);
    let text = envelope.to_string();
    for private in ["host-session-", "9000000001", "9000000002", "9000000003"] {
        assert!(!text.contains(private), "{private} leaked: {text}");
    }
}

#[test]
fn adapter_capture_envelope_has_no_process_or_root_identity() {
    let mut capture = image(CborValue::Integer(1.into()));
    let private = session("pid-31337:/home/owner/.local/share/osu", 31337, Some(true));
    capture.session = Some(private.clone());
    capture.roster.sessions = Arc::from([private]);
    let bytes = sample(capture).envelope().unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    for leaked in ["31337", "/home/owner", "pid"] {
        assert!(!text.contains(leaked), "{leaked} leaked: {text}");
    }
    let envelope: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        keys(&envelope),
        [
            "format",
            "schema",
            "provider_generation",
            "session",
            "focus_revision",
            "roster",
            "bus_revision",
            "namespace_revisions",
            "entries",
            "derived",
            "http",
            "digest"
        ]
    );
    let session = [
        "ordinal",
        "generation",
        "owner",
        "owner_instance",
        "game",
        "variant",
        "foreground",
    ];
    assert_eq!(keys(&envelope["session"]), session);
    assert_eq!(keys(&envelope["roster"]), ["revision", "sessions"]);
    assert_eq!(keys(&envelope["roster"]["sessions"][0]), session);
    assert_eq!(keys(&envelope["derived"]), ["revision", "status"]);
    assert_eq!(keys(&envelope["http"]), ["kind", "status", "body_utf8"]);
    assert_eq!(envelope["format"], FORMAT);
    assert_eq!(envelope["schema"], "v2");
    assert_eq!(envelope["namespace_revisions"], json!([["vosu.osu", 9]]));
    assert_eq!(
        (
            envelope["focus_revision"].clone(),
            envelope["bus_revision"].clone()
        ),
        (json!(3), json!(9))
    );
}

#[test]
fn adapter_capture_digest_is_the_selection_only_and_reproducible() {
    let base = sample(image(CborValue::Integer(1.into())));
    let digest = |sample: &AdapterCaptureSample| envelope(sample)["digest"].clone();
    let mut other = base.clone();
    other.provider_generation = 99;
    other.derived_revision = 7;
    other.derived = DerivedStatus::Pending;
    other.http = CachedJson::NotReady;
    assert_eq!(digest(&base), digest(&other));
    let mut focus = base.clone();
    focus.capture.focus_revision += 1;
    assert_ne!(digest(&base), digest(&focus));
    let mut roster = base.clone();
    roster.capture.roster.revision += 1;
    assert_ne!(digest(&base), digest(&roster));
    let changed = sample(with_value(&base.capture, CborValue::Integer(2.into())));
    assert_ne!(digest(&base), digest(&changed));

    // The documented recomputation over the parsed envelope reproduces it.
    let envelope = envelope(&base);
    let selection: serde_json::Map<String, Value> = [
        "session",
        "focus_revision",
        "roster",
        "bus_revision",
        "namespace_revisions",
        "entries",
    ]
    .into_iter()
    .map(|key| (key.to_owned(), envelope[key].clone()))
    .collect();
    let recomputed = super::digest(&serde_json::to_vec(&selection).unwrap());
    assert_eq!(envelope["digest"], recomputed);
    assert!(recomputed.starts_with("sha256:") && recomputed.len() == 7 + 64);
}

#[test]
fn adapter_capture_schema_query_names_are_exact() {
    for (name, schema) in SCHEMAS {
        assert_eq!(schema_from_query(name), Some(schema));
        assert_eq!(schema_name(schema), name);
    }
    for near in [
        "",
        "V2",
        "v2 ",
        " v2",
        "v2/",
        "precise",
        "v2_precise",
        "json/v2",
    ] {
        assert_eq!(schema_from_query(near), None, "{near:?}");
    }
}

// Real envelopes for the Python `golden-capture` fake vosu harness server
// (tools/live-parity/test_live_parity_golden_capture.py). They come from this
// encoder through `capture_for_harness`, never from hand-typed JSON. The
// streamCompanion stand-in releases its derived work on every read, so its
// generation and derived revision advance per read as the real provider's do.
const FAKE_ENVELOPES: &str = "tools/live-parity/testdata/golden-capture-envelopes.json";
const FAKE_READS: usize = 3;
/// (barrier, recorded client state); `None` is the not-ready no-client image.
const FAKE_BARRIERS: [(&str, Option<i64>); 5] = [
    ("no-client", None),
    ("menu", Some(0)),
    ("play", Some(1)),
    ("paused", Some(2)),
    ("result", Some(3)),
];

/// Encodes the recorded client state as the barrier-bearing part of a v2-like
/// body. With `releases` (a client session has chart work), every read's
/// retained derived work is released afterwards, which advances its revision.
struct StateEcho {
    derived: Option<Arc<watch::Sender<u64>>>,
    releases: bool,
}
struct StateView {
    state: i128,
    revision: u64,
}
struct Released(Arc<watch::Sender<u64>>);
impl Drop for Released {
    fn drop(&mut self) {
        self.0.send_modify(|revision| *revision += 1);
    }
}
impl Projection<Filter, Bytes> for StateEcho {
    fn selectors(&self) -> Vec<Selector> {
        Echo::default().selectors()
    }
    fn demand_selectors(&self, _: &[Filter]) -> Vec<Selector> {
        self.selectors()
    }
    fn decode(
        &self,
        capture: CapturedSelection,
    ) -> Result<Arc<dyn ProjectionView<Filter, Bytes>>, ProjectionError> {
        Ok(Arc::new(StateView {
            state: score(&capture),
            revision: self.revision(),
        }))
    }
    fn revision(&self) -> u64 {
        self.derived.as_ref().map_or(0, |derived| *derived.borrow())
    }
    fn changes(&self) -> Option<watch::Receiver<u64>> {
        self.derived.as_ref().map(|derived| derived.subscribe())
    }
    fn retain(&self, _: &[Filter]) -> Option<Box<dyn Send + Sync>> {
        let derived = self.derived.clone().filter(|_| self.releases)?;
        Some(Box::new(Released(derived)))
    }
}
impl ProjectionView<Filter, Bytes> for StateView {
    fn revision(&self) -> u64 {
        self.revision
    }
    fn encode(&self, _: &[Filter]) -> Result<Bytes, String> {
        let (number, name, paused) = match self.state {
            0 => (0, "menu", false),
            1 => (2, "play", false),
            2 => (2, "play", true),
            3 => (7, "resultScreen", false),
            other => return Err(format!("unknown recorded state {other}")),
        };
        Ok(Bytes::from(
            json!({"state": {"number": number, "name": name}, "game": {"paused": paused}})
                .to_string(),
        ))
    }
}

fn fake_vosu_envelopes() -> Value {
    let mut barriers = serde_json::Map::new();
    for (barrier, state) in FAKE_BARRIERS {
        let mut schemas = serde_json::Map::new();
        for (name, schema) in SCHEMAS {
            let mut image = image(CborValue::Integer(state.unwrap_or(0).into()));
            if state.is_none() {
                image.session = None;
                image.roster.sessions = Arc::from([]);
                image.values = Arc::from([entry("/play/score", None)]);
            }
            let bus = Bus::new(image, false);
            let adapter = Arc::new(StateEcho {
                derived: (schema == Schema::StreamCompanion).then(|| Arc::new(watch::channel(1).0)),
                releases: state.is_some(),
            });
            let provider = BusStateProvider::new(schema, bus, adapter, Duration::from_secs(3600));
            let reads = (0..FAKE_READS)
                .map(|_| envelope(&provider.capture_for_harness().unwrap()))
                .collect();
            schemas.insert(name.to_owned(), Value::Array(reads));
        }
        barriers.insert(barrier.to_owned(), Value::Object(schemas));
    }
    json!({
        "format": "vosu-golden-capture-fake-envelopes-v1",
        "generator": "src/server/adapter_capture/tests.rs export_golden_capture_fake_envelopes",
        "reads_per_schema": FAKE_READS,
        "barriers": barriers,
    })
}

fn fake_envelopes_text() -> String {
    serde_json::to_string_pretty(&fake_vosu_envelopes()).unwrap() + "\n"
}

#[test]
fn adapter_capture_fake_envelopes_follow_the_provider_generation_rules() {
    let value = fake_vosu_envelopes();
    for (barrier, schemas) in value["barriers"].as_object().unwrap() {
        for (schema, reads) in schemas.as_object().unwrap() {
            let reads = reads.as_array().unwrap();
            let generations: Vec<_> = reads
                .iter()
                .map(|r| r["provider_generation"].clone())
                .collect();
            let advancing = schema == "sc" && barrier != "no-client";
            for pair in reads.windows(2) {
                assert_eq!(pair[0]["digest"], pair[1]["digest"], "{barrier}/{schema}");
                assert_eq!(pair[0]["http"], pair[1]["http"], "{barrier}/{schema}");
                assert_eq!(
                    pair[0]["derived"]["status"], pair[1]["derived"]["status"],
                    "{barrier}/{schema}"
                );
                let step = pair[1]["provider_generation"].as_u64().unwrap()
                    - pair[0]["provider_generation"].as_u64().unwrap();
                assert_eq!(
                    step,
                    u64::from(advancing),
                    "{barrier}/{schema} {generations:?}"
                );
            }
            let kind = if barrier == "no-client" {
                "not_ready"
            } else {
                "ready"
            };
            assert_eq!(reads[0]["http"]["kind"], kind, "{barrier}/{schema}");
        }
    }
}

#[test]
fn adapter_capture_fake_envelopes_are_current() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(FAKE_ENVELOPES);
    let committed = std::fs::read_to_string(&path).unwrap();
    assert!(
        committed == fake_envelopes_text(),
        "{FAKE_ENVELOPES} is stale; regenerate it with `cargo test --locked --features \
         test-harness -p vosu --lib export_golden_capture_fake_envelopes -- --ignored`"
    );
}

#[test]
#[ignore = "writes the committed golden-capture fake envelopes"]
fn export_golden_capture_fake_envelopes() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(FAKE_ENVELOPES);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, fake_envelopes_text()).unwrap();
}
