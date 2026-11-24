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
    pub roster_revision: u64,
    pub last_roster: Vec<vosu_surface::state::IdentifiedSession>,
    pub focus_revision: u64,
    pub last_focus: Option<String>,
    pub routes: Vec<SurfaceRoute>,
    pub next_route: u64,
    pub management: vosu_surface::management::ManagementAuthority,
}

pub(crate) struct SurfaceReader {
    pub id: u64,
    pub reader: String,
    pub target: SessionTarget,
    pub key: Option<SessionKey>,
    pub selectors: Vec<(Arc<str>, Pointer)>,
    pub grant: InternalGrant,
    pub read_fields: Option<Vec<String>>,
    pub updates: watch::Sender<Arc<SurfaceUpdate>>,
}

pub(crate) struct SurfaceRoute {
    pub id: u64,
    pub owner: u64,
    pub reader: String,
    pub descriptor: vosu_surface::routes::RouteDescriptor,
    pub provider: Arc<dyn vosu_surface::routes::RouteProvider>,
    pub feeds: Vec<std::sync::Weak<vosu_surface::routes::FeedState>>,
}

impl Drop for SurfaceRoute {
    fn drop(&mut self) {
        for feed in self.feeds.iter().filter_map(std::sync::Weak::upgrade) {
            feed.close();
        }
    }
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

pub(crate) fn demand_selectors(
    selectors: &[(Arc<str>, Pointer)],
    fields: Option<&[String]>,
) -> Vec<(Arc<str>, Pointer)> {
    let Some(fields) = fields else {
        return selectors.to_vec();
    };
    let mut selected: Vec<(Arc<str>, Pointer)> = Vec::new();
    for (ns, pointer) in selectors {
        for field in fields {
            let Some(grant) = vosu_plugin_api::field_to_pointer(ns, field)
                .and_then(|value| Pointer::parse(&value).ok())
            else {
                continue;
            };
            let intersection = if pointer.tokens().starts_with(grant.tokens()) {
                pointer.clone()
            } else if grant.tokens().starts_with(pointer.tokens()) {
                grant
            } else {
                continue;
            };
            if selected.iter().any(|(name, existing)| {
                name == ns && intersection.tokens().starts_with(existing.tokens())
            }) {
                continue;
            }
            selected.retain(|(name, existing)| {
                name != ns || !existing.tokens().starts_with(intersection.tokens())
            });
            selected.push((ns.clone(), intersection));
        }
    }
    selected
}

pub(crate) fn selection_granted(
    registry: &SchemaRegistry,
    state: Option<&SessionState>,
    selectors: &[(Arc<str>, Pointer)],
    grant: &InternalGrant,
    fields: Option<&[String]>,
) -> Vec<StateEntry> {
    selectors
        .iter()
        .map(|(ns, pointer)| {
            let value = state.and_then(|state| {
                project_select(registry, state, ns, pointer, Audience::Plugin(grant))
            });
            let value = value.and_then(|value| {
                let Some(fields) = fields else {
                    return Some(value.to_cbor_value());
                };
                let grants: Vec<_> = fields
                    .iter()
                    .filter_map(|field| vosu_plugin_api::field_to_pointer(ns, field))
                    .filter_map(|value| Pointer::parse(&value).ok())
                    .collect();
                prune_public(&value, &mut pointer.tokens().to_vec(), &grants)
            });
            StateEntry {
                ns: ns.to_string(),
                pointer: pointer.to_string(),
                value,
                error: None,
            }
        })
        .collect()
}

fn prune_public(
    value: &super::StateValue,
    path: &mut Vec<Box<str>>,
    grants: &[Pointer],
) -> Option<vosu_plugin_api::CborValue> {
    if grants.iter().any(|grant| path.starts_with(grant.tokens())) {
        return Some(value.to_cbor_value());
    }
    if !grants.iter().any(|grant| grant.tokens().starts_with(path)) {
        return None;
    }
    let super::StateValue::Object(object) = value else {
        return None;
    };
    let entries: Vec<_> = object
        .iter()
        .filter_map(|(name, child)| {
            path.push(name.into());
            let child = prune_public(child, path, grants);
            path.pop();
            child.map(|child| (vosu_plugin_api::CborValue::Text(name.into()), child))
        })
        .collect();
    (!entries.is_empty()).then_some(vosu_plugin_api::CborValue::Map(entries))
}
