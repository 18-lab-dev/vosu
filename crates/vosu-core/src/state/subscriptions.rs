//! Pure subscription plans. The host validates session visibility before replacement.
use std::sync::Arc;

use vosu_plugin_api::StateEntry;

use super::{
    Audience, CommitOutcome, DemandSnapshot, InternalGrant, LaneBits, Pointer, SchemaRegistry,
    SessionKey, SessionState, StateValue, project_select,
};

pub const MAX_SELECTORS: usize = 256;
pub const MAX_SUBSCRIPTIONS_PER_SUBSCRIBER: usize = 256;
/// Bounds both accumulated demand and the total registry, including surface readers.
pub const MAX_SUBSCRIPTIONS: usize = 1024;
pub const MAX_NAMESPACE_BYTES: usize = 128;

/// Keeps the complete flat DEMAND_CHANGED snapshot below the 1 MiB cap,
/// even at the maximum namespace/pointer lengths.
pub const MAX_DEMAND_SELECTORS: usize = 1024;
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SubscriberId {
    Plugin(u64),
    Surface(u64),
    Api(u64),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubscribeError {
    #[error("at most 256 selectors are allowed")]
    TooManySelectors,
    #[error("subscription registry capacity reached")]
    TooManySubscriptions,
    #[error("invalid or reserved namespace")]
    InvalidNamespace,
    #[error("duplicate or overlapping selectors")]
    Overlap,
}

struct Subscription {
    who: SubscriberId,
    session: SessionKey,
    selectors: Vec<(Arc<str>, Pointer)>,
    wire_selectors: Vec<(Arc<str>, Arc<str>)>,
    grant: InternalGrant,
    lanes: LaneBits,
    /// Per-selector lanes, parallel to `selectors`, so a namespace-limited
    /// demand needs no schema lookups on the commit path.
    selector_lanes: Vec<LaneBits>,
}

#[derive(Default)]
pub struct SubscriptionRegistry {
    subscriptions: Vec<Subscription>,
}

impl SubscriptionRegistry {
    pub fn replace(
        &mut self,
        reg: &SchemaRegistry,
        who: SubscriberId,
        session: SessionKey,
        selectors: Vec<(Arc<str>, Pointer)>,
        grant: InternalGrant,
    ) -> Result<(), SubscribeError> {
        let limit = if matches!(who, SubscriberId::Api(_)) {
            MAX_DEMAND_SELECTORS
        } else {
            MAX_SELECTORS
        };
        if selectors.len() > limit {
            return Err(SubscribeError::TooManySelectors);
        }
        for (i, (ns, p)) in selectors.iter().enumerate() {
            if ns.is_empty()
                || ns.len() > MAX_NAMESPACE_BYTES
                || ns.as_ref() == super::SESSIONS_NAMESPACE
                || !ns
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
            {
                return Err(SubscribeError::InvalidNamespace);
            }
            if selectors[..i]
                .iter()
                .any(|(other, q)| ns == other && p.overlaps(q))
            {
                return Err(SubscribeError::Overlap);
            }
        }
        let existing = self
            .subscriptions
            .iter()
            .position(|s| s.who == who && s.session == session);
        if selectors.is_empty() {
            if let Some(i) = existing {
                self.subscriptions.remove(i);
            }
            return Ok(());
        }
        if existing.is_none()
            && (self.subscriptions.len() >= MAX_SUBSCRIPTIONS
                || self.subscriptions.iter().filter(|s| s.who == who).count()
                    >= MAX_SUBSCRIPTIONS_PER_SUBSCRIBER)
        {
            return Err(SubscribeError::TooManySubscriptions);
        }
        let mut demand_keys = Vec::new();
        for s in self
            .subscriptions
            .iter()
            .filter(|s| s.session == session && s.who != who)
        {
            demand_keys.extend(s.selectors.iter().map(|(ns, p)| (ns, p.tokens())));
        }
        demand_keys.extend(selectors.iter().map(|(ns, p)| (ns, p.tokens())));
        demand_keys.sort_unstable();
        demand_keys.dedup();
        if demand_keys.len() > MAX_DEMAND_SELECTORS {
            return Err(SubscribeError::TooManySelectors);
        }
        let selector_lanes: Vec<_> = selectors
            .iter()
            .map(|selector| super::demand::compile(reg, std::slice::from_ref(selector)))
            .collect();
        let lanes = selector_lanes
            .iter()
            .fold(LaneBits::default(), |bits, lanes| bits.union(*lanes));
        let subscription = Subscription {
            who,
            session,
            wire_selectors: selectors
                .iter()
                .map(|(ns, p)| (Arc::clone(ns), Arc::from(p.to_string())))
                .collect(),
            selectors,
            grant,
            lanes,
            selector_lanes,
        };
        if let Some(i) = existing {
            self.subscriptions[i] = subscription;
        } else {
            self.subscriptions.push(subscription);
        }
        Ok(())
    }

    pub fn remove_subscriber(&mut self, who: SubscriberId) -> Vec<SessionKey> {
        let mut removed = Vec::new();
        self.subscriptions.retain(|s| {
            if s.who != who {
                return true;
            }
            removed.push(s.session);
            false
        });
        removed
    }

    pub fn remove_session(&mut self, session: SessionKey) -> Vec<SubscriberId> {
        let mut removed = Vec::new();
        self.subscriptions.retain(|s| {
            if s.session != session {
                return true;
            }
            removed.push(s.who);
            false
        });
        removed
    }

    pub fn demand(&self, session: SessionKey, status: Option<&str>) -> DemandSnapshot {
        let mut lanes = LaneBits::default();
        let mut selectors = Vec::new();
        for s in self.subscriptions.iter().filter(|s| s.session == session) {
            lanes = lanes.union(s.lanes);
            selectors.extend(s.wire_selectors.iter().cloned());
        }
        selectors.sort_unstable();
        selectors.dedup();
        let (regular, precise) = lanes.effective(matches!(status, Some("play" | "pause")));
        DemandSnapshot {
            regular,
            precise,
            selectors,
        }
    }

    /// Demand restricted to `ns` for a non-owning provider of `ns`
    /// (`docs/v2/plugin-abi.md` §6.7): only its own namespace's selectors,
    /// and the lanes those selectors compile to.
    pub fn namespace_demand(
        &self,
        session: SessionKey,
        status: Option<&str>,
        ns: &str,
    ) -> DemandSnapshot {
        let mut lanes = LaneBits::default();
        let mut selectors = Vec::new();
        for s in self.subscriptions.iter().filter(|s| s.session == session) {
            for ((name, pointer), selector_lanes) in s.wire_selectors.iter().zip(&s.selector_lanes)
            {
                if name.as_ref() == ns {
                    lanes = lanes.union(*selector_lanes);
                    selectors.push((name.clone(), pointer.clone()));
                }
            }
        }
        selectors.sort_unstable();
        selectors.dedup();
        let (regular, precise) = lanes.effective(matches!(status, Some("play" | "pause")));
        DemandSnapshot {
            regular,
            precise,
            selectors,
        }
    }

    pub fn baseline(
        &self,
        reg: &SchemaRegistry,
        who: SubscriberId,
        state: &SessionState,
    ) -> Vec<StateEntry> {
        self.subscriptions
            .iter()
            .find(|s| s.who == who && s.session == state.key)
            .map(|s| {
                s.selectors
                    .iter()
                    .map(|(ns, p)| entry(reg, s, state, ns, p))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn changes(
        &self,
        reg: &SchemaRegistry,
        outcome: &CommitOutcome,
    ) -> Vec<(SubscriberId, Vec<StateEntry>)> {
        self.subscriptions
            .iter()
            .filter(|s| s.session == outcome.current.key)
            // API readers project their full snapshots with Audience::Api.
            .filter(|s| !matches!(s.who, SubscriberId::Api(_)))
            .filter_map(|s| {
                let entries: Vec<_> = s
                    .selectors
                    .iter()
                    .filter_map(|(ns, p)| {
                        if !outcome
                            .touched
                            .iter()
                            .any(|(other, q)| ns == other && p.overlaps(q))
                            || same(outcome.previous.get(ns, p), outcome.current.get(ns, p))
                        {
                            return None;
                        }
                        let previous = project_select(
                            reg,
                            &outcome.previous,
                            ns,
                            p,
                            Audience::Plugin(&s.grant),
                        );
                        let current = project_select(
                            reg,
                            &outcome.current,
                            ns,
                            p,
                            Audience::Plugin(&s.grant),
                        );
                        (!same(previous.as_deref(), current.as_deref()))
                            .then(|| projected_entry(ns, p, current.as_deref()))
                    })
                    .collect();
                (!entries.is_empty()).then_some((s.who, entries))
            })
            .collect()
    }
}

/// Change detection preserves IEEE bits without changing StateValue's PartialEq.
/// Recurse through rebuilt containers, retaining the shared-tree fast path.
fn same(a: Option<&StateValue>, b: Option<&StateValue>) -> bool {
    match (a, b) {
        (Some(StateValue::F64(a)), Some(StateValue::F64(b))) => a.to_bits() == b.to_bits(),
        (Some(StateValue::Object(a)), Some(StateValue::Object(b))) => {
            Arc::ptr_eq(a, b)
                || (a.len() == b.len()
                    && a.iter()
                        .zip(b.iter())
                        .all(|((ka, a), (kb, b))| ka == kb && same(Some(a), Some(b))))
        }
        (Some(StateValue::Array(a)), Some(StateValue::Array(b))) => {
            Arc::ptr_eq(a, b)
                || (a.len() == b.len()
                    && a.iter().zip(b.iter()).all(|(a, b)| same(Some(a), Some(b))))
        }
        _ => a == b,
    }
}

fn entry(
    reg: &SchemaRegistry,
    s: &Subscription,
    state: &SessionState,
    ns: &str,
    p: &Pointer,
) -> StateEntry {
    let value = project_select(reg, state, ns, p, Audience::Plugin(&s.grant));
    projected_entry(ns, p, value.as_deref())
}

fn projected_entry(ns: &str, p: &Pointer, value: Option<&StateValue>) -> StateEntry {
    StateEntry {
        ns: ns.into(),
        pointer: p.to_string(),
        value: value.map(StateValue::to_cbor_value),
        error: None,
    }
}
