use super::*;
use std::sync::atomic::AtomicUsize;

struct Provider {
    live: Arc<AtomicUsize>,
    changes: Arc<AtomicUsize>,
    events: watch::Sender<u64>,
}
struct Lease {
    live: Arc<AtomicUsize>,
    changes: Arc<AtomicUsize>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.live.fetch_sub(1, Ordering::SeqCst);
    }
}
impl StateDemand for Lease {
    fn replace_filters(&self, filters: &[Filter]) -> Result<(), String> {
        if filters == [Filter::Field("reject".into())] {
            return Err("rejected".into());
        }
        self.changes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
impl StateProvider for Provider {
    fn poll_rate(&self, _: Schema) -> Duration {
        Duration::from_secs(1)
    }
    fn subscribe(&self, _: Schema) -> watch::Receiver<u64> {
        self.events.subscribe()
    }
    fn capture(&self, _: Schema) -> Option<Arc<dyn StateSnapshot>> {
        None
    }
    fn demand(&self, _: Schema, _: &[Filter]) -> Result<Option<Box<dyn StateDemand>>, String> {
        self.live.fetch_add(1, Ordering::SeqCst);
        Ok(Some(Box::new(Lease {
            live: self.live.clone(),
            changes: self.changes.clone(),
        })))
    }
}
fn setup() -> (WebSocketHub, Arc<Provider>) {
    let provider = Arc::new(Provider {
        live: Arc::default(),
        changes: Arc::default(),
        events: watch::channel(0).0,
    });
    (
        WebSocketHub::new(Schema::V2Precise, provider.clone()),
        provider,
    )
}
#[test]
fn receiver_loss_releases_only_its_own_demand() {
    let (hub, provider) = setup();
    let (a, receiver_a) = hub.connect();
    let (_b, _receiver_b) = hub.connect();
    let (_commands, _commands_receiver) = hub.connect_commands();
    assert_eq!(provider.live.load(Ordering::SeqCst), 2);
    drop(receiver_a);
    assert_eq!(provider.live.load(Ordering::SeqCst), 1);
    a.cleanup();
    assert_eq!(provider.live.load(Ordering::SeqCst), 1);
}
#[test]
fn filter_rejection_keeps_plan_and_accepted_replacement_rebalances() {
    let (hub, provider) = setup();
    let (connection, _receiver) = hub.connect();
    connection.set_filters(vec![Filter::Field("keys".into())]);
    let control = connection.control_handle();
    control.set_filters(vec![Filter::Field("reject".into())]);
    assert_eq!(
        connection.client.filters.load().filters,
        vec![Filter::Field("keys".into())]
    );
    assert_eq!(provider.changes.load(Ordering::SeqCst), 1);
    assert_eq!(provider.live.load(Ordering::SeqCst), 1);
    control.cleanup();
    assert_eq!(provider.live.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn shutdown_releases_demand_even_when_connections_are_retained() {
    let (hub, provider) = setup();
    let (_connection, _receiver) = hub.connect();
    tokio::time::timeout(Duration::from_secs(1), hub.close_all(GOING_AWAY))
        .await
        .unwrap();
    assert_eq!(provider.live.load(Ordering::SeqCst), 0);
    let (_rejected, _receiver) = hub.connect();
    assert_eq!(provider.live.load(Ordering::SeqCst), 0);
}
