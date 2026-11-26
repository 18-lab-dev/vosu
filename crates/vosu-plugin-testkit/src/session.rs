//! A session/focus testkit plugin (`docs/v2/implementation-plan.md` M1
//! Exit: session lifecycle, identification, the Focus rule, end to end):
//! reacts to `SESSION_ATTACHED` per its configured [`OnAttach`] behavior
//! and reports every session/focus message it receives to a bounded
//! channel a test reads from — `PluginHost`'s session and focus wiring
//! exercised through one real plugin instance instead of reaching into
//! its private state.
//!
//! Per-instance test behavior ([`SessionPluginConfig`]) travels through a
//! plugin-id-keyed registry set up before `PluginHost::create_instance`,
//! the same pattern [`register`] already uses for the event channel:
//! `create`'s `init.settings` is always `{}` here
//! (`PluginSettingsService::effective_settings` returns an empty map for
//! any plugin id without a `settings.schema.json`, and this testkit
//! plugin ships none), so settings cannot carry it.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

use crate::outbox::{Coalesce, Merge, Outbox};

use vosu_plugin_api::{
    FocusChanged, HostServices, Init, Message, Plugin, PluginError, SessionAttached,
    SessionDecline, SessionDetached, SessionIdentify, VtableHostServices,
};

/// One message [`SessionPlugin`] observed, in arrival order.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    Attached(SessionAttached),
    Detached(SessionDetached),
    Focus(FocusChanged),
}

/// Per-plugin-id test behavior: whether `create`'s instance should
/// request `focus_priority` on every `SESSION_IDENTIFY` it sends, how it
/// reacts to `SESSION_ATTACHED`, and whether its own `create` should
/// fail outright (P2-5: proving a failed `create` does not leak the
/// `[match]` table `register` installed before it ran).
#[derive(Debug, Clone, Copy, Default)]
pub struct SessionPluginConfig {
    pub focus_priority: bool,
    pub on_attach: OnAttach,
    pub fail_create: bool,
}

/// How [`SessionPlugin`] reacts to `SESSION_ATTACHED`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OnAttach {
    /// Sends `SESSION_IDENTIFY` right away (the default: every other test
    /// in this phase exercises the identify path).
    #[default]
    Identify,
    /// Sends `SESSION_DECLINE` instead.
    Decline,
    /// Does nothing — records the `SESSION_ATTACHED` event and leaves the
    /// session pending, so a test can exercise the 5-second identification
    /// timeout sweep.
    Ignore,
}

fn events_registry() -> &'static Mutex<HashMap<String, SyncSender<SessionEvent>>> {
    static REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, SyncSender<SessionEvent>>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &REGISTRY
}

fn config_registry() -> &'static Mutex<HashMap<String, SessionPluginConfig>> {
    static REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, SessionPluginConfig>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &REGISTRY
}

/// Registers a fresh, bounded event channel for `plugin_id` with the
/// default config (identify every session, no `focus_priority`),
/// returning the receiver: call before `PluginHost::create_instance` so
/// the channel exists before the plugin's own `create` can look it up.
pub fn register(plugin_id: impl Into<String>) -> Receiver<SessionEvent> {
    register_with_config(plugin_id, SessionPluginConfig::default())
}

/// Same as [`register`], with explicit [`SessionPluginConfig`].
pub fn register_with_config(
    plugin_id: impl Into<String>,
    config: SessionPluginConfig,
) -> Receiver<SessionEvent> {
    let plugin_id = plugin_id.into();
    let (tx, rx) = sync_channel(64);
    let mut events = events_registry()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    assert!(
        !events.contains_key(&plugin_id),
        "testkit plugin id {plugin_id:?} is already registered: ids must be unique per test \
         (a process-wide registry keyed by plugin id silently overwrites the earlier test's \
         event channel when two tests share one, which can deliver its events to the wrong \
         test under parallel execution)"
    );
    events.insert(plugin_id.clone(), tx);
    drop(events);
    config_registry()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(plugin_id, config);
    rx
}

pub struct SessionPlugin {
    host: VtableHostServices,
    events: Outbox<SessionEvent>,
    config: SessionPluginConfig,
}

impl Plugin for SessionPlugin {
    fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        // Consumed (`remove`, not `get().cloned()`): once this instance has
        // taken its own registration, the plugin id is free again for a
        // later, different test to reuse without tripping
        // `register_with_config`'s own duplicate check.
        let config = config_registry()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&init.plugin_id)
            .unwrap_or_default();
        if config.fail_create {
            return Err(PluginError(vosu_plugin_api::vosu_status::VOSU_ERR_BACKEND));
        }
        let events = events_registry()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&init.plugin_id)
            .unwrap_or_else(|| {
                panic!(
                    "no session-event channel registered for {:?}",
                    init.plugin_id
                )
            });
        Ok(Self {
            host,
            events: Outbox::new(events),
            config,
        })
    }

    fn on_message(&mut self, message: Message, _correlation_id: u64) -> Result<(), PluginError> {
        match message {
            Message::SessionAttached(attached) => {
                match self.config.on_attach {
                    OnAttach::Identify => {
                        let identify = SessionIdentify {
                            handle: attached.handle,
                            game: "testkit".to_owned(),
                            variant: "session".to_owned(),
                            name: "Session Testkit".to_owned(),
                            version: "0.1.0".to_owned(),
                            focus_priority: self.config.focus_priority,
                        };
                        let _ = self.host.send(&Message::SessionIdentify(identify), 0);
                    }
                    OnAttach::Decline => {
                        let _ = self.host.send(
                            &Message::SessionDecline(SessionDecline {
                                handle: attached.handle,
                            }),
                            0,
                        );
                    }
                    OnAttach::Ignore => {}
                }
                self.events.send(SessionEvent::Attached(attached));
            }
            Message::SessionDetached(detached) => {
                self.events.send(SessionEvent::Detached(detached));
            }
            Message::FocusChanged(focus) => {
                self.events.send(SessionEvent::Focus(focus));
            }
            _ => {}
        }
        Ok(())
    }
}

/// Focus is latest-only per session; a full event channel keeps the newest.
impl Coalesce for SessionEvent {
    fn merge(&mut self, next: Self) -> Merge<Self> {
        match (self, next) {
            (Self::Focus(queued), Self::Focus(focus)) if queued.handle == focus.handle => {
                *queued = focus;
                Merge::Merged
            }
            (Self::Focus(_), next @ Self::Focus(_)) => Merge::Skip(next),
            (_, next) => Merge::Stop(next),
        }
    }
}

vosu_plugin_internal::export_plugin!(bundled SessionPlugin);
