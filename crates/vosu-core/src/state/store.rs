//! The sessions map is held only long enough to clone/remove a slot. Slot
//! commit locks serialize writers and close; no slot lock acquires the map.
//! No plugin code or callbacks run under locks. ArcSwap reads never wait for
//! validation, and watch retains one revision.
use super::commit::{
    Usage, WriteIndex, budget, empty_object, removes_member, validate, write_path,
};
use super::{
    ClientFacts, CommitError, CommitOutcome, Pointer, PublisherGrant, SchemaRegistry, SessionKey,
    SessionState, StateObject, StateValue,
};
use arc_swap::ArcSwap;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::watch;
use vosu_plugin_api::PublishFieldRef;

pub struct StateStore {
    sessions: RwLock<HashMap<SessionKey, Arc<SessionSlot>>>,
    schemas: Arc<RwLock<SchemaRegistry>>,
}
struct SessionSlot {
    owner: Arc<str>,
    commit: Mutex<SlotMeta>,
    current: ArcSwap<SessionState>,
    revisions: watch::Sender<u64>,
}

#[derive(Default)]
struct SlotMeta {
    closed: bool,
    usage: HashMap<Arc<str>, Usage>,
}

// D-decision candidate: bound each namespace's JSON projection at 4 MiB
// (before audience redaction) and all object keys in a session at 65,536.
const MAX_NAMESPACE_BYTES: i64 = 4 * 1024 * 1024;
const MAX_SESSION_KEYS: i64 = 65_536;

impl StateStore {
    pub fn new(schemas: Arc<RwLock<SchemaRegistry>>) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            schemas,
        }
    }

    /// Called only after successful identify. Repeated identify is idempotent.
    pub fn open_session(
        &self,
        key: SessionKey,
        grant: &PublisherGrant,
        client: ClientFacts,
        focused: bool,
    ) -> Arc<SessionState> {
        let mut namespaces = vec![(Arc::from("vosu.client"), client.into_value(focused))];
        {
            let schemas = self.schemas.read().unwrap();
            for ns in grant.covered_core_namespaces() {
                let schema = schemas.get(&ns).expect("core schemas are installed");
                let fields = schema
                    .required_top_level
                    .iter()
                    .map(|key| {
                        let value = if ns.as_ref() == "vosu.chart" && key.as_ref() == "resources" {
                            empty_object()
                        } else {
                            StateValue::Null
                        };
                        (key.clone(), value)
                    })
                    .collect();
                namespaces.push((ns, StateValue::Object(Arc::new(StateObject::new(fields)))));
            }
        }
        let mut sessions = self.sessions.write().unwrap();
        if let Some(slot) = sessions.get(&key) {
            return slot.current.load_full();
        }
        let usage = namespaces
            .iter()
            .map(|(ns, value)| (ns.clone(), Usage::measure(value)))
            .collect();
        let current = Arc::new(SessionState::new(key, 0, namespaces));
        let (revisions, _) = watch::channel(0);
        sessions.insert(
            key,
            Arc::new(SessionSlot {
                owner: grant.plugin_id.clone(),
                commit: Mutex::new(SlotMeta {
                    closed: false,
                    usage,
                }),
                current: ArcSwap::from(current.clone()),
                revisions,
            }),
        );
        current
    }

    pub fn close_session(&self, key: SessionKey) -> Option<Arc<SessionState>> {
        let slot = self.sessions.write().unwrap().remove(&key)?;
        let mut meta = slot.commit.lock().unwrap();
        meta.closed = true;
        Some(slot.current.load_full())
    }

    pub fn current(&self, key: SessionKey) -> Option<Arc<SessionState>> {
        self.sessions
            .read()
            .unwrap()
            .get(&key)
            .map(|slot| slot.current.load_full())
    }

    pub fn revisions(&self, key: SessionKey) -> Option<watch::Receiver<u64>> {
        self.sessions
            .read()
            .unwrap()
            .get(&key)
            .map(|slot| slot.revisions.subscribe())
    }

    pub fn commit<'a>(
        &self,
        grant: &PublisherGrant,
        key: SessionKey,
        entries: impl ExactSizeIterator<Item = PublishFieldRef<'a>>,
    ) -> Result<CommitOutcome, CommitError> {
        let slot = self
            .sessions
            .read()
            .unwrap()
            .get(&key)
            .cloned()
            .ok_or(CommitError::UnknownSession)?;
        if slot.owner != grant.plugin_id {
            return Err(CommitError::NotOwner);
        }
        let mut meta = slot.commit.lock().unwrap();
        if meta.closed {
            return Err(CommitError::Detached);
        }
        let previous = slot.current.load_full();
        let validated = validate(grant, &previous, entries)?;
        if validated.is_empty() {
            return Ok(CommitOutcome {
                current: previous.clone(),
                previous,
                touched: Vec::new(),
            });
        }
        let usage = budget(
            &previous,
            &validated,
            &meta.usage,
            MAX_NAMESPACE_BYTES,
            MAX_SESSION_KEYS,
        )?;
        let mut lookup = WriteIndex::default();
        let mut namespaces = copy_namespaces(&previous);
        let mut touched = Vec::with_capacity(validated.len());
        for entry in validated {
            let index = namespaces
                .iter()
                .position(|(ns, _)| *ns == entry.ns)
                .unwrap_or_else(|| {
                    namespaces.push((entry.ns.clone(), StateValue::Null));
                    namespaces.len() - 1
                });
            let remove = removes_member(&entry);
            write_path(
                &mut namespaces[index].1,
                entry.pointer.tokens(),
                entry.value,
                remove,
                &mut lookup,
            );
            touched.push((entry.ns, entry.pointer));
        }
        meta.usage = usage;
        Ok(publish(&slot, previous, namespaces, touched))
    }

    pub fn set_focused(&self, key: SessionKey, focused: bool) -> Option<CommitOutcome> {
        let slot = self.sessions.read().unwrap().get(&key).cloned()?;
        let mut meta = slot.commit.lock().unwrap();
        if meta.closed {
            return None;
        }
        let previous = slot.current.load_full();
        let pointer = Pointer::parse("/focused").expect("fixed pointer");
        if previous.get("vosu.client", &pointer) == Some(&StateValue::Bool(focused)) {
            return None;
        }
        let mut namespaces = copy_namespaces(&previous);
        let (_, client) = namespaces
            .iter_mut()
            .find(|(ns, _)| ns.as_ref() == "vosu.client")
            .expect("client exists");
        write_path(
            client,
            pointer.tokens(),
            StateValue::Bool(focused),
            false,
            &mut WriteIndex::default(),
        );
        // The only possible transition is true (4 bytes) <-> false (5).
        meta.usage
            .get_mut("vosu.client")
            .expect("client accounting exists")
            .bytes += if focused { -1 } else { 1 };
        Some(publish(
            &slot,
            previous,
            namespaces,
            vec![(Arc::from("vosu.client"), pointer)],
        ))
    }
}

fn copy_namespaces(state: &SessionState) -> Vec<(Arc<str>, StateValue)> {
    state.namespace_entries().to_vec()
}

fn publish(
    slot: &SessionSlot,
    previous: Arc<SessionState>,
    namespaces: Vec<(Arc<str>, StateValue)>,
    touched: Vec<(Arc<str>, Pointer)>,
) -> CommitOutcome {
    let current = Arc::new(SessionState::new(
        previous.key,
        previous
            .revision
            .checked_add(1)
            .expect("session revision exhausted"),
        namespaces,
    ));
    slot.current.store(current.clone());
    slot.revisions.send_replace(current.revision);
    CommitOutcome {
        previous,
        current,
        touched,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;
    use vosu_plugin_api::{PluginManifest, PublishValueRef};

    fn fixture(id: &str, key: SessionKey) -> (Arc<StateStore>, PublisherGrant) {
        let grant = PublisherGrant::from_manifest(
            &PluginManifest::parse(
                &r#"
kind = "plugin"
manifest_version = 1
id = "test.store-locking"
name = "Store locking"
version = "1.0.0"
host = ">=2.0.0-alpha.1, <3.0.0"
abi_version = 1
runtime = "in-process"
[[provides]]
id = "field:test.store-locking"
version = "1.0.0"
schema = "schema.json"
[[resource_kinds]]
kind = "audio"
families = ["audio"]
[[resource_kinds]]
kind = "background"
families = ["image"]
"#
                .replace("test.store-locking", id),
            )
            .unwrap(),
        )
        .unwrap();
        let store = Arc::new(StateStore::new(Arc::new(RwLock::new(
            SchemaRegistry::with_core().unwrap(),
        ))));
        open(&store, &grant, key);
        (store, grant)
    }

    fn open(store: &StateStore, grant: &PublisherGrant, key: SessionKey) {
        store.open_session(
            key,
            grant,
            ClientFacts {
                game: "osu".into(),
                variant: "stable".into(),
                name: "test".into(),
                version: "1".into(),
                pid: key.0 as u32,
                session: format!("test-{}", key.0),
                plugin_id: grant.plugin_id.to_string(),
                plugin_version: "1.0.0".into(),
            },
            false,
        );
    }

    #[test]
    fn blocked_commit_does_not_block_unrelated_session_operations() {
        let (store, grant) = fixture("test.store-locking", SessionKey(7401));
        let (entered, started) = mpsc::sync_channel(1);
        let (release, released) = mpsc::sync_channel(1);
        let (done, completed) = mpsc::sync_channel(1);
        let writer_store = store.clone();
        let writer_grant = grant.clone();
        let writer = std::thread::spawn(move || {
            let entries = std::iter::once_with(|| {
                entered.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(10)).unwrap();
                PublishFieldRef {
                    handle: 7401,
                    ns: "test.store-locking",
                    pointer: "/a",
                    value: PublishValueRef::Null,
                }
            });
            writer_store
                .commit(&writer_grant, SessionKey(7401), entries)
                .unwrap();
            done.send(()).unwrap();
        });
        started.recv_timeout(Duration::from_secs(10)).unwrap();
        let other_store = store.clone();
        let (done, completed_other) = mpsc::sync_channel(1);
        let other = std::thread::spawn(move || {
            open(&other_store, &grant, SessionKey(7402));
            assert!(other_store.current(SessionKey(7401)).is_some());
            assert!(other_store.revisions(SessionKey(7402)).is_some());
            assert!(other_store.close_session(SessionKey(7402)).is_some());
            done.send(()).unwrap();
        });
        completed_other
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        release.send(()).unwrap();
        completed.recv_timeout(Duration::from_secs(10)).unwrap();
        vosu_proptest::wait::JoinBounded::join_bounded(writer).unwrap();
        vosu_proptest::wait::JoinBounded::join_bounded(other).unwrap();
        let slot = store
            .sessions
            .read()
            .unwrap()
            .get(&SessionKey(7401))
            .unwrap()
            .clone();
        let closed = store.close_session(SessionKey(7401)).unwrap();
        assert!(slot.commit.lock().unwrap().closed);
        assert!(Arc::ptr_eq(&closed, &slot.current.load_full()));
        assert!(store.set_focused(SessionKey(7401), true).is_none());
    }

    #[test]
    fn incremental_usage_matches_full_json_after_updates() {
        let (store, grant) = fixture("test.store-accounting", SessionKey(7501));
        let values = [
            PublishValueRef::String("\0\né\\\""),
            PublishValueRef::Bytes(&[1, 2, 3, 4]),
            PublishValueRef::F64(f64::NAN),
            PublishValueRef::F64(1.25),
            PublishValueRef::Cbor(&[0xa1, 0x61, b'a', 0x82, 0xf5, 0x21]),
            PublishValueRef::Null,
        ];
        for (i, value) in values.into_iter().enumerate() {
            let pointer = format!("/nested/k{i}");
            store
                .commit(
                    &grant,
                    SessionKey(7501),
                    [PublishFieldRef {
                        handle: 7501,
                        ns: "test.store-accounting",
                        pointer: &pointer,
                        value,
                    }]
                    .into_iter(),
                )
                .unwrap();
            store.set_focused(SessionKey(7501), i % 2 == 0);
            store
                .commit(
                    &grant,
                    SessionKey(7501),
                    ["audio", "background"]
                        .map(|kind| {
                            let pointer = if kind == "audio" {
                                "/resources/audio"
                            } else {
                                "/resources/background"
                            };
                            PublishFieldRef {
                                handle: 7501,
                                ns: "vosu.chart",
                                pointer,
                                value: if i % 2 == 0 {
                                    PublishValueRef::String("ref")
                                } else {
                                    PublishValueRef::Null
                                },
                            }
                        })
                        .into_iter(),
                )
                .unwrap();
            let sessions = store.sessions.read().unwrap();
            let slot = sessions.get(&SessionKey(7501)).unwrap();
            let meta = slot.commit.lock().unwrap();
            for (ns, value) in slot.current.load().namespaces() {
                let mut json = Vec::new();
                value.write_json(&mut json);
                let usage = meta.usage.get(ns).unwrap();
                assert_eq!(usage.bytes, json.len() as i64);
                assert_eq!(usage.keys, Usage::measure(value).keys);
            }
        }
    }
}
