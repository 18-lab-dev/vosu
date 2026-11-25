//! Session-local direct bus and tournament aggregate demand; no forwarding task.
use tokio::sync::watch;
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
    pub collect_errors: bool,
}
/// One instance-owned roster input/output pair. The roster resets its output on
/// unsubscribe, manager loss and detach; SessionDemand also resets on shutdown.
#[derive(Clone, Default)]
pub(crate) struct TournamentDemand {
    requested: std::sync::Arc<std::sync::atomic::AtomicU8>,
    effective: std::sync::Arc<std::sync::Mutex<EffectiveDemand>>,
}
#[derive(Default)]
struct EffectiveDemand {
    direct: SubscriptionDemand,
    tournament: SubscriptionDemand,
    cancelled: bool,
    out: Option<watch::Sender<SubscriptionDemand>>,
}
impl EffectiveDemand {
    fn publish(&self) {
        if let Some(out) = &self.out {
            let value = if self.cancelled {
                SubscriptionDemand::default()
            } else {
                self.direct.union(self.tournament)
            };
            out.send_if_modified(|current| {
                if *current == value {
                    false
                } else {
                    *current = value;
                    true
                }
            });
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
        let mut effective = self.effective.lock().unwrap();
        effective.tournament = demand;
        effective.publish();
    }
}
pub struct SessionDemand {
    bus: watch::Sender<BusDemand>,
    out: watch::Receiver<SubscriptionDemand>,
    pub(crate) tournament: TournamentDemand,
}
impl Default for SessionDemand {
    fn default() -> Self {
        Self::new()
    }
}
impl SessionDemand {
    pub fn new() -> Self {
        let (bus, _) = watch::channel(BusDemand::default());
        let (out, receiver) = watch::channel(SubscriptionDemand::default());
        let tournament = TournamentDemand::default();
        tournament.effective.lock().unwrap().out = Some(out);
        Self {
            bus,
            out: receiver,
            tournament,
        }
    }
    pub fn set_bus(&self, regular: bool, precise: bool) {
        self.set_bus_collection(regular, precise, false);
    }
    pub fn receiver(&self) -> watch::Receiver<SubscriptionDemand> {
        self.out.clone()
    }
    pub(crate) fn set_bus_collection(&self, regular: bool, precise: bool, collect_errors: bool) {
        self.bus.send_replace(BusDemand { collect_errors });
        let mut effective = self.tournament.effective.lock().unwrap();
        effective.direct = SubscriptionDemand {
            regular,
            precise,
            ..SubscriptionDemand::default()
        };
        effective.publish();
    }
    pub(crate) fn bus_receiver(&self) -> watch::Receiver<BusDemand> {
        self.bus.subscribe()
    }
    pub(crate) fn cancel(&self) {
        self.tournament.request(false, false);
        let mut effective = self.tournament.effective.lock().unwrap();
        effective.cancelled = true;
        effective.publish();
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
    async fn precise_bus_and_tournament_demand_release_independently_and_cancel_synchronously() {
        let demand = SessionDemand::new();
        let mut out = demand.receiver();
        demand.set_bus(false, true);
        next(
            &mut out,
            SubscriptionDemand {
                precise: true,
                ..Default::default()
            },
        )
        .await;
        demand.tournament.set_effective(SubscriptionDemand {
            regular: true,
            ..Default::default()
        });
        next(
            &mut out,
            SubscriptionDemand {
                regular: true,
                precise: true,
                ..Default::default()
            },
        )
        .await;
        demand.set_bus(false, false);
        next(
            &mut out,
            SubscriptionDemand {
                regular: true,
                ..Default::default()
            },
        )
        .await;
        demand.cancel();
        assert_eq!(*out.borrow_and_update(), SubscriptionDemand::default());
        demand.set_bus(true, true);
        demand.tournament.set_effective(SubscriptionDemand {
            precise: true,
            ..Default::default()
        });
        assert_eq!(*out.borrow_and_update(), SubscriptionDemand::default());
        drop(demand);
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
