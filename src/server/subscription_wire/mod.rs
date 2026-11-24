//! Shared selected-field subscription wire mechanics; hubs own scheduling and demand.

mod frames;
mod mailbox;
mod pointer;

pub use frames::{Extraction, FrameArena, Selected, changes, error_frame, store_baseline};
pub use mailbox::{Mailbox, Outbound, Receiver};
pub use pointer::{Selection, decode_pointer, extract, traverse};

#[cfg(test)]
pub(crate) use frames::{FRAME_ARENA_CAPACITY, initial_frame, update_frame};
#[cfg(test)]
pub(crate) use pointer::{LINEAR_KEY_SEARCH_MAX, parse_array_index};

pub const INVALID_REQUEST: u16 = 1;
pub const INVALID_POINTER: u16 = 2;
pub const SUBSCRIPTION_TIMEOUT: u16 = 3;
pub const SERIALIZATION_FAILED: u16 = 4;
pub const ALREADY_SUBSCRIBED: u16 = 5;

pub const INVALID_REQUEST_MESSAGE: &str = "invalid subscription request";
pub const INVALID_POINTER_MESSAGE: &str = "invalid JSON Pointer";
pub const SUBSCRIPTION_TIMEOUT_MESSAGE: &str = "subscription timed out";
pub const SERIALIZATION_FAILED_MESSAGE: &str = "state serialization failed";
pub const ALREADY_SUBSCRIBED_MESSAGE: &str = "subscription already established";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubscriptionRequestError {
    InvalidRequest,
    InvalidPointer,
    AlreadySubscribed,
}

impl SubscriptionRequestError {
    pub const fn code(self) -> u16 {
        match self {
            Self::InvalidRequest => INVALID_REQUEST,
            Self::InvalidPointer => INVALID_POINTER,
            Self::AlreadySubscribed => ALREADY_SUBSCRIBED,
        }
    }

    pub const fn message(self) -> &'static str {
        match self {
            Self::InvalidRequest => INVALID_REQUEST_MESSAGE,
            Self::InvalidPointer => INVALID_POINTER_MESSAGE,
            Self::AlreadySubscribed => ALREADY_SUBSCRIBED_MESSAGE,
        }
    }
}

/// Parses one complete client text message as a selector request.
pub fn parse_subscription_request(data: &[u8]) -> Result<Vec<String>, SubscriptionRequestError> {
    serde_json::from_slice(data).map_err(|_| SubscriptionRequestError::InvalidRequest)
}
