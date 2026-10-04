//! Transport-independent `/websocket/commands` protocol.
mod framing;

use std::{
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use bytes::Bytes;
use serde_json::Value;
use tokio::{
    sync::watch,
    task::JoinHandle,
    time::{Instant, sleep_until},
};
use tokio_util::sync::CancellationToken;

use super::websocket::Filter;
use framing::{error_value, parse_filters, reply};

pub const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

/// Counter settings boundary. Values are the compact browser-facing map.
pub trait SettingsProvider: Send + Sync + 'static {
    fn get(&self, overlay: &str) -> Result<Value, String>;
    fn save(&self, overlay: &str, body: Value) -> Result<(), String>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Relay {
    pub except: Option<u64>,
    pub command: String,
    pub overlay: String,
    pub payload: Option<String>,
}

/// Internal refresh boundary used by counter install/delete/settings routes.
pub trait RelaySink: Send + Sync + 'static {
    fn relay(&self, event: Relay);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandClient {
    pub id: u64,
    /// Decoded value of the `l` query parameter.
    pub overlay: String,
}

impl CommandClient {
    pub fn from_query(id: u64, query: Option<&str>) -> Result<Self, UriDecodeError> {
        Ok(Self {
            id,
            overlay: decode_uri(query.unwrap_or_default())?,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum CommandAction {
    Reply(Bytes),
    ApplyFilters(Vec<Filter>),
    NoAck,
}

#[derive(Clone)]
struct SaveRequest {
    generation: u64,
    overlay: String,
    value: Value,
}

struct SaveCoordinator {
    generation: u64,
    closed: bool,
    sender: watch::Sender<Option<SaveRequest>>,
    receiver: Option<watch::Receiver<Option<SaveRequest>>>,
    task: Option<JoinHandle<()>>,
}

pub struct CommandProcessor {
    settings: Arc<dyn SettingsProvider>,
    relays: Arc<dyn RelaySink>,
    save: Mutex<SaveCoordinator>,
    save_cancellation: CancellationToken,
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl CommandProcessor {
    pub fn new(settings: Arc<dyn SettingsProvider>, relays: Arc<dyn RelaySink>) -> Self {
        let (sender, receiver) = watch::channel(None);
        Self {
            settings,
            relays,
            save: Mutex::new(SaveCoordinator {
                generation: 0,
                closed: false,
                sender,
                receiver: Some(receiver),
                task: None,
            }),
            save_cancellation: CancellationToken::new(),
        }
    }

    pub fn process(&self, client: &CommandClient, data: &str) -> CommandAction {
        let Some(first) = data.find(':') else {
            return CommandAction::NoAck;
        };
        let second = data[first + 1..].find(':').map(|index| first + 1 + index);
        let command = &data[..first];
        let overlay_encoded = second.map_or(&data[first + 1..], |index| &data[first + 1..index]);
        let Ok(overlay) = decode_uri_component(overlay_encoded) else {
            return CommandAction::NoAck;
        };
        let legacy_payload = &data[first + 1..];
        // JS substring(SecondIndex + 1) uses zero when no second colon.
        let payload = second.map_or(data, |index| &data[index + 1..]);

        match command {
            "getSettings" => {
                let message = if overlay != client.overlay {
                    error_value("Wrong overlay")
                } else {
                    match self.settings.get(&overlay) {
                        Ok(value) => value,
                        Err(error) => error_value(error),
                    }
                };
                reply(command, Some(message))
            }
            "updateSettings" => match serde_json::from_str(payload) {
                Ok(value) => reply(command, Some(value)),
                Err(_) => CommandAction::NoAck,
            },
            "saveSettings" => {
                let Ok(value) = serde_json::from_str::<Value>(payload) else {
                    return CommandAction::NoAck;
                };
                self.schedule_save(client.overlay.clone(), value);
                self.relays.relay(Relay {
                    except: Some(client.id),
                    command: "updateSettings".into(),
                    overlay,
                    payload: Some(payload.to_owned()),
                });
                CommandAction::NoAck
            }
            "applyFilters" => {
                let source = if payload.starts_with('[') {
                    payload
                } else {
                    legacy_payload
                };
                parse_filters(source)
                    .map(CommandAction::ApplyFilters)
                    .unwrap_or(CommandAction::NoAck)
            }
            _ => reply(command, None),
        }
    }

    /// Relays a settings refresh only to clients whose decoded `l` matches.
    pub fn refresh_settings(&self, overlay: impl Into<String>) {
        self.relays.relay(Relay {
            except: None,
            command: "getSettings".into(),
            overlay: overlay.into(),
            payload: None,
        });
    }

    /// Applies the source relay targeting rules to a receiving client and
    /// returns the command frame to feed back through `process`.
    pub fn relay_frame(client: &CommandClient, relay: &Relay) -> Option<String> {
        if relay.except == Some(client.id) {
            return None;
        }
        if matches!(relay.command.as_str(), "getSettings" | "updateSettings")
            && relay.overlay != client.overlay
        {
            return None;
        }
        Some(format!(
            "{}:{}:{}",
            relay.command,
            relay.overlay,
            relay.payload.as_deref().unwrap_or_default()
        ))
    }
    /// Stops and joins the debounce worker. Runtime shutdown should call this
    /// before dropping the processor.
    pub async fn shutdown(&self) {
        let task = {
            let mut save = lock_unpoisoned(&self.save);
            save.closed = true;
            self.save_cancellation.cancel();
            save.task.take()
        };
        if let Some(task) = task {
            let _ = task.await;
        }
    }

    fn schedule_save(&self, overlay: String, value: Value) {
        self.schedule_save_inner(overlay, value, || {});
    }

    fn schedule_save_inner<F>(&self, overlay: String, value: Value, before_publish: F)
    where
        F: FnOnce(),
    {
        let mut save = lock_unpoisoned(&self.save);
        if save.closed {
            return;
        }
        if save.task.is_none() {
            let receiver = save
                .receiver
                .take()
                .expect("save receiver is consumed only by its worker");
            let settings = Arc::clone(&self.settings);
            let cancellation = self.save_cancellation.clone();
            save.task = Some(tokio::spawn(save_worker(settings, receiver, cancellation)));
        }
        save.generation = save
            .generation
            .checked_add(1)
            .expect("save generation cannot overflow");
        let request = SaveRequest {
            generation: save.generation,
            overlay,
            value,
        };
        // Publishing is inside the same critical section as generation
        // assignment, so an older concurrent caller can never publish after a
        // newer one.
        before_publish();
        save.sender.send_replace(Some(request));
    }
}

async fn save_worker(
    settings: Arc<dyn SettingsProvider>,
    mut receiver: watch::Receiver<Option<SaveRequest>>,
    cancellation: CancellationToken,
) {
    loop {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => return,
            changed = receiver.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
        let Some(mut request) = receiver.borrow_and_update().clone() else {
            continue;
        };
        let mut deadline = Instant::now() + SAVE_DEBOUNCE;
        loop {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return,
                changed = receiver.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    if let Some(newer) = receiver.borrow_and_update().clone()
                        && newer.generation > request.generation
                    {
                        request = newer;
                        deadline = Instant::now() + SAVE_DEBOUNCE;
                    }
                }
                _ = sleep_until(deadline) => {
                    let latest = receiver.borrow_and_update().clone();
                    if let Some(newer) = latest
                        && newer.generation > request.generation
                    {
                        request = newer;
                        deadline = Instant::now() + SAVE_DEBOUNCE;
                        continue;
                    }
                    let _ = settings.save(&request.overlay, request.value);
                    break;
                }
            }
        }
    }
}

impl Drop for CommandProcessor {
    fn drop(&mut self) {
        self.save_cancellation.cancel();
        let save = self.save.get_mut().unwrap_or_else(PoisonError::into_inner);
        save.closed = true;
        if let Some(task) = save.task.take() {
            task.abort();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UriDecodeError;

fn decode_uri_component(input: &str) -> Result<String, UriDecodeError> {
    percent_decode(input, false)
}

fn decode_uri(input: &str) -> Result<String, UriDecodeError> {
    percent_decode(input, true)
}

fn percent_decode(input: &str, preserve_reserved: bool) -> Result<String, UriDecodeError> {
    const RESERVED: &[u8] = b";/?:@&=+$,#";
    let bytes = input.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            result.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return Err(UriDecodeError);
        }
        let high = hex(bytes[index + 1]).ok_or(UriDecodeError)?;
        let low = hex(bytes[index + 2]).ok_or(UriDecodeError)?;
        let decoded = high << 4 | low;
        if preserve_reserved && RESERVED.contains(&decoded) {
            result.extend_from_slice(&bytes[index..index + 3]);
        } else {
            result.push(decoded);
        }
        index += 3;
    }
    String::from_utf8(result).map_err(|_| UriDecodeError)
}

fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Providers {
        saves: Mutex<Vec<(String, Value)>>,
        relays: Mutex<Vec<Relay>>,
    }

    impl SettingsProvider for Providers {
        fn get(&self, overlay: &str) -> Result<Value, String> {
            if overlay == "broken" {
                Err("Missing settings".into())
            } else {
                Ok(serde_json::json!({"enabled":true}))
            }
        }
        fn save(&self, overlay: &str, body: Value) -> Result<(), String> {
            lock_unpoisoned(&self.saves).push((overlay.into(), body));
            Ok(())
        }
    }

    impl RelaySink for Providers {
        fn relay(&self, event: Relay) {
            lock_unpoisoned(&self.relays).push(event);
        }
    }

    fn setup(overlay: &str) -> (Arc<Providers>, CommandProcessor, CommandClient) {
        let providers = Arc::new(Providers::default());
        let processor = CommandProcessor::new(providers.clone(), providers.clone());
        let client = CommandClient {
            id: 7,
            overlay: overlay.into(),
        };
        (providers, processor, client)
    }

    fn reply_json(action: CommandAction) -> Value {
        let CommandAction::Reply(bytes) = action else {
            panic!("expected reply")
        };
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn malformed_and_unknown_envelopes() {
        let (_, processor, client) = setup("counter");
        assert_eq!(processor.process(&client, "no-colon"), CommandAction::NoAck);
        assert_eq!(
            processor.process(&client, "getSettings:%GG"),
            CommandAction::NoAck
        );
        assert_eq!(
            reply_json(processor.process(&client, "future:counter")),
            serde_json::json!({"command":"future"})
        );
    }

    #[test]
    fn settings_update_and_filter_envelopes() {
        let (_, processor, client) = setup("my overlay");
        assert_eq!(
            reply_json(processor.process(&client, "getSettings:my%20overlay")),
            serde_json::json!({"command":"getSettings","message":{"enabled":true}})
        );
        assert_eq!(
            reply_json(processor.process(&client, "getSettings:wrong")),
            serde_json::json!({"command":"getSettings","message":{"error":"Wrong overlay"}})
        );
        let (_, broken_processor, broken_client) = setup("broken");
        assert_eq!(
            reply_json(broken_processor.process(&broken_client, "getSettings:broken")),
            serde_json::json!({"command":"getSettings","message":{"error":"Missing settings"}})
        );
        assert_eq!(
            reply_json(processor.process(&client, "updateSettings:any:{\"x\":1}")),
            serde_json::json!({"command":"updateSettings","message":{"x":1}})
        );
        assert_eq!(
            processor.process(&client, "updateSettings:any:{"),
            CommandAction::NoAck
        );
        assert_eq!(
            processor.process(
                &client,
                "applyFilters:any:[\"play\",42,{\"unknown\":true},{\"field\":\"menu\",\"keys\":[\"state\",false,{\"field\":\"beatmap\",\"keys\":[\"title\",null]}]}]"
            ),
            CommandAction::ApplyFilters(vec![
                Filter::Field("play".into()),
                Filter::Nested {
                    field: "menu".into(),
                    keys: vec![
                        Filter::Field("state".into()),
                        Filter::Nested {
                            field: "beatmap".into(),
                            keys: vec![Filter::Field("title".into())],
                        },
                    ],
                },
            ])
        );
        assert_eq!(
            processor.process(&client, "applyFilters:[\"legacy\"]"),
            CommandAction::ApplyFilters(vec![Filter::Field("legacy".into())])
        );
        assert_eq!(
            processor.process(&client, "applyFilters:any:{}"),
            CommandAction::NoAck
        );
    }

    #[test]
    fn excessive_filters_are_bounded() {
        let excessive = format!("[{}]", vec!["\"field\""; 257].join(","));
        assert_eq!(parse_filters(&excessive), None);
        let too_deep = format!(
            "{}{}{}",
            "[{\"field\":\"a\",\"keys\":".repeat(9),
            "[\"leaf\"]",
            "}]".repeat(9)
        );
        assert_eq!(parse_filters(&too_deep), None);
    }
    #[test]
    fn nested_filter_fields_use_javascript_truthiness_and_property_keys() {
        let keys = |field: &str| Filter::Nested {
            field: field.into(),
            keys: vec![Filter::Field("value".into())],
        };
        assert_eq!(
            parse_filters(
                r#"[{"field":1.0,"keys":["value"]},{"field":true,"keys":["value"]},{"field":[],"keys":["value"]},{"field":[1,null,"x"],"keys":["value"]},{"field":{},"keys":["value"]},{"field":0,"keys":["value"]},{"field":false,"keys":["value"]},{"field":"","keys":["value"]},{"field":null,"keys":["value"]}]"#
            ),
            Some(vec![
                keys("1"),
                keys("true"),
                keys(""),
                keys("1,,x"),
                keys("[object Object]"),
            ])
        );
    }

    #[tokio::test]
    async fn mixed_filter_command_reaches_projection_with_valid_entries_only() {
        use crate::server::websocket::{
            Outbound, Schema, StateProvider, StateSnapshot, WebSocketHub,
        };

        struct ProjectionSnapshot;
        impl StateSnapshot for ProjectionSnapshot {
            fn generation(&self) -> u64 {
                1
            }

            fn encode_unfiltered(&self) -> Result<Bytes, String> {
                Ok(Bytes::from_static(b"unfiltered"))
            }

            fn encode_filtered(&self, filters: &[Filter]) -> Result<Bytes, String> {
                serde_json::to_vec(filters)
                    .map(Bytes::from)
                    .map_err(|error| error.to_string())
            }
        }

        struct ProjectionProvider {
            events: tokio::sync::watch::Sender<u64>,
        }
        impl StateProvider for ProjectionProvider {
            fn poll_rate(&self, _: Schema) -> Duration {
                Duration::from_millis(10)
            }

            fn subscribe(&self, _: Schema) -> tokio::sync::watch::Receiver<u64> {
                self.events.subscribe()
            }

            fn capture(&self, _: Schema) -> Option<Arc<dyn StateSnapshot>> {
                Some(Arc::new(ProjectionSnapshot))
            }
        }

        let (_, processor, client) = setup("overlay");
        let CommandAction::ApplyFilters(filters) = processor.process(
            &client,
            "applyFilters:overlay:[\"play\",42,{\"field\":\"menu\",\"keys\":[\"state\",null]},{\"other\":true}]",
        ) else {
            panic!("mixed array must apply its supported filters");
        };
        let expected = vec![
            Filter::Field("play".into()),
            Filter::Nested {
                field: "menu".into(),
                keys: vec![Filter::Field("state".into())],
            },
        ];
        assert_eq!(filters, expected);

        let (events, _) = tokio::sync::watch::channel(0);
        let hub = WebSocketHub::new(Schema::V2, Arc::new(ProjectionProvider { events }));
        let (connection, mut frames) = hub.connect();
        connection.set_filters(filters);
        let runner = tokio::spawn({
            let hub = hub.clone();
            async move { hub.run().await }
        });
        assert_eq!(
            frames.recv().await,
            Some(Outbound::Text(Bytes::from(
                serde_json::to_vec(&expected).unwrap()
            )))
        );
        connection.cleanup();
        hub.shutdown();
        runner.await.unwrap();
    }

    #[tokio::test]
    async fn save_is_no_ack_relayed_and_globally_debounced() {
        let (providers, processor, client) = setup("overlay");
        assert_eq!(
            processor.process(&client, "saveSettings:overlay:{\"n\":1}"),
            CommandAction::NoAck
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            processor.process(&client, "saveSettings:overlay:{\"n\":2}"),
            CommandAction::NoAck
        );
        assert_eq!(
            processor.process(&client, "saveSettings:overlay:{"),
            CommandAction::NoAck
        );
        tokio::time::sleep(Duration::from_millis(550)).await;
        assert_eq!(
            *lock_unpoisoned(&providers.saves),
            vec![("overlay".into(), serde_json::json!({"n":2}))]
        );
        let relays = lock_unpoisoned(&providers.relays);
        assert_eq!(relays.len(), 2);
        assert_eq!(relays[1].except, Some(7));
        assert_eq!(relays[1].command, "updateSettings");
        assert_eq!(relays[1].payload.as_deref(), Some("{\"n\":2}"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_older_publish_cannot_replace_newer_save() {
        use std::sync::mpsc as std_mpsc;

        let (providers, processor, _) = setup("overlay");
        let processor = Arc::new(processor);
        let runtime = tokio::runtime::Handle::current();
        let (older_locked_tx, older_locked_rx) = std_mpsc::channel();
        let (release_older_tx, release_older_rx) = std_mpsc::channel();

        let older = {
            let processor = Arc::clone(&processor);
            let runtime = runtime.clone();
            std::thread::spawn(move || {
                let _runtime = runtime.enter();
                processor.schedule_save_inner(
                    "older".into(),
                    serde_json::json!({"n":1}),
                    move || {
                        older_locked_tx.send(()).unwrap();
                        release_older_rx.recv().unwrap();
                    },
                );
            })
        };
        older_locked_rx.recv().unwrap();

        let (newer_started_tx, newer_started_rx) = std_mpsc::channel();
        let newer = {
            let processor = Arc::clone(&processor);
            std::thread::spawn(move || {
                newer_started_tx.send(()).unwrap();
                processor.schedule_save_inner("newer".into(), serde_json::json!({"n":2}), || {});
            })
        };
        newer_started_rx.recv().unwrap();
        release_older_tx.send(()).unwrap();
        older.join().unwrap();
        newer.join().unwrap();

        tokio::time::sleep(Duration::from_millis(550)).await;
        assert_eq!(
            *lock_unpoisoned(&providers.saves),
            vec![("newer".into(), serde_json::json!({"n":2}))]
        );
        processor.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_cancels_and_joins_pending_save() {
        let (providers, processor, _) = setup("overlay");
        processor.schedule_save("overlay".into(), serde_json::json!({"n":1}));
        processor.shutdown().await;
        tokio::time::sleep(SAVE_DEBOUNCE + Duration::from_millis(20)).await;
        assert!(lock_unpoisoned(&providers.saves).is_empty());
    }

    #[test]
    fn internal_refresh_and_relay_targeting_match_source() {
        let (providers, processor, client) = setup("overlay");
        processor.refresh_settings("overlay");
        let relays = lock_unpoisoned(&providers.relays);
        assert!(CommandProcessor::relay_frame(&client, &relays[0]).is_some());
        let other = CommandClient {
            id: 9,
            overlay: "other".into(),
        };
        assert_eq!(CommandProcessor::relay_frame(&other, &relays[0]), None);
        let excluded = Relay {
            except: Some(7),
            command: "updateSettings".into(),
            overlay: "overlay".into(),
            payload: Some("{}".into()),
        };
        assert_eq!(CommandProcessor::relay_frame(&client, &excluded), None);
    }

    #[test]
    fn query_l_uses_decode_uri_reserved_semantics() {
        let client = CommandClient::from_query(1, Some("my%20overlay%2Fpart")).unwrap();
        assert_eq!(client.overlay, "my overlay%2Fpart");
        assert_eq!(
            CommandClient::from_query(1, Some("%GG")),
            Err(UriDecodeError)
        );
    }
}
