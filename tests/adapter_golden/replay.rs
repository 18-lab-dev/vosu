//! Real-host replay of recorded adapter selections (M4b §3.6).
//!
//! A bundled fixture game with the actual osu! manifest, schemas and grants
//! replaces the production game provider in the test production composition.
//! For each recorded roster session, in roster order, the host sees a
//! synthetic process record; the game identifies that session, and for the
//! selected session it declares the replay roots and publishes the recorded
//! entries plus the replay resource references in one `PublishBatchWriter`
//! batch through `VOSU_OP_PUBLISH_BATCH`. The host validates that batch
//! against the manifest grants and commits it atomically, or rejects all of
//! it. The production adapters, `adapter_routes` registrations and listener
//! then serve the replayed bus. Nothing writes the store, injects provider
//! bytes or bypasses a grant.
//!
//! Before a replay is used, each provider's own harness dump must select
//! what the envelope recorded. Route bytes are never compared here. Callers
//! read them from the real listener.
use super::{
    assets::{Materialized, ReplayAssets},
    envelope::{
        Envelope, RecordedEntry, RecordedSession, contains_reference, increasing,
        reference_position, tokens, tree_position,
    },
};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc},
    time::{Duration, Instant},
};
use tempfile::TempDir;
use vosu::{
    runtime::{
        TestGame, TestProductionComposition, TestProductionCompositionOptions,
        start_test_production_composition,
    },
    server::{
        adapter_capture::DerivedStatus,
        routes::ProviderFuture,
        runtime::ListenTarget,
        service::{UpdateRouteResult, UpdaterHandoff},
        websocket::Schema,
    },
};
use vosu_core::{
    config::ConfigStore,
    plugin::{BundledPlugin, ProcessRecord, SyntheticForegroundSource},
};
use vosu_plugin_api::{CborValue, vosu_status};

pub const WAIT: Duration = Duration::from_secs(15);
/// The manifest id the replay game publishes and identifies as.
const OWNER: &str = "vosu.osu";
const PID_BASE: u32 = 760_100;
/// Replays share process-global game setup; one runs at a time.
static SERIAL: Mutex<()> = Mutex::new(());

/// What the game reported for one attached session.
#[derive(Clone, Debug)]
pub struct Attached {
    pub ordinal: usize,
    pub identify: vosu_status,
    pub roots: Vec<vosu_status>,
    /// `None` when the session had nothing to publish.
    pub publish: Option<vosu_status>,
}

pub struct Replay {
    pub address: SocketAddr,
    pub attached: Vec<Attached>,
    composition: Option<TestProductionComposition>,
    config: Arc<ConfigStore>,
    runtime: tokio::runtime::Runtime,
    assets: Materialized,
    _dirs: TempDir,
    _serial: MutexGuard<'static, ()>,
}

impl Replay {
    /// Replays `envelopes` (one coherent selection, one or more schemas) and
    /// requires every identify, root declaration and publish to succeed and
    /// every provider dump to match its recording.
    pub fn start(envelopes: &[Envelope], assets: &ReplayAssets) -> Result<Self, String> {
        let replay = Self::launch(envelopes, assets)?;
        for attached in &replay.attached {
            let ordinal = attached.ordinal;
            if attached.identify != vosu_status::VOSU_OK {
                return Err(format!(
                    "session {ordinal} identify: {:?}",
                    attached.identify
                ));
            }
            if let Some(status) = attached.roots.iter().find(|s| **s != vosu_status::VOSU_OK) {
                return Err(format!("session {ordinal} root declaration: {status:?}"));
            }
            if let Some(status) = attached.publish.filter(|s| *s != vosu_status::VOSU_OK) {
                return Err(format!("session {ordinal} publish batch: {status:?}"));
            }
        }
        for envelope in envelopes {
            replay.settle(envelope)?;
        }
        Ok(replay)
    }

    /// Starts the composition and replays without judging the host's
    /// answers, so a caller can inspect a rejected batch.
    pub fn launch(envelopes: &[Envelope], assets: &ReplayAssets) -> Result<Self, String> {
        let recorded = Recorded::merge(envelopes)?;
        let materialized = assets.materialize()?;
        let publish = recorded.publications(assets)?;
        if recorded.session.is_none() && !(publish.is_empty() && assets.roots.is_empty()) {
            return Err("replay assets need a selected session".into());
        }
        let selected = recorded.session.as_ref().map(|s| s.ordinal);
        let foreground = recorded
            .roster
            .iter()
            .position(|s| s.foreground == Some(true));
        // Focus falls to the foreground owner, else to the first session to
        // attach. Another selected session needs `focus_priority`.
        let priority = selected.filter(|s| *s != foreground.unwrap_or(0));
        let serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let (events, attached) = mpsc::sync_channel(recorded.roster.len().max(1));
        *game::SETUP.lock().unwrap() = Some(game::Plan {
            sessions: recorded
                .roster
                .iter()
                .map(|s| game::Session {
                    game: s.game.clone(),
                    variant: s.variant.clone(),
                    focus_priority: priority == Some(s.ordinal),
                })
                .collect(),
            selected,
            roots: materialized
                .roots
                .iter()
                .map(|(slot, path)| (slot.clone(), path.to_string_lossy().into_owned()))
                .collect(),
            resources: assets
                .resources
                .iter()
                .map(|resource| game::Resource {
                    kind: resource.kind.clone(),
                    name: resource.name.clone(),
                    content_id: assets.content_id(resource),
                    slot: resource.slot.clone(),
                    path: resource.path.clone(),
                })
                .collect(),
            publish,
            events,
        });
        let dirs = TempDir::new().map_err(|e| format!("replay directory: {e}"))?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| format!("replay runtime: {e}"))?;
        // A recorded `null` foreground is what a host without an
        // OS-foreground source reports (`available()` false, as production
        // off Windows); a recorded `true`/`false` needs one. The recording
        // decides, never the platform this replay runs on.
        let synthetic = Arc::new(if recorded.foreground_available() {
            SyntheticForegroundSource::new(None)
        } else {
            SyntheticForegroundSource::unavailable()
        });
        let root = dirs.path().to_path_buf();
        let started = runtime.block_on(compose(root, synthetic.clone()));
        let created = game::SETUP.lock().unwrap().take().is_none();
        let (config, composition) = started?;
        let mut replay = Self {
            address: composition.address(),
            attached: Vec::new(),
            composition: Some(composition),
            config,
            runtime,
            assets: materialized,
            _dirs: dirs,
            _serial: serial,
        };
        if !created {
            return Err("the composition did not create the replay game".into());
        }
        let host = replay.composition().plugins().clone();
        for ordinal in 0..recorded.roster.len() {
            host.process_appeared(process(ordinal));
            let event = attached
                .recv_timeout(WAIT)
                .map_err(|_| format!("session {ordinal} was never attached"))?;
            if event.ordinal != ordinal {
                return Err(format!("session {} attached as {ordinal}", event.ordinal));
            }
            replay.attached.push(event);
        }
        if let Some(ordinal) = foreground {
            synthetic.set(Some(pid(ordinal)));
            host.foreground_changed();
        }
        Ok(replay)
    }

    /// The real temporary directory of each declared replay root slot.
    pub fn roots(&self) -> &[(String, PathBuf)] {
        &self.assets.roots
    }

    /// The selected-entry dump of the mounted `schema` provider.
    pub fn dump(&self, schema: Schema) -> Result<Envelope, String> {
        Envelope::parse(&self.dump_bytes(schema)?)
    }

    /// The raw envelope bytes the harness endpoint would serve for `schema`.
    pub fn dump_bytes(&self, schema: Schema) -> Result<Vec<u8>, String> {
        let _runtime = self.runtime.enter();
        self.composition().adapter_capture(schema)?.envelope()
    }

    fn composition(&self) -> &TestProductionComposition {
        self.composition.as_ref().expect("live replay composition")
    }

    /// Waits for derived work to reach the recorded status, then checks the
    /// provider's own dump against the recording.
    fn settle(&self, envelope: &Envelope) -> Result<(), String> {
        let _runtime = self.runtime.enter();
        let deadline = Instant::now() + WAIT;
        let sample = loop {
            let sample = self.composition().adapter_capture(envelope.schema)?;
            let status = match sample.derived {
                DerivedStatus::Ready => "ready",
                DerivedStatus::Pending => "pending",
                DerivedStatus::None => "none",
            };
            if status == envelope.derived {
                break sample;
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "{}: derived status stayed {status}, recorded {}",
                    envelope.name(),
                    envelope.derived
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        envelope.check_replayed(&Envelope::parse(&sample.envelope()?)?)
    }
}

impl Drop for Replay {
    /// Joins the listener, hubs, chart worker and plugin instances.
    fn drop(&mut self) {
        let Some(composition) = self.composition.take() else {
            return;
        };
        let config = self.config.clone();
        let stopped = self.runtime.block_on(async move {
            composition.shutdown().await;
            config.shutdown().await
        });
        if !std::thread::panicking() {
            stopped.expect("replay configuration shutdown");
        }
    }
}

async fn compose(
    root: PathBuf,
    foreground: Arc<SyntheticForegroundSource>,
) -> Result<(Arc<ConfigStore>, TestProductionComposition), String> {
    let config = Arc::new(
        ConfigStore::initialize(root.join("config/vosu.toml"))
            .await
            .map_err(|e| format!("replay configuration: {e}"))?,
    );
    let started = start_test_production_composition(TestProductionCompositionOptions {
        target: ListenTarget::new("127.0.0.1", 0),
        config: config.clone(),
        static_root: root.join("static"),
        settings_root: root.join("settings"),
        cache_root: root.join("cache"),
        logs_root: root.join("logs"),
        updater: Arc::new(NoUpdates),
        network: network()?,
        game: Some(TestGame {
            plugin: BundledPlugin {
                manifest: vosu_osu::MANIFEST,
                settings_schema: Some(vosu_osu::SETTINGS_SCHEMA),
                namespace_schemas: &[vosu_osu::OSU_NAMESPACE_SCHEMA],
                action_schemas: vosu_osu::ACTION_SCHEMAS,
                vtable: game::vosu_plugin_vtable,
            },
            foreground,
        }),
    })
    .await;
    match started {
        Ok(composition) => Ok((config, composition)),
        Err(error) => {
            config.shutdown().await.ok();
            Err(format!("replay composition: {error}"))
        }
    }
}

/// A reserved network that nothing in a replay contacts.
fn network() -> Result<vosu_net::TestNetworkConfig, String> {
    let endpoint = |path: &str| format!("http://replay.invalid:9/{path}");
    vosu_net::TestNetworkConfig::new(
        Arc::new(vosu_net::streaming::SystemResolver),
        vec!["replay.invalid".to_owned()],
        vec![SocketAddr::from(([127, 0, 0, 1], 9))],
    )
    .and_then(|network| {
        network.with_endpoints(vosu_net::TestNetworkEndpoints {
            catalog: endpoint("catalog"),
            release: endpoint("release"),
            offsets_base: endpoint("offsets/"),
        })
    })
    .map_err(|e| format!("replay network: {e}"))
}

struct NoUpdates;
impl UpdaterHandoff for NoUpdates {
    fn run_update(&self, _: bool) -> ProviderFuture<'_, Result<UpdateRouteResult, String>> {
        Box::pin(async { Err("replays have no updater".into()) })
    }
}

fn pid(ordinal: usize) -> u32 {
    PID_BASE + u32::try_from(ordinal).expect("bounded roster")
}

/// A synthetic process the osu! manifest matches. It carries no real path.
fn process(ordinal: usize) -> ProcessRecord {
    ProcessRecord {
        pid: pid(ordinal),
        name: "osu!.exe".into(),
        path: "/vosu-replay/osu!.exe".into(),
        command_line: "/vosu-replay/osu!.exe".into(),
        argv: Some(vec!["/vosu-replay/osu!.exe".into()]),
        bitness: vosu_plugin_api::Bitness::X64,
        start_time: 1,
    }
}

/// The recorded selection all replayed envelopes agree on.
struct Recorded {
    session: Option<RecordedSession>,
    roster: Vec<RecordedSession>,
    entries: Vec<RecordedEntry>,
}

impl Recorded {
    fn merge(envelopes: &[Envelope]) -> Result<Self, String> {
        let first = envelopes.first().ok_or("nothing to replay")?;
        let mut entries: Vec<RecordedEntry> = Vec::new();
        for envelope in envelopes {
            if (&envelope.session, &envelope.roster) != (&first.session, &first.roster) {
                return Err(format!(
                    "{} records another session or roster than {}",
                    envelope.name(),
                    first.name()
                ));
            }
            if envelope.derived == "pending" {
                return Err(format!(
                    "{}: pending derived work is not a stable replay input",
                    envelope.name()
                ));
            }
            for entry in &envelope.entries {
                match entries
                    .iter()
                    .find(|e| (&e.ns, &e.pointer) == (&entry.ns, &entry.pointer))
                {
                    Some(known) if known != entry => {
                        return Err(format!(
                            "{}{} is recorded differently by {}",
                            entry.ns,
                            entry.pointer,
                            envelope.name()
                        ));
                    }
                    Some(_) => {}
                    None => entries.push(entry.clone()),
                }
            }
        }
        let recorded = Self {
            session: first.session.clone(),
            roster: first.roster.clone(),
            entries,
        };
        recorded.validate()?;
        Ok(recorded)
    }

    /// Rejects facts this host cannot reproduce rather than approximating.
    fn validate(&self) -> Result<(), String> {
        for (index, session) in self.roster.iter().enumerate() {
            if session.ordinal != index {
                return Err(format!(
                    "roster session {index} has ordinal {}",
                    session.ordinal
                ));
            }
            if session.owner != OWNER || session.owner_instance != 0 {
                return Err(format!(
                    "roster session {index} is owned by {}#{}, not the replay game {OWNER}",
                    session.owner, session.owner_instance
                ));
            }
        }
        if self
            .roster
            .windows(2)
            .any(|pair| pair[0].foreground.is_some() != pair[1].foreground.is_some())
        {
            return Err(
                "roster foreground facts are partly unknown; one host source cannot \
                 report both"
                    .into(),
            );
        }
        if !increasing(&self.roster) {
            return Err("roster generations do not follow roster order".into());
        }
        if self
            .roster
            .iter()
            .filter(|s| s.foreground == Some(true))
            .count()
            > 1
        {
            return Err("more than one roster session owns the foreground".into());
        }
        match &self.session {
            None if !self.roster.is_empty() => {
                Err("a nonempty roster always has a selected session".into())
            }
            Some(session) if self.roster.get(session.ordinal) != Some(session) => {
                Err("the selected session is not its roster entry".into())
            }
            _ => Ok(()),
        }
    }

    /// Whether the recording host had an OS-foreground source. An empty
    /// roster records no fact either way; the source is then irrelevant.
    fn foreground_available(&self) -> bool {
        self.roster.first().is_none_or(|s| s.foreground.is_some())
    }

    /// The batch for the selected session: every recorded value, a resource
    /// reference for each replay resource, and nothing below a published
    /// ancestor (the host rejects overlapping pointers in one batch).
    fn publications(&self, assets: &ReplayAssets) -> Result<Vec<game::Publish>, String> {
        let mut publish = Vec::new();
        for entry in &self.entries {
            let at = format!("{}{}", entry.ns, entry.pointer);
            let path = tokens(&entry.pointer).ok_or_else(|| format!("{at}: malformed pointer"))?;
            let value = entry
                .value
                .as_ref()
                .map(|bytes| vosu_plugin_api::decode::<CborValue>(bytes))
                .transpose()
                .map_err(|e| format!("{at}: {e}"))?;
            if reference_position(&entry.ns, &entry.pointer) {
                if tree_position(&entry.ns, &entry.pointer)
                    && matches!(value, Some(CborValue::Map(_)))
                {
                    return Err(format!(
                        "{at}: adapter selections never include tree refs; extend replay if \
                         an adapter starts selecting one"
                    ));
                }
                let asset = (entry.ns == "vosu.chart")
                    .then(|| assets.resource(&path[1]))
                    .flatten();
                match (value, asset) {
                    (None, None) => {}
                    (Some(CborValue::Null), None) => publish.push(game::Publish {
                        ns: entry.ns.clone(),
                        pointer: entry.pointer.clone(),
                        value: game::Value::Cbor(entry.value.clone().unwrap()),
                    }),
                    (Some(CborValue::Map(_)), Some(asset)) => publish.push(game::Publish {
                        ns: entry.ns.clone(),
                        pointer: entry.pointer.clone(),
                        value: game::Value::Resource(asset.kind.clone()),
                    }),
                    (Some(CborValue::Map(_)), None) => {
                        return Err(format!(
                            "{at} needs a replay asset; recorded paths are never file authority"
                        ));
                    }
                    (_, Some(_)) => {
                        return Err(format!(
                            "{at}: a replay asset contradicts the recorded value"
                        ));
                    }
                    (Some(_), None) => {
                        return Err(format!("{at}: unexpected reference value"));
                    }
                }
                continue;
            }
            if let Some(value) = value {
                if contains_reference(&value) {
                    return Err(format!("{at}: a reference outside a reference position"));
                }
                publish.push(game::Publish {
                    ns: entry.ns.clone(),
                    pointer: entry.pointer.clone(),
                    value: game::Value::Cbor(entry.value.clone().unwrap()),
                });
            }
        }
        for resource in &assets.resources {
            let pointer = format!("/resources/{}", resource.kind);
            if !self
                .entries
                .iter()
                .any(|e| e.ns == "vosu.chart" && e.pointer == pointer)
            {
                publish.push(game::Publish {
                    ns: "vosu.chart".into(),
                    pointer,
                    value: game::Value::Resource(resource.kind.clone()),
                });
            }
        }
        let paths: Vec<(String, Vec<String>)> = publish
            .iter()
            .map(|p| (p.ns.clone(), tokens(&p.pointer).expect("checked pointer")))
            .collect();
        let covered = |index: usize| {
            let (ns, path) = &paths[index];
            paths.iter().enumerate().any(|(other, (other_ns, prefix))| {
                other != index
                    && other_ns == ns
                    && prefix.len() < path.len()
                    && path.starts_with(prefix)
            })
        };
        let keep: Vec<bool> = (0..publish.len()).map(|i| !covered(i)).collect();
        let mut keep = keep.into_iter();
        publish.retain(|_| keep.next().expect("one flag per publication"));
        Ok(publish)
    }
}

/// The bundled replay game: the osu! manifest's identity and grants, with no
/// memory reader. It only identifies, declares replay roots, publishes and
/// resolves replay resources.
mod game {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use vosu_plugin_api::{
        ConcurrentPlugin, ErrorInfo, HostServices, Init, Message, PluginError, PublishBatchWriter,
        PublishValueRef, ResourceRef, ResourceResolved, RootDeclare, SessionIdentify, StreamChunk,
        VtableHostServices, vosu_opcode,
    };

    pub(super) static SETUP: Mutex<Option<Plan>> = Mutex::new(None);

    pub(super) struct Plan {
        pub sessions: Vec<Session>,
        pub selected: Option<usize>,
        pub roots: Vec<(String, String)>,
        pub resources: Vec<Resource>,
        pub publish: Vec<Publish>,
        pub events: mpsc::SyncSender<Attached>,
    }
    pub(super) struct Session {
        pub game: String,
        pub variant: String,
        pub focus_priority: bool,
    }
    pub(super) struct Resource {
        pub kind: String,
        pub name: String,
        pub content_id: String,
        pub slot: String,
        pub path: String,
    }
    pub(super) struct Publish {
        pub ns: String,
        pub pointer: String,
        pub value: Value,
    }
    pub(super) enum Value {
        /// Recorded canonical CBOR, published as `VOSU_VALUE_CBOR`.
        Cbor(Vec<u8>),
        /// A replay resource of this kind, published as a resource reference.
        Resource(String),
    }

    pub struct ReplayGame {
        host: VtableHostServices,
        plan: Plan,
        next: AtomicUsize,
        selected: Mutex<Option<u64>>,
    }

    fn key(kind: &str) -> CborValue {
        CborValue::Text(format!("replay:{kind}"))
    }
    fn not_found() -> ErrorInfo {
        ErrorInfo {
            code: "not_found".into(),
            message: "not a replay resource".into(),
        }
    }

    impl ReplayGame {
        fn attach(&self, handle: u64) -> Attached {
            let ordinal = self.next.fetch_add(1, Ordering::AcqRel);
            let Some(session) = self.plan.sessions.get(ordinal) else {
                // An unplanned session is never identified or published.
                return Attached {
                    ordinal,
                    identify: vosu_status::VOSU_ERR_INVALID_ARGUMENT,
                    roots: Vec::new(),
                    publish: None,
                };
            };
            let identify = self.host.send(
                &Message::SessionIdentify(SessionIdentify {
                    handle,
                    game: session.game.clone(),
                    variant: session.variant.clone(),
                    name: "osu! (replay)".into(),
                    version: "replay".into(),
                    focus_priority: session.focus_priority,
                }),
                0,
            );
            if self.plan.selected != Some(ordinal) {
                return Attached {
                    ordinal,
                    identify,
                    roots: Vec::new(),
                    publish: None,
                };
            }
            *self.selected.lock().unwrap() = Some(handle);
            let roots = self
                .plan
                .roots
                .iter()
                .map(|(slot, path)| {
                    self.host.send(
                        &Message::RootDeclare(RootDeclare {
                            handle: Some(handle),
                            name: slot.clone(),
                            path: path.clone(),
                        }),
                        0,
                    )
                })
                .collect();
            Attached {
                ordinal,
                identify,
                roots,
                publish: (!self.plan.publish.is_empty()).then(|| self.publish(handle)),
            }
        }

        fn publish(&self, handle: u64) -> vosu_status {
            let mut writer = PublishBatchWriter::new();
            writer.begin(handle);
            for entry in &self.plan.publish {
                match &entry.value {
                    Value::Cbor(bytes) => {
                        writer.push(&entry.ns, &entry.pointer, PublishValueRef::Cbor(bytes));
                    }
                    Value::Resource(kind) => {
                        let resource = self
                            .plan
                            .resources
                            .iter()
                            .find(|r| &r.kind == kind)
                            .expect("planned resource");
                        let reference = vosu_plugin_api::encode(&ResourceRef {
                            kind: kind.clone(),
                            key: key(kind),
                            name: resource.name.clone(),
                            content_id: CborValue::Text(resource.content_id.clone()),
                        })
                        .expect("resource reference encodes");
                        writer.push(
                            &entry.ns,
                            &entry.pointer,
                            PublishValueRef::ResourceRef(&reference),
                        );
                    }
                }
            }
            let mut bytes = Vec::new();
            if writer.finish_into(&mut bytes).is_err() {
                return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
            }
            self.host
                .send_flat(vosu_opcode::VOSU_OP_PUBLISH_BATCH, 0, 0, &bytes)
        }

        fn resolve(&self, handle: u64, kind: &str, requested: &CborValue) -> ResourceResolved {
            let selected = *self.selected.lock().unwrap() == Some(handle);
            match self
                .plan
                .resources
                .iter()
                .find(|r| selected && r.kind == kind && key(&r.kind) == *requested)
            {
                Some(resource) => ResourceResolved::Path {
                    root: resource.slot.clone(),
                    path: resource.path.clone(),
                },
                None => ResourceResolved::Error { error: not_found() },
            }
        }
    }

    impl ConcurrentPlugin for ReplayGame {
        fn create(host: VtableHostServices, _: Init) -> Result<Self, PluginError> {
            let plan = SETUP
                .lock()
                .unwrap()
                .take()
                .ok_or(PluginError(vosu_status::VOSU_ERR_INVALID_ARGUMENT))?;
            Ok(Self {
                host,
                plan,
                next: AtomicUsize::new(0),
                selected: Mutex::new(None),
            })
        }

        fn on_message(&self, message: Message, correlation: u64) -> Result<(), PluginError> {
            let reply = match message {
                Message::SessionAttached(attached) => {
                    let event = self.attach(attached.handle);
                    self.plan
                        .events
                        .try_send(event)
                        .map_err(|_| PluginError(vosu_status::VOSU_ERR_BACKEND))?;
                    return Ok(());
                }
                Message::ResourceResolve(request) => {
                    self.resolve(request.handle, &request.kind, &request.key)
                }
                Message::TreeList(_) | Message::TreeLookup(_) => {
                    ResourceResolved::Error { error: not_found() }
                }
                Message::StreamRead(_) => {
                    self.host.send(
                        &Message::StreamChunk(StreamChunk::Error { error: not_found() }),
                        correlation,
                    );
                    return Ok(());
                }
                _ => return Ok(()),
            };
            self.host
                .send(&Message::ResourceResolved(reply), correlation);
            Ok(())
        }
    }

    vosu_plugin_api::export_plugin!(bundled concurrent ReplayGame);
}
