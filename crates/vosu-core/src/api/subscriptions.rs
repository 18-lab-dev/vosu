//! Immutable merged-document plans and bounded, latest-only registrations.
use std::sync::Arc;

use tokio::sync::watch;

use super::{ApiUpdate, SessionTarget};
use crate::state::{LaneBits, MAX_SELECTORS, Pointer, SchemaRegistry, SessionKey};

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ApiError {
    #[error("unknown session")]
    UnknownSession,
    #[error("plugin host is shut down")]
    Closed,
    #[error("subscription capacity reached")]
    Capacity,
    #[error("invalid selection: {0}")]
    InvalidSelection(String),
}

/// A validated 1–256 non-overlapping RFC 6901 merged-document selection.
#[derive(Clone, Debug)]
pub struct ApiPlan {
    pub(crate) pointers: Vec<Pointer>,
    pub(crate) selectors: Vec<Option<(Arc<str>, Pointer)>>,
}
impl ApiPlan {
    pub fn new(pointers: &[impl AsRef<str>]) -> Result<Self, ApiError> {
        if pointers.is_empty() || pointers.len() > MAX_SELECTORS {
            return Err(ApiError::InvalidSelection("expected 1–256 pointers".into()));
        }
        let mut parsed = Vec::<Pointer>::with_capacity(pointers.len());
        for text in pointers {
            let pointer = Pointer::parse(text.as_ref())
                .map_err(|e| ApiError::InvalidSelection(e.to_string()))?;
            if parsed.iter().any(|p| p.overlaps(&pointer)) {
                return Err(ApiError::InvalidSelection("overlapping pointers".into()));
            }
            parsed.push(pointer);
        }
        let selectors = parsed
            .iter()
            .map(|p| {
                p.tokens()
                    .split_first()
                    .map(|(ns, rest)| (Arc::from(ns.as_ref()), below_namespace(rest)))
            })
            .collect();
        Ok(Self {
            pointers: parsed,
            selectors,
        })
    }

    pub fn pointers(&self) -> &[Pointer] {
        &self.pointers
    }

    /// Translate public merged pointers for the existing namespace demand
    /// compiler. Expansion is internal: the external selector bound stays 256.
    pub(crate) fn demand(&self, registry: &SchemaRegistry) -> Vec<(Arc<str>, Pointer)> {
        if self.pointers[0].is_root() {
            return registry
                .namespaces()
                .map(|ns| (Arc::from(ns), Pointer::root()))
                .collect();
        }
        self.public_selectors(registry).cloned().collect()
    }

    /// Compare borrowed compiled selectors on the stable publication path.
    pub(crate) fn demand_matches(
        &self,
        registry: &SchemaRegistry,
        demand: &[(Arc<str>, Pointer)],
    ) -> bool {
        if self.pointers[0].is_root() {
            return demand
                .iter()
                .map(|(ns, _)| ns.as_ref())
                .eq(registry.namespaces());
        }
        self.public_selectors(registry).eq(demand.iter())
    }

    fn public_selectors<'a>(
        &'a self,
        registry: &'a SchemaRegistry,
    ) -> impl Iterator<Item = &'a (Arc<str>, Pointer)> {
        self.selectors.iter().filter_map(|selector| {
            let selector = selector.as_ref()?;
            let (ns, pointer) = selector;
            // Syntactically valid unknown fields are permitted, even when the
            // namespace token cannot name an ABI namespace. They remain absent.
            if ns.is_empty()
                || ns.len() > crate::state::MAX_NAMESPACE_BYTES
                || ns.as_ref() == crate::state::SESSIONS_NAMESPACE
                || !ns
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
            {
                return None;
            }
            if registry
                .hidden_subtrees(ns)
                .iter()
                .any(|h| h.is_prefix_of(pointer))
            {
                return None;
            }
            Some(selector)
        })
    }

    /// Schema-derived producing lanes, for the independently scheduled API hub.
    pub fn lanes(&self, registry: &SchemaRegistry) -> LaneBits {
        if self.pointers[0].is_root() {
            return registry.namespaces().fold(LaneBits::default(), |bits, ns| {
                bits.union(registry.lanes(ns, &[]))
            });
        }
        self.selectors
            .iter()
            .flatten()
            .fold(LaneBits::default(), |bits, (ns, p)| {
                if registry
                    .hidden_subtrees(ns)
                    .iter()
                    .any(|h| h.is_prefix_of(p))
                {
                    return bits;
                }
                let mut tokens = [""; 32];
                for (slot, token) in tokens.iter_mut().zip(p.tokens()) {
                    *slot = token;
                }
                bits.union(registry.lanes(ns, &tokens[..p.tokens().len()]))
            })
    }
}

pub(crate) fn below_namespace(tokens: &[Box<str>]) -> Pointer {
    let mut text = String::new();
    for token in tokens {
        text.push('/');
        text.push_str(&token.replace('~', "~0").replace('/', "~1"));
    }
    Pointer::parse(&text).expect("suffix of a validated pointer is valid")
}

pub struct ApiSubscription {
    pub updates: watch::Receiver<Arc<ApiUpdate>>,
    _guard: Box<dyn Send + Sync>,
}
impl ApiSubscription {
    pub(crate) fn new(
        updates: watch::Receiver<Arc<ApiUpdate>>,
        guard: impl Send + Sync + 'static,
    ) -> Self {
        Self {
            updates,
            _guard: Box::new(guard),
        }
    }

    /// The full latest selected snapshot; no selection values are consumed.
    pub fn current(&self) -> Arc<ApiUpdate> {
        Arc::clone(&self.updates.borrow())
    }

    /// Await the next latest-value notification. Callers own their wait bound.
    pub async fn changed(&mut self) -> Result<Arc<ApiUpdate>, ApiError> {
        self.updates.changed().await.map_err(|_| ApiError::Closed)?;
        Ok(self.current())
    }
}

#[derive(Default)]
pub(crate) struct ApiReaders {
    pub next: u64,
    pub revision: u64,
    pub readers: Vec<ApiReader>,
}
pub(crate) struct ApiReader {
    pub id: u64,
    /// None selects the coherent lifecycle document rather than session state.
    pub target: Option<SessionTarget>,
    pub key: Option<SessionKey>,
    pub plan: ApiPlan,
    pub demand: Vec<(Arc<str>, Pointer)>,
    pub updates: watch::Sender<Arc<ApiUpdate>>,
}
