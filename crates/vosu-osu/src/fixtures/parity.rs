//! M5 X0 parity ingress: committed source-oracle replay through the
//! production publication path, in the plugin host or bundled daemon.
//!
//! The in-process oracle replay (`tests/state_api_compat/oracle.rs`) feeds
//! each committed `input.json` through the production publication path:
//! `vosu_osu::fixtures::Roster` attaches the real `BusPublisher` (projection,
//! resource publication, tournament roster) to the session and publishes
//! the snapshot `oracle_input::instances` builds. Its fixture game
//! (`tests/adapter_fixture/mod.rs`, `Game`) identifies the session, declares
//! the immutable oracle chart and routes resource requests. This module does
//! exactly the same inside the plugin host, through the child's own host
//! services and rings, so the only difference between the two replays is
//! the runtime. Both share the conversion (`oracle_input.rs`) and the chart
//! (`chart.rs`) source files.
//!
//! It runs only when the instance's data directory holds `parity.json`
//! (written by `tests/plugin_host_native/parity.rs`):
//!
//! - `input`: the committed `input.json`;
//! - `update`: an optional second committed input, published to the
//!   selected session once the test creates `<receipts>/update`;
//! - `chart_root`: where the oracle chart is written and declared;
//! - `pids`: the target PID of each source instance, in attach order;
//! - `receipts`: where `published-<pid>` and `updated` are written once
//!   core acknowledged the publication.
//! - `stable_import_songs`: optional absolute, existing Songs directory
//!   strictly inside this fixture's data root. No default or game discovery.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::{Publisher, Roster};
use crate::actions::ImportActions;
use crate::project::SessionFacts;
use tokio_util::sync::CancellationToken;
use vosu_osu_model::enums::ClientType;
use vosu_plugin_api::{
    HostServices, Message, PluginError, SessionIdentify, VtableHostServices, vosu_status,
};

#[path = "../../../../tests/adapter_fixture/chart.rs"]
mod chart;
#[path = "../../../../tests/state_api_compat/oracle_input.rs"]
#[expect(
    dead_code,
    reason = "the shared oracle conversion; the ingress needs only the snapshot builders"
)]
mod oracle_input;

/// How long a publication may take to settle before its receipt.
const SETTLE: Duration = Duration::from_secs(20);

/// One source instance waiting for, or attached to, its session.
struct Source {
    pid: u32,
    instance: oracle_input::SourceInstance,
}

struct FixtureSession {
    publisher: Arc<Publisher>,
    import_life: CancellationToken,
}

fn isolated_songs(data_dir: &Path, songs: &Path) -> Option<PathBuf> {
    if !songs.is_absolute() {
        return None;
    }
    let data_dir = data_dir.canonicalize().ok()?;
    let songs = songs.canonicalize().ok()?;
    (songs != data_dir && songs.starts_with(&data_dir) && songs.is_dir()).then_some(songs)
}

pub struct Parity {
    host: Arc<VtableHostServices>,
    client: ClientType,
    chart: Option<chart::Chart>,
    roster: Roster,
    sources: Vec<Source>,
    update: Option<oracle_input::Input>,
    receipts: PathBuf,
    // Session handle → the fixture's resource and import lifetime.
    sessions: Mutex<BTreeMap<u64, FixtureSession>>,
    stable_import_songs: Option<PathBuf>,
    imports: ImportActions,
    stopping: Arc<AtomicBool>,
    updates: Mutex<Vec<JoinHandle<()>>>,
}

fn read_input(path: &Path) -> oracle_input::Input {
    serde_json::from_slice(&std::fs::read(path).expect("the committed oracle input"))
        .expect("a valid oracle input")
}

impl Parity {
    /// The ingress, when `data_dir` holds `parity.json`.
    pub fn load(host: VtableHostServices, data_dir: &Path) -> Option<Self> {
        let control: serde_json::Value =
            serde_json::from_slice(&std::fs::read(data_dir.join("parity.json")).ok()?)
                .expect("a valid parity control");
        let path = |key: &str| PathBuf::from(control[key].as_str().expect(key));
        let input = read_input(&path("input"));
        let client = oracle_input::client(&input);
        let pids: Vec<u32> = control["pids"]
            .as_array()
            .expect("pids")
            .iter()
            .map(|pid| u32::try_from(pid.as_u64().expect("pid")).unwrap())
            .collect();
        let instances = oracle_input::instances(&input);
        assert_eq!(pids.len(), instances.len(), "one PID per source instance");
        let chart = oracle_input::chart_content(&input).map(|content| {
            chart::Chart::new(&path("chart_root"), &content, client == ClientType::Lazer)
        });
        Some(Self {
            host: Arc::new(host),
            client,
            roster: if chart.is_some() {
                Roster::without_resources()
            } else {
                Roster::default()
            },
            chart,
            sources: pids
                .into_iter()
                .zip(instances)
                .map(|(pid, instance)| Source { pid, instance })
                .collect(),
            update: control["update"]
                .as_str()
                .map(|path| read_input(Path::new(path))),
            receipts: path("receipts"),
            sessions: Mutex::default(),
            stable_import_songs: control["stable_import_songs"]
                .as_str()
                .filter(|_| client == ClientType::Stable)
                .and_then(|songs| isolated_songs(data_dir, Path::new(songs))),
            imports: ImportActions::default(),
            stopping: Arc::default(),
            updates: Mutex::default(),
        })
    }

    pub fn on_message(&self, message: Message, correlation: u64) -> Result<(), PluginError> {
        match message {
            Message::SessionAttached(attached) => self.attach(attached.handle, attached.pid),
            Message::SessionDetached(detached) => {
                let session = self.sessions.lock().unwrap().remove(&detached.handle);
                if let Some(session) = session {
                    session.import_life.cancel();
                }
            }
            Message::ActionInvoke(action) => {
                let lifecycle = action.session.and_then(|handle| {
                    self.sessions
                        .lock()
                        .unwrap()
                        .get(&handle)
                        .map(|session| session.import_life.clone())
                });
                return self.imports.invoke_fixture(
                    &self.host,
                    action,
                    lifecycle.as_ref().zip(self.stable_import_songs.as_deref()),
                );
            }
            Message::ActionCancel(cancel) => return self.imports.cancel(cancel.invocation),
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
                    .chart
                    .as_ref()
                    .is_some_and(|chart| chart.resolve(&self.host, &message, correlation))
                {
                    return Ok(());
                }
                let publisher = self
                    .sessions
                    .lock()
                    .unwrap()
                    .get(&handle)
                    .map(|session| session.publisher.clone());
                publisher
                    .ok_or(PluginError(vosu_status::VOSU_ERR_TARGET_GONE))?
                    .request(message, correlation)
                    .map_err(PluginError)?;
            }
            message @ (Message::StreamRead(_) | Message::StreamClose(_)) => {
                let stream = match &message {
                    Message::StreamRead(r) => r.stream,
                    Message::StreamClose(r) => r.stream,
                    _ => unreachable!(),
                };
                // Production stream routing: only the owning session's
                // resource worker serves a stream.
                let owner = self
                    .sessions
                    .lock()
                    .unwrap()
                    .values()
                    .map(|session| &session.publisher)
                    .find(|publisher| publisher.owns_stream(stream))
                    .cloned();
                owner
                    .ok_or(PluginError(vosu_status::VOSU_ERR_TARGET_GONE))?
                    .request(message, correlation)
                    .map_err(PluginError)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn attach(&self, handle: u64, pid: u32) {
        let Some(source) = self.sources.iter().find(|source| source.pid == pid) else {
            return;
        };
        let status = self.host.send(
            &Message::SessionIdentify(SessionIdentify {
                handle,
                game: "osu".into(),
                variant: if self.client == ClientType::Lazer {
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
        if let Some(chart) = &self.chart {
            chart.attach(&self.host, handle);
        }
        // The replay's selected instance is its logical PID 10.
        let selected = source.instance.pid == 10;
        let publisher = self.roster.attach(
            self.host.clone(),
            handle,
            SessionFacts {
                client: self.client,
                settings: source.instance.settings.clone(),
                foreground: Some(selected),
            },
            source.instance.manager,
            selected,
        );
        self.sessions.lock().unwrap().insert(
            handle,
            FixtureSession {
                publisher: publisher.clone(),
                import_life: CancellationToken::new(),
            },
        );
        publisher
            .publish(source.instance.state.clone(), vosu_osu_state::Delivery::All)
            .expect("the oracle snapshot publishes");
        settle(&publisher);
        self.receipt(&format!("published-{pid}"));
        if selected && let Some(update) = &self.update {
            let state = oracle_input::state(update);
            let receipts = self.receipts.clone();
            let stopping = Arc::clone(&self.stopping);
            let update = std::thread::spawn(move || {
                if !wait_file(&receipts.join("update"), &stopping) {
                    return;
                }
                publisher
                    .publish(state, vosu_osu_state::Delivery::All)
                    .expect("the update snapshot publishes");
                settle(&publisher);
                write_receipt(&receipts, "updated");
            });
            self.updates.lock().unwrap().push(update);
        }
    }

    fn receipt(&self, name: &str) {
        write_receipt(&self.receipts, name);
    }
}

impl Drop for Parity {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        self.imports.shutdown();
        for update in self.updates.get_mut().unwrap().drain(..) {
            update.join().expect("parity update worker");
        }
    }
}

#[cfg(test)]
#[test]
fn import_root_must_be_an_explicit_subdirectory_of_the_fixture() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let songs = root.path().join("Songs");
    std::fs::create_dir(&songs).unwrap();
    assert_eq!(
        isolated_songs(root.path(), &songs),
        songs.canonicalize().ok()
    );
    for invalid in [
        Path::new("Songs"),
        root.path(),
        outside.path(),
        &songs.join("missing"),
    ] {
        assert_eq!(isolated_songs(root.path(), invalid), None);
    }
}

/// Waits until the publisher's asynchronous resource publication committed
/// and its mailbox is idle.
#[expect(
    clippy::disallowed_methods,
    reason = "the publisher exposes only a settled probe; the fixture polls it until a bounded deadline"
)]
fn settle(publisher: &Publisher) {
    let deadline = Instant::now() + SETTLE;
    while !publisher.settled() {
        assert!(Instant::now() < deadline, "the publication never settled");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Waits up to a minute for the test's update barrier, or until teardown.
#[expect(
    clippy::disallowed_methods,
    reason = "a file barrier another process creates; the fixture polls it until a bounded deadline"
)]
fn wait_file(path: &Path, stopping: &AtomicBool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !path.exists() {
        if stopping.load(Ordering::Relaxed) || Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    true
}

/// Writes receipt `name` whole: a partial file is never visible.
#[expect(
    clippy::disallowed_methods,
    reason = "a disposable test receipt in the test's own scratch directory"
)]
fn write_receipt(dir: &Path, name: &str) {
    let partial = dir.join(format!("{name}.partial"));
    std::fs::write(&partial, std::process::id().to_string()).unwrap();
    std::fs::rename(partial, dir.join(name)).unwrap();
}
