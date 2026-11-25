//! Connection leases share one full-image watch, so histories are never copied
//! into a separate bus mailbox for each otherwise identical transport client.
use super::*;
use std::collections::HashMap;
use vosu_plugin_api::Selector;

pub(super) struct Registry {
    adapter: Arc<Adapter>,
    requester: Arc<dyn BundledRequester>,
    events: watch::Sender<u64>,
    state: Mutex<State>,
}
#[derive(Default)]
struct State {
    next: u64,
    plans: HashMap<u64, Vec<Selector>>,
    selectors: Vec<Selector>,
    source: Option<SurfaceSubscription>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}
struct Lease {
    registry: Arc<Registry>,
    id: u64,
    retained: Mutex<Option<Box<dyn Send + Sync>>>,
}
impl Registry {
    pub(super) fn new(
        adapter: Arc<Adapter>,
        requester: Arc<dyn BundledRequester>,
        events: watch::Sender<u64>,
    ) -> Arc<Self> {
        Arc::new(Self {
            adapter,
            requester,
            events,
            state: Mutex::new(State::default()),
        })
    }
    pub(super) fn acquire(
        self: &Arc<Self>,
        filters: &[Filter],
    ) -> Result<Box<dyn StateDemand>, String> {
        let selectors = self.adapter.demand_selectors(filters);
        let mut state = self.state.lock().unwrap();
        let id = state.next;
        let union = union(&state, id, &selectors);
        self.replace(&mut state, union)?;
        state.next = state.next.wrapping_add(1);
        state.plans.insert(id, selectors);
        drop(state);
        Ok(Box::new(Lease {
            registry: Arc::clone(self),
            id,
            retained: Mutex::new(self.adapter.retain(filters)),
        }))
    }
    fn replace(&self, state: &mut State, selectors: Vec<Selector>) -> Result<(), String> {
        if let Some(source) = &state.source {
            if state.selectors != selectors {
                source
                    .replace_selectors(selectors.clone())
                    .map_err(|e| e.to_string())?;
            }
        } else {
            let source = self
                .requester
                .subscribe(SessionTarget::Focused, selectors.clone())
                .map_err(|e| e.to_string())?;
            state
                .tasks
                .push(forward(source.updates.clone(), self.events.clone()));
            if let Some(changes) = self.adapter.changes() {
                state.tasks.push(forward(changes, self.events.clone()));
            }
            state.source = Some(source);
        }
        state.selectors = selectors;
        Ok(())
    }
}
fn union(state: &State, replacing: u64, additional: &[Selector]) -> Vec<Selector> {
    let mut selectors = Vec::new();
    for selector in state
        .plans
        .iter()
        .filter(|(id, _)| **id != replacing)
        .flat_map(|(_, selectors)| selectors)
        .chain(additional)
    {
        if !selectors.contains(selector) {
            selectors.push(selector.clone());
        }
    }
    selectors.sort_by(|a, b| (&a.ns, &a.pointer).cmp(&(&b.ns, &b.pointer)));
    selectors
}
impl StateDemand for Lease {
    fn replace_filters(&self, filters: &[Filter]) -> Result<(), String> {
        let mut retained = self.retained.lock().unwrap();
        let selectors = self.registry.adapter.demand_selectors(filters);
        let mut state = self.registry.state.lock().unwrap();
        let union = union(&state, self.id, &selectors);
        self.registry.replace(&mut state, union)?;
        state.plans.insert(self.id, selectors);
        drop(state);
        *retained = self.registry.adapter.retain(filters);
        Ok(())
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = self.registry.state.lock().unwrap();
        state.plans.remove(&self.id);
        if state.plans.is_empty() {
            let source = state.source.take();
            state.selectors.clear();
            let tasks = std::mem::take(&mut state.tasks);
            drop(state);
            for task in tasks {
                task.abort();
            }
            drop(source);
        } else {
            let union = union(&state, self.id, &[]);
            if let Err(error) = self.registry.replace(&mut state, union) {
                // A dead/revoked requester has already lost its host demand.
                // Preserve other clients' registrations on any rejection.
                tracing::debug!(%error, "surface demand withdrawal rejected");
            }
        }
    }
}
fn forward<T: Send + Sync + 'static>(
    mut source: watch::Receiver<T>,
    target: watch::Sender<u64>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while source.changed().await.is_ok() {
            target.send_modify(|revision| *revision = revision.wrapping_add(1));
        }
    })
}
