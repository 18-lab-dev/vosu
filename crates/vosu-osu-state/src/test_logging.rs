use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::{Layer, Registry, layer::Context, prelude::*};

struct Errors(Arc<AtomicUsize>);
impl<S: Subscriber> Layer<S> for Errors {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        if *event.metadata().level() == Level::ERROR {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
}
pub(crate) fn errors() -> (impl Subscriber + Send + Sync, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    (Registry::default().with(Errors(count.clone())), count)
}
