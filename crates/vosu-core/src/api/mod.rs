//! Public API audience adapter. Reads are demand-free; subscriptions own their
//! demand through a non-owning RAII guard. No bundled plugin identity is used.
mod sessions;
mod state;
pub(crate) mod subscriptions;

pub use sessions::SessionTarget;
pub use state::{ApiStateSource, ApiUpdate};
pub use subscriptions::{ApiError, ApiPlan, ApiSubscription};

pub(crate) use state::ApiStateHost;
pub(crate) use subscriptions::{ApiReader, ApiReaders};
