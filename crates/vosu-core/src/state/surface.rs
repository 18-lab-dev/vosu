//! Surface registrations share the ABI subscription registry. Lock order is
//! focus -> instances -> sessions -> surfaces -> schema registry -> subscriptions. No plugin
//! callbacks run under these locks; demand delivery happens after releasing them.
use std::sync::Arc;

use tokio::sync::watch;
use vosu_plugin_api::{Selector, StateEntry};
use vosu_surface::state::{SessionTarget, SurfaceError, SurfaceUpdate};

use super::{
    Audience, InternalGrant, MAX_SELECTORS, Pointer, SchemaRegistry, SessionKey, SessionState,
    SubscribeError, SubscriberId, SubscriptionRegistry, project_select,
};

#[derive(Default)]
pub(crate) struct SurfaceReaders {
    pub next: u64,
    pub readers: Vec<SurfaceReader>,
}

pub(crate) struct SurfaceReader {
    pub id: u64,
    pub reader: String,
    pub target: SessionTarget,
    pub key: Option<SessionKey>,
    pub selectors: Vec<(Arc<str>, Pointer)>,
    pub grant: InternalGrant,
    pub updates: watch::Sender<Arc<SurfaceUpdate>>,
}

pub(crate) fn selectors(
    registry: &SchemaRegistry,
    selectors: &[Selector],
) -> Result<Vec<(Arc<str>, Pointer)>, SurfaceError> {
    if selectors.len() > MAX_SELECTORS {
        return Err(SurfaceError::TooManySelectors);
    }
    let parsed = selectors
        .iter()
        .map(|s| {
            Pointer::parse(&s.pointer)
                .map(|p| (Arc::from(s.ns.as_str()), p))
                .map_err(|e| SurfaceError::InvalidSelector(e.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    // Use precisely the ABI's namespace, overlap and selector-limit validation.
    SubscriptionRegistry::default()
        .replace(
            registry,
            SubscriberId::Surface(0),
            SessionKey(0),
            parsed.clone(),
            InternalGrant::none(),
        )
        .map_err(error)?;
    Ok(parsed)
}

pub(crate) fn error(error: SubscribeError) -> SurfaceError {
    match error {
        SubscribeError::TooManySelectors | SubscribeError::TooManySubscriptions => {
            SurfaceError::TooManySelectors
        }
        other => SurfaceError::InvalidSelector(other.to_string()),
    }
}

pub(crate) fn selection(
    registry: &SchemaRegistry,
    state: Option<&SessionState>,
    selectors: &[(Arc<str>, Pointer)],
    grant: &InternalGrant,
) -> Vec<StateEntry> {
    selectors
        .iter()
        .map(|(ns, pointer)| StateEntry {
            ns: ns.to_string(),
            pointer: pointer.to_string(),
            value: state
                .and_then(|s| project_select(registry, s, ns, pointer, Audience::Plugin(grant)))
                .map(|v| v.to_cbor_value()),
            error: None,
        })
        .collect()
}
