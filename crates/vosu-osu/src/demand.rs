//! Session-local union of legacy, direct bus and tournament aggregate demand.
use tokio::{runtime::Handle, sync::watch};
use vosu_osu_state::SubscriptionDemand;

/// Aggregate precision is demanded only by selectors covering precise client
/// facts, never by an unrelated precise selector in the same subscription.
pub(crate) fn tournament_lanes(
    selectors: &[vosu_plugin_api::FieldSelector],
    regular: bool,
    precise: bool,
) -> (bool, bool) {
    let mut lanes = (false, false);
    for selector in selectors.iter().filter(|s| s.ns == "vosu.osu") {
        let path = selector.pointer.as_str();
        if path == "/tourneyAvailability" {
            lanes.0 = true;
        } else if path.is_empty() || path == "/tourney" || path.starts_with("/tourney/") {
            let client = path.strip_prefix("/tourney/clients");
            let precise_only = client.is_some_and(|path| {
                path.contains("/keysOverlay")
                    || path.contains("/play/hitErrors")
                    || path.contains("/play/preciseUnstableRate")
            });
            let client_parent = client.is_some_and(|path| {
                path.is_empty() || path.split('/').count() <= 2 || path.ends_with("/play")
            });
            lanes.0 |= !precise_only;
            lanes.1 |= precise_only || client_parent || path.is_empty() || path == "/tourney";
        }
    }
    (lanes.0 && regular, lanes.1 && precise)
}

#[derive(Clone, Copy, Default)]
pub(crate) struct BusDemand {
    pub lanes: SubscriptionDemand,
    pub collect_errors: bool,
}
/// One instance-owned roster input/output pair. The roster resets its output on
/// unsubscribe, manager loss and detach; SessionDemand also resets on shutdown.
#[derive(Clone)]
pub(crate) struct TournamentDemand {
    requested: std::sync::Arc<std::sync::atomic::AtomicU8>,
    effective: watch::Sender<SubscriptionDemand>,
}
impl Default for TournamentDemand {
    fn default() -> Self {
        Self {
            requested: Default::default(),
            effective: watch::channel(SubscriptionDemand::default()).0,
        }
    }
}
impl TournamentDemand {
    pub(crate) fn request(&self, regular: bool, precise: bool) {
        self.requested.store(
            u8::from(regular) | (u8::from(precise) << 1),
            std::sync::atomic::Ordering::Release,
        );
    }
    pub(crate) fn requested(&self) -> SubscriptionDemand {
        let bits = self.requested.load(std::sync::atomic::Ordering::Acquire);
        SubscriptionDemand {
            regular: bits & 1 != 0,
            precise: bits & 2 != 0,
            ..SubscriptionDemand::default()
        }
    }
    pub(crate) fn set_effective(&self, demand: SubscriptionDemand) {
        self.effective.send_if_modified(|current| {
            if *current == demand {
                false
            } else {
                *current = demand;
                true
            }
        });
    }
}
pub struct SessionDemand {
    bus: watch::Sender<BusDemand>,
    out: watch::Receiver<SubscriptionDemand>,
    total: watch::Sender<SubscriptionDemand>,
    pub(crate) tournament: TournamentDemand,
    task: tokio::task::JoinHandle<()>,
    alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl SessionDemand {
    pub fn new(mut hub: watch::Receiver<SubscriptionDemand>, rt: &Handle) -> Self {
        let (bus, mut bus_rx) = watch::channel(BusDemand::default());
        let (out, receiver) = watch::channel(*hub.borrow_and_update());
        let total = out.clone();
        let tournament = TournamentDemand::default();
        let mut aggregate = tournament.effective.subscribe();
        let alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let task_alive = alive.clone();
        let task = rt.spawn(async move {
            let mut hub_open = true;
            loop {
                tokio::select! {
                    changed = hub.changed(), if hub_open => { hub_open = changed.is_ok(); }
                    changed = bus_rx.changed() => { if changed.is_err() { break; } }
                    changed = aggregate.changed() => { if changed.is_err() { break; } }
                }
                let value = (*hub.borrow_and_update())
                    .union(bus_rx.borrow_and_update().lanes)
                    .union(*aggregate.borrow_and_update());
                out.send_if_modified(|current| {
                    if !task_alive.load(std::sync::atomic::Ordering::Acquire) {
                        return false;
                    }
                    if *current == value {
                        false
                    } else {
                        *current = value;
                        true
                    }
                });
            }
        });
        Self {
            bus,
            out: receiver,
            total,
            tournament,
            task,
            alive,
        }
    }
    pub fn set_bus(&self, regular: bool, precise: bool) {
        self.set_bus_collection(regular, precise, false);
    }
    pub fn receiver(&self) -> watch::Receiver<SubscriptionDemand> {
        self.out.clone()
    }
    pub(crate) fn set_bus_collection(&self, regular: bool, precise: bool, collect_errors: bool) {
        self.bus.send_replace(BusDemand {
            lanes: SubscriptionDemand {
                regular,
                precise,
                ..SubscriptionDemand::default()
            },
            collect_errors,
        });
    }
    pub(crate) fn bus_receiver(&self) -> watch::Receiver<BusDemand> {
        self.bus.subscribe()
    }
    pub(crate) fn cancel(&self) {
        self.alive
            .store(false, std::sync::atomic::Ordering::Release);
        self.tournament.request(false, false);
        self.tournament.set_effective(SubscriptionDemand::default());
        self.total.send_replace(SubscriptionDemand::default());
        self.task.abort();
    }
}
impl Drop for SessionDemand {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    async fn next(
        receiver: &mut watch::Receiver<SubscriptionDemand>,
        expected: SubscriptionDemand,
    ) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if *receiver.borrow_and_update() == expected {
                    break;
                }
                receiver.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn union_preserves_all_hub_bits_and_removes_bus_only_bits() {
        let hub_value = SubscriptionDemand {
            regular: true,
            gameplay_precise: true,
            ..SubscriptionDemand::default()
        };
        let (hub, receiver) = watch::channel(hub_value);
        let demand = SessionDemand::new(receiver, &Handle::current());
        let mut out = demand.receiver();
        assert_eq!(*out.borrow_and_update(), hub_value);
        demand.set_bus(false, true);
        next(
            &mut out,
            SubscriptionDemand {
                precise: true,
                ..hub_value
            },
        )
        .await;
        hub.send_replace(SubscriptionDemand {
            gameplay_regular: true,
            ..SubscriptionDemand::default()
        });
        next(
            &mut out,
            SubscriptionDemand {
                precise: true,
                gameplay_regular: true,
                ..SubscriptionDemand::default()
            },
        )
        .await;
        demand.set_bus(false, false);
        next(
            &mut out,
            SubscriptionDemand {
                gameplay_regular: true,
                ..SubscriptionDemand::default()
            },
        )
        .await;
        drop(demand);
        assert_eq!(*out.borrow_and_update(), SubscriptionDemand::default());
        tokio::time::timeout(Duration::from_secs(5), out.changed())
            .await
            .unwrap()
            .unwrap_err();
    }
    #[test]
    fn tournament_selectors_do_not_borrow_unrelated_precision() {
        let select = |paths: &[&str]| {
            paths
                .iter()
                .map(|path| vosu_plugin_api::FieldSelector {
                    ns: "vosu.osu".into(),
                    pointer: (*path).into(),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            tournament_lanes(&select(&["/tourney/team", "/play/hitOffsets"]), true, true),
            (true, false)
        );
        assert_eq!(
            tournament_lanes(
                &select(&["/tourney/clients/1/play/hitErrors/all"]),
                false,
                true
            ),
            (false, true)
        );
        assert_eq!(
            tournament_lanes(&select(&["/tourney/clients/1/play/score"]), true, true),
            (true, false)
        );
        assert_eq!(
            tournament_lanes(&select(&["/tourney/clients/1"]), true, true),
            (true, true)
        );
        assert_eq!(
            tournament_lanes(&select(&["/tourney"]), true, true),
            (true, true)
        );
        assert_eq!(tournament_lanes(&select(&[]), true, true), (false, false));
    }
}
