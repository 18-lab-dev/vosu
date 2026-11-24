//! Instance-owned stable roster. Handles identify generations, never PIDs.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};
use vosu_osu_state::StateBundleSnapshot;
use crate::publish::{BusPublisher, SessionHost};

type Aggregate = crate::project::StableTourney;
struct Entry {
    manager: bool,
    selected: Arc<std::sync::atomic::AtomicBool>,
    settings: vosu_osu_model::launch::InstanceSettings,
    bus: Weak<BusPublisher<SessionHost>>,
    snapshot: Option<Arc<StateBundleSnapshot>>,
}
#[derive(Default)]
struct Roster {
    entries: BTreeMap<u64, Entry>,
    revision: u64,
}
#[derive(Default)]
pub(crate) struct Tournament(Mutex<Roster>);
impl Tournament {
    pub fn attach(&self, bus: &Arc<BusPublisher<SessionHost>>, handle: u64, manager: bool, settings: vosu_osu_model::launch::InstanceSettings, selected: Arc<std::sync::atomic::AtomicBool>) {
        let mut roster = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !bus.identified.load(std::sync::atomic::Ordering::Acquire) { return; }
        roster.entries.insert(handle, Entry {
            manager,
            selected,
            settings,
            bus: Arc::downgrade(bus),
            snapshot: None,
        });
        let update = roster.update();
        drop(roster);
        broadcast(update);
    }
    pub fn detach(&self, handle: u64) {
        let mut roster = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if roster.entries.remove(&handle).is_none() { return; }
        let update = roster.update();
        drop(roster);
        broadcast(update);
    }
    pub fn focus_changed(&self) {
        let mut roster = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if roster.entries.values().filter(|entry| entry.manager).count() < 2 { return; }
        let update = roster.update();
        drop(roster);
        broadcast(update);
    }
    pub fn observe(&self, handle: u64, snapshot: &Arc<StateBundleSnapshot>) {
        let mut roster = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(entry) = roster.entries.get_mut(&handle) else { return; };
        if !entry.manager && !entry.settings.tournament_spectator { return; }
        if entry.snapshot.as_ref().is_some_and(|old| Arc::ptr_eq(&old.regular, &snapshot.regular) && Arc::ptr_eq(&old.precise, &snapshot.precise)) { return; }
        entry.snapshot = Some(snapshot.clone());
        let update = roster.update();
        drop(roster);
        broadcast(update);
    }
}
type Update = (Aggregate, Vec<Arc<BusPublisher<SessionHost>>>);
impl Roster {
    fn update(&mut self) -> Update {
        self.revision += 1;
        let manager = self.entries.values().find(|entry| entry.manager && entry.selected.load(std::sync::atomic::Ordering::Acquire))
            .or_else(|| self.entries.values().find(|entry| entry.manager));
        let mut clients: Vec<_> = self.entries.values().filter(|entry| entry.settings.tournament_spectator).collect();
        clients.sort_by(|a, b| a.settings.tournament_ipc_id.partial_cmp(&b.settings.tournament_ipc_id).unwrap_or(std::cmp::Ordering::Equal));
        let pending = manager.is_some_and(|entry| entry.snapshot.is_none()) || (manager.is_some() && clients.iter().any(|entry| entry.snapshot.is_none()));
        let clients: Vec<_> = clients.iter().filter_map(|entry| Some((entry.settings.tournament_ipc_id?, entry.snapshot.as_deref()?))).collect();
        let bytes = if pending { vec![0xf6] } else { crate::project::stable_tourney(manager.and_then(|entry| entry.snapshot.as_deref()), &clients) };
        (Aggregate { revision: self.revision, bytes: bytes.into(), availability: if pending { "pending" } else { "ready" } }, self.entries.values().filter_map(|entry| entry.bus.upgrade()).collect())
    }
}
fn broadcast((aggregate, buses): Update) {
    // No roster lock across host calls. Each bus rejects a late older revision.
    for bus in buses { bus.set_tournament(aggregate.clone()); }
}

#[cfg(test)]
mod tests;
