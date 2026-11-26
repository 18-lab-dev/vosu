//! Session events use the same immutable selection and framing as session state.
use super::subscriptions::{ApiV1SubscriptionConnection, ApiV1SubscriptionHub};
use crate::server::subscription_wire::Receiver;

impl ApiV1SubscriptionHub {
    pub fn connect_sessions(&self) -> (ApiV1SubscriptionConnection, Receiver) {
        self.open(None)
    }
}
