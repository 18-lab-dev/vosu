//! Session-local union of legacy and state-bus sampling demand.
use tokio::{runtime::Handle, sync::watch};
use vosu_osu_state::SubscriptionDemand;

#[derive(Clone, Copy, Default)]
pub(crate) struct BusDemand {
    pub lanes: SubscriptionDemand,
    pub collect_errors: bool,
}
pub struct SessionDemand {
    bus: watch::Sender<BusDemand>,
    out: watch::Receiver<SubscriptionDemand>,
    task: tokio::task::JoinHandle<()>,
}
impl SessionDemand {
    pub fn new(mut hub: watch::Receiver<SubscriptionDemand>, rt: &Handle) -> Self {
        let (bus, mut bus_rx) = watch::channel(BusDemand::default());
        let (out, receiver) = watch::channel(*hub.borrow_and_update());
        let task = rt.spawn(async move {
            let mut hub_open = true;
            loop {
                tokio::select! {
                    changed = hub.changed(), if hub_open => { hub_open = changed.is_ok(); }
                    changed = bus_rx.changed() => { if changed.is_err() { break; } }
                }
                let value = (*hub.borrow_and_update()).union(bus_rx.borrow_and_update().lanes);
                out.send_if_modified(|current| {
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
            task,
        }
    }
    pub fn set_bus(&self, regular: bool, precise: bool) {
        self.set_bus_collection(regular, precise, precise);
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
        self.task.abort();
    }
}
impl Drop for SessionDemand {
    fn drop(&mut self) {
        self.task.abort();
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
        tokio::time::timeout(Duration::from_secs(5), out.changed())
            .await
            .unwrap()
            .unwrap_err();
    }
}
