//! D4 validation is completed against the old snapshot before path copying.
use super::{
    Permit, Pointer, PointerError, PublisherGrant, SessionState, StateObject, StateValue,
    ValueError,
};
use std::sync::Arc;
use vosu_plugin_api::vosu_status::{
    VOSU_ERR_INVALID_ARGUMENT, VOSU_ERR_PERMISSION_DENIED, VOSU_ERR_TARGET_GONE,
};
use vosu_plugin_api::{PublishFieldRef, PublishValueRef, vosu_status};

#[derive(Debug, thiserror::Error)]
pub enum CommitError {
    #[error("overlapping pointers {a} and {b} in {ns}")]
    Overlap { ns: String, a: String, b: String },
    #[error("uncovered field {ns}{pointer}")]
    Uncovered { ns: String, pointer: String },
    #[error("vosu.client is core-owned")]
    ClientNamespace,
    #[error("reserved namespace")]
    ReservedNamespace,
    #[error("foreign namespace {0}")]
    ForeignNamespace(String),
    #[error("pointer descends through an array or scalar: {0}")]
    ArrayDescent(String),
    #[error(transparent)]
    Pointer(#[from] PointerError),
    #[error(transparent)]
    Value(#[from] ValueError),
    #[error("references are unavailable before M3")]
    ResourceRefBeforeM3,
    #[error("session has not identified")]
    NotIdentified,
    #[error("unknown session or mismatched entry handle")]
    UnknownSession,
    #[error("session detached")]
    Detached,
    #[error("session belongs to another plugin")]
    NotOwner,
}

impl From<CommitError> for vosu_status {
    fn from(error: CommitError) -> Self {
        match error {
            CommitError::NotOwner => VOSU_ERR_PERMISSION_DENIED,
            CommitError::Detached => VOSU_ERR_TARGET_GONE,
            _ => VOSU_ERR_INVALID_ARGUMENT,
        }
    }
}

pub struct CommitOutcome {
    pub previous: Arc<SessionState>,
    pub current: Arc<SessionState>,
    pub touched: Vec<(Arc<str>, Pointer)>,
}

pub(super) struct Validated {
    pub ns: Arc<str>,
    pub pointer: Pointer,
    pub value: StateValue,
}

fn permitted(grant: &PublisherGrant, ns: &str, p: &Pointer) -> Result<(), CommitError> {
    match grant.permits(ns, p) {
        Permit::Own | Permit::Covered => Ok(()),
        Permit::Uncovered => Err(CommitError::Uncovered {
            ns: ns.into(),
            pointer: p.to_string(),
        }),
        Permit::ClientNamespace => Err(CommitError::ClientNamespace),
        Permit::Reserved => Err(CommitError::ReservedNamespace),
        Permit::Foreign => Err(CommitError::ForeignNamespace(ns.into())),
    }
}

pub(super) fn validate<'a>(
    grant: &PublisherGrant,
    old: &SessionState,
    entries: impl ExactSizeIterator<Item = PublishFieldRef<'a>>,
) -> Result<Vec<Validated>, CommitError> {
    let mut validated: Vec<Validated> = Vec::with_capacity(entries.len());
    for entry in entries {
        if entry.handle != old.key.0 {
            return Err(CommitError::UnknownSession);
        }
        let pointer = Pointer::parse(entry.pointer)?;
        permitted(grant, entry.ns, &pointer)?;
        for prior in &validated {
            if prior.ns.as_ref() == entry.ns && prior.pointer.overlaps(&pointer) {
                return Err(CommitError::Overlap {
                    ns: entry.ns.into(),
                    a: prior.pointer.to_string(),
                    b: entry.pointer.into(),
                });
            }
        }
        let mut value = match entry.value {
            PublishValueRef::Null => StateValue::Null,
            PublishValueRef::Bool(v) => StateValue::Bool(v),
            PublishValueRef::I64(v) => StateValue::I64(v),
            PublishValueRef::U64(v) => StateValue::U64(v),
            PublishValueRef::F64(v) => StateValue::F64(v),
            PublishValueRef::String(v) => StateValue::String(Arc::from(v)),
            PublishValueRef::Bytes(v) => StateValue::Bytes(Arc::from(v)),
            PublishValueRef::Cbor(v) => StateValue::from_cbor(v)?,
            PublishValueRef::ResourceRef(_) | PublishValueRef::TreeRef(_) => {
                return Err(CommitError::ResourceRefBeforeM3);
            }
        };
        // Ancestor CBOR writes must not smuggle protected reference fields past covers.
        if pointer.is_root() {
            for field in match entry.ns {
                "vosu.chart" => &["resources"][..],
                "vosu.skin" => &["tree"][..],
                _ => &[],
            } {
                if let Some(child) = value.get(&[*field]) {
                    validate_protected(grant, entry.ns, field, child)?;
                }
            }
        } else if entry.ns == "vosu.chart"
            && pointer.tokens().len() == 1
            && pointer.tokens()[0].as_ref() == "resources"
        {
            validate_protected(grant, entry.ns, "resources", &value)?;
            if matches!(value, StateValue::Null) {
                value = empty_object();
            }
        }
        // abi:212-214: missing intermediates become objects. Null skeleton
        // fields likewise mean absent, so nested publication can initialize
        // them; existing non-null scalars and arrays must never be traversed.
        let mut at = old.namespace(entry.ns);
        for token in pointer.tokens() {
            at = match at {
                None | Some(StateValue::Null) => None,
                Some(StateValue::Object(object)) => object.get(token),
                Some(_) => return Err(CommitError::ArrayDescent(entry.pointer.into())),
            };
        }
        validated.push(Validated {
            ns: Arc::from(entry.ns),
            pointer,
            value,
        });
    }
    Ok(validated)
}

fn validate_protected(
    grant: &PublisherGrant,
    ns: &str,
    field: &str,
    value: &StateValue,
) -> Result<(), CommitError> {
    permitted(grant, ns, &Pointer::parse(&format!("/{field}"))?)?;
    if ns == "vosu.chart"
        && let StateValue::Object(object) = value
    {
        for (kind, _) in object.iter() {
            // A resource map member is a single decoded pointer token.
            let escaped = kind.replace('~', "~0").replace('/', "~1");
            permitted(
                grant,
                ns,
                &Pointer::parse(&format!("/resources/{escaped}"))?,
            )?;
        }
    }
    Ok(())
}

pub(super) fn empty_object() -> StateValue {
    StateValue::Object(Arc::new(StateObject::default()))
}

pub(super) fn write_path(
    at: &mut StateValue,
    tokens: &[Box<str>],
    value: StateValue,
    remove: bool,
) {
    let Some((head, tail)) = tokens.split_first() else {
        *at = value;
        return;
    };
    if matches!(at, StateValue::Null) {
        *at = empty_object();
    }
    let StateValue::Object(object) = at else {
        unreachable!("validated object descent");
    };
    let entries = Arc::make_mut(object).entries_mut();
    let index = entries
        .iter()
        .position(|(k, _)| k.as_ref() == head.as_ref());
    if tail.is_empty() && remove {
        if let Some(index) = index {
            entries.remove(index);
        }
        return;
    }
    let index = index.unwrap_or_else(|| {
        entries.push((Arc::from(head.as_ref()), StateValue::Null));
        entries.len() - 1
    });
    write_path(&mut entries[index].1, tail, value, remove);
}
