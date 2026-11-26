#![expect(
    dead_code,
    reason = "Shared integration and benchmark fixture; each caller uses a subset"
)]
mod chart;
#[path = "../api_v1/support.rs"]
pub mod host;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard, mpsc},
    time::Duration,
};
use vosu::server::bus_provider::BusStateProvider;
use vosu_core::plugin::PluginInstanceId;
use vosu_core::server::{
    routes::{CachedJson, StateJsonProvider, StateRoute},
    websocket::Schema,
};
use vosu_osu::{
    fixtures::{Publisher, Roster},
    project::SessionFacts,
};
use vosu_osu_model::{enums::ClientType, launch::InstanceSettings};
use vosu_plugin_api::{
    HostServices, Init, Message, PluginError, SessionIdentify, VtableHostServices, vosu_status,
};
use vosu_surface::projection::Projection;

static SERIAL: Mutex<()> = Mutex::new(());
static SETUP: Mutex<Option<Setup>> = Mutex::new(None);
type Publishers = Arc<Mutex<BTreeMap<u64, Arc<Publisher>>>>;
struct Setup {
    source: ClientType,
    chart: Option<chart::Chart>,
    attached: mpsc::SyncSender<(u64, VtableHostServices)>,
    publishers: Publishers,
}
struct Game {
    host: VtableHostServices,
    setup: Setup,
}
impl vosu_plugin_api::Plugin for Game {
    fn create(host: VtableHostServices, _: Init) -> Result<Self, PluginError> {
        Ok(Self {
            host,
            setup: SETUP
                .lock()
                .unwrap()
                .take()
                .expect("fixture create consumes its own setup"),
        })
    }
    fn on_message(&mut self, message: Message, correlation: u64) -> Result<(), PluginError> {
        match message {
            Message::SessionAttached(attached) => {
                let status = self.host.send(
                    &Message::SessionIdentify(SessionIdentify {
                        handle: attached.handle,
                        game: "osu".into(),
                        variant: if self.setup.source == ClientType::Lazer {
                            "lazer"
                        } else {
                            "stable"
                        }
                        .into(),
                        name: "Fixture osu!".into(),
                        version: "2026-fixture".into(),
                        focus_priority: false,
                    }),
                    0,
                );
                assert_eq!(status, vosu_status::VOSU_OK);
                if let Some(chart) = &self.setup.chart {
                    chart.attach(&self.host, attached.handle);
                }
                self.setup
                    .attached
                    .try_send((attached.handle, self.host))
                    .unwrap();
            }
            message @ (Message::ResourceResolve(_)
            | Message::TreeList(_)
            | Message::TreeLookup(_)) => {
                let handle = match &message {
                    Message::ResourceResolve(r) => r.handle,
                    Message::TreeList(r) => r.handle,
                    Message::TreeLookup(r) => r.handle,
                    _ => unreachable!(),
                };
                if self
                    .setup
                    .chart
                    .as_ref()
                    .is_some_and(|chart| chart.resolve(&self.host, &message, correlation))
                {
                    return Ok(());
                }
                let publisher = self
                    .setup
                    .publishers
                    .lock()
                    .unwrap()
                    .get(&handle)
                    .cloned()
                    .unwrap();
                publisher
                    .request(message, correlation)
                    .map_err(PluginError)?;
            }
            message @ (Message::StreamRead(_) | Message::StreamClose(_)) => {
                let stream = match &message {
                    Message::StreamRead(r) => r.stream,
                    Message::StreamClose(r) => r.stream,
                    _ => unreachable!(),
                };
                route_stream(&self.setup.publishers, stream)
                    .and_then(|publisher| publisher.request(message, correlation))
                    .map_err(PluginError)?;
            }
            _ => {}
        }
        Ok(())
    }
}
vosu_plugin_internal::export_plugin!(bundled Game);

/// Production stream routing (vosu-osu `plugin/resources.rs`): only the owning
/// session's resource worker serves a stream; an unowned stream is TARGET_GONE.
fn route_stream(publishers: &Publishers, stream: u64) -> Result<Arc<Publisher>, vosu_status> {
    publishers
        .lock()
        .unwrap()
        .values()
        .find(|publisher| publisher.owns_stream(stream))
        .cloned()
        .ok_or(vosu_status::VOSU_ERR_TARGET_GONE)
}

pub struct Fixture {
    pub h: host::Harness,
    pub providers: [Arc<BusStateProvider>; 4],
    pub files: vosu_adapter_upstream::files::Provider,
    pub chart: Arc<vosu_adapter_streamcompanion::provider::Provider>,
    attached: mpsc::Receiver<(u64, VtableHostServices)>,
    publishers: Publishers,
    roster: Roster,
    source: ClientType,
    has_chart: bool,
    next_pid: u32,
    _serial: MutexGuard<'static, ()>,
}
impl Fixture {
    pub fn new(source: ClientType) -> Self {
        Self::with_chart(source, None, None)
    }
    pub fn with_chart(
        source: ClientType,
        content: Option<&str>,
        engine: Option<Arc<dyn vosu_osu_calc::beatmap::BeatmapEngine + Send + Sync>>,
    ) -> Self {
        let serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let mut h = host::Harness::new();
        let has_chart = content.is_some();
        let chart = content
            .map(|content| chart::Chart::new(h.root.path(), content, source == ClientType::Lazer));
        let (attached, receiver) = mpsc::sync_channel(32);
        let publishers = Arc::new(Mutex::new(BTreeMap::new()));
        *SETUP.lock().unwrap() = Some(Setup {
            source,
            chart,
            attached,
            publishers: publishers.clone(),
        });
        let mut manifest: toml::Table = toml::from_str(vosu_osu::MANIFEST).unwrap();
        manifest.remove("actions");
        manifest.remove("concurrent_dispatch");
        manifest.remove("settings_schema");
        manifest
            .get_mut("provides")
            .and_then(toml::Value::as_array_mut)
            .unwrap()
            .retain(|provide| {
                !provide
                    .get("id")
                    .and_then(toml::Value::as_str)
                    .is_some_and(|id| id.starts_with("action:"))
            });
        h.load(
            &toml::to_string(&manifest).unwrap(),
            &[vosu_osu::OSU_NAMESPACE_SCHEMA],
            &[],
            vosu_plugin_vtable,
        );
        Self::with_adapters(serial, h, source, has_chart, engine, receiver, publishers)
    }

    /// The same adapters and providers over a game `create` makes as
    /// instance 1 through [`host::Harness::create`] (M5 X0: the external
    /// osu! producer in a plugin host). [`Self::attach`] drives only the
    /// in-process fixture game, so it is never used with this game.
    pub fn with_game(
        source: ClientType,
        has_chart: bool,
        engine: Option<Arc<dyn vosu_osu_calc::beatmap::BeatmapEngine + Send + Sync>>,
        create: impl FnOnce(&mut host::Harness),
    ) -> Self {
        let serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let mut h = host::Harness::new();
        create(&mut h);
        let (_, receiver) = mpsc::sync_channel(1);
        Self::with_adapters(
            serial,
            h,
            source,
            has_chart,
            engine,
            receiver,
            Arc::default(),
        )
    }

    fn with_adapters(
        serial: MutexGuard<'static, ()>,
        mut h: host::Harness,
        source: ClientType,
        has_chart: bool,
        engine: Option<Arc<dyn vosu_osu_calc::beatmap::BeatmapEngine + Send + Sync>>,
        receiver: mpsc::Receiver<(u64, VtableHostServices)>,
        publishers: Publishers,
    ) -> Self {
        h.load(
            vosu_adapter_upstream::MANIFEST,
            &[],
            &[],
            vosu_adapter_upstream::vosu_plugin_vtable,
        );
        h.load(
            vosu_adapter_gosumemory::MANIFEST,
            &[],
            &[],
            vosu_adapter_gosumemory::vosu_plugin_vtable,
        );
        h.load(
            vosu_adapter_streamcompanion::MANIFEST,
            &[],
            &[],
            vosu_adapter_streamcompanion::vosu_plugin_vtable,
        );
        let upstream = h.host.surface_requester(PluginInstanceId::new(2)).unwrap();
        let gosu = h.host.surface_requester(PluginInstanceId::new(3)).unwrap();
        let sc = h.host.surface_requester(PluginInstanceId::new(4)).unwrap();
        let chart = Arc::new(match engine {
            Some(engine) => vosu_adapter_streamcompanion::provider::Provider::with_fixture_engine(
                sc.clone(),
                engine,
            ),
            None => vosu_adapter_streamcompanion::provider::Provider::new(sc.clone()),
        });
        let providers = [
            Arc::new(BusStateProvider::new(
                Schema::V1,
                gosu,
                Arc::new(vosu_adapter_gosumemory::provider::Provider),
                Duration::from_millis(10),
            )),
            Arc::new(BusStateProvider::new(
                Schema::StreamCompanion,
                sc,
                chart.clone(),
                Duration::from_millis(10),
            )),
            Arc::new(BusStateProvider::new(
                Schema::V2,
                upstream.clone(),
                Arc::new(vosu_adapter_upstream::provider::Regular),
                Duration::from_millis(10),
            )),
            Arc::new(BusStateProvider::new(
                Schema::V2Precise,
                upstream.clone(),
                Arc::new(vosu_adapter_upstream::provider::Precise),
                Duration::from_millis(10),
            )),
        ];
        Self {
            h,
            providers,
            files: vosu_adapter_upstream::files::Provider::new(upstream),
            chart,
            attached: receiver,
            publishers,
            roster: if has_chart {
                Roster::without_resources()
            } else {
                Roster::default()
            },
            source,
            has_chart,
            next_pid: 750000,
            _serial: serial,
        }
    }
    pub fn attach(
        &mut self,
        settings: InstanceSettings,
        manager: bool,
        selected: bool,
    ) -> Arc<Publisher> {
        self.next_pid += 1;
        let pid = self.next_pid;
        let mut record = host::record(pid, "osu!.exe");
        record.path = "/fixture/osu!.exe".into();
        record.bitness = vosu_plugin_api::Bitness::X86;
        self.h.host.process_appeared(record);
        let (handle, services) = self.attached.recv_timeout(host::WAIT).unwrap();
        let publisher = self.roster.attach(
            Arc::new(services),
            handle,
            SessionFacts {
                client: self.source,
                settings,
                foreground: Some(selected),
            },
            manager,
            selected,
        );
        self.publishers
            .lock()
            .unwrap()
            .insert(handle, publisher.clone());
        if selected {
            self.h.foreground.set(Some(pid));
            self.h.host.foreground_changed();
        }
        publisher
    }
    /// The publisher the plugin would route `stream` to, or the production error.
    pub fn stream_owner(&self, stream: u64) -> Result<Arc<Publisher>, vosu_status> {
        route_stream(&self.publishers, stream)
    }
    /// The in-process fixture game's publisher for session `handle`.
    pub fn publisher(&self, handle: u64) -> Option<Arc<Publisher>> {
        self.publishers.lock().unwrap().get(&handle).cloned()
    }
    /// The provider's typed result, so callers can reject a vacuous not-ready
    /// response instead of comparing its corpus placeholder bytes.
    pub fn outcome(&self, schema: &str) -> CachedJson {
        let (index, route) = match schema {
            "v1" => (0, StateRoute::V1),
            "sc" => (1, StateRoute::StreamCompanion),
            "v2" => (2, StateRoute::V2),
            "v2-precise" => (3, StateRoute::V2Precise),
            _ => panic!("unknown schema"),
        };
        self.h.runtime.block_on(async {
            let _lease = (index == 1 && self.has_chart)
                .then(|| self.chart.retain(&[]))
                .flatten();
            let mut changes = self.chart.changes().expect("live fixture chart worker");
            let first = self.providers[index].cached_json(route).await;
            if index == 1 && self.has_chart {
                // A loaded host can exceed the HTTP wait bound: settle the
                // retained chart job itself, then read its completed view.
                tokio::time::timeout(Duration::from_secs(5), async {
                    while self.chart.pending() || self.chart.revision() == 0 {
                        changes.changed().await.unwrap();
                    }
                })
                .await
                .expect("source-oracle chart publication deadline");
                drop(first);
                self.providers[index].cached_json(route).await
            } else {
                first
            }
        })
    }
    pub fn encode(&self, schema: &str) -> Vec<u8> {
        match self.outcome(schema) {
            CachedJson::Ready(bytes) => bytes.to_vec(),
            // The source-oracle corpus records the typed not-ready outcome,
            // not the transport's legacy error envelope (covered separately).
            CachedJson::NotReady => br#"{"error":"not_ready"}"#.to_vec(),
            CachedJson::Failed { body, .. } => body.to_vec(),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.h.runtime.block_on(self.chart.shutdown()).unwrap();
        self.publishers.lock().unwrap().clear();
        self.h.host.destroy_all(host::WAIT);
    }
}
