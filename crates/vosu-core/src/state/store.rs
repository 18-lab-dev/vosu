//! Lock order: sessions -> slot commit. Schema reads are only needed during
//! open, before sessions is acquired. No plugin code or callbacks run under locks.
//! ArcSwap readers never take the slot commit mutex; watch retains one revision.
use super::commit::{empty_object, validate, write_path};
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
    commit: Mutex<()>,
    current: ArcSwap<SessionState>,
    revisions: watch::Sender<u64>,
}

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
        let current = Arc::new(SessionState::new(key, 0, namespaces));
        let (revisions, _) = watch::channel(0);
        sessions.insert(
            key,
            Arc::new(SessionSlot {
                owner: grant.plugin_id.clone(),
                commit: Mutex::new(()),
                current: ArcSwap::from(current.clone()),
                revisions,
            }),
        );
        current
    }

    pub fn close_session(&self, key: SessionKey) -> Option<Arc<SessionState>> {
        self.sessions
            .write()
            .unwrap()
            .remove(&key)
            .map(|slot| slot.current.load_full())
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
        // Keeping the read guard through publication serializes close against commit,
        // without retaining detached handles or an unbounded tombstone map.
        let sessions = self.sessions.read().unwrap();
        let slot = sessions.get(&key).ok_or(CommitError::UnknownSession)?;
        if slot.owner != grant.plugin_id {
            return Err(CommitError::NotOwner);
        }
        let _commit = slot.commit.lock().unwrap();
        let previous = slot.current.load_full();
        let validated = validate(grant, &previous, entries)?;
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
            let remove = entry.ns.as_ref() == "vosu.chart"
                && entry.pointer.tokens().len() == 2
                && entry.pointer.tokens()[0].as_ref() == "resources"
                && matches!(entry.value, StateValue::Null);
            write_path(
                &mut namespaces[index].1,
                entry.pointer.tokens(),
                entry.value,
                remove,
            );
            touched.push((entry.ns, entry.pointer));
        }
        Ok(publish(slot, previous, namespaces, touched))
    }

    pub fn set_focused(&self, key: SessionKey, focused: bool) -> Option<CommitOutcome> {
        let sessions = self.sessions.read().unwrap();
        let slot = sessions.get(&key)?;
        let _commit = slot.commit.lock().unwrap();
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
        write_path(client, pointer.tokens(), StateValue::Bool(focused), false);
        Some(publish(
            slot,
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
