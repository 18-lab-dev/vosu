//! D4 validation is completed against the old snapshot before path copying.
use super::{
    Permit, Pointer, PointerError, PublisherGrant, SessionState, StateObject, StateValue,
    ValueError,
};
use std::collections::HashMap;
use std::fmt::Write as _;
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
    #[error("invalid reference class, kind, or publication location")]
    InvalidReference,
    #[error(transparent)]
    Registry(#[from] crate::resources::RegistryError),
    #[error("session has not identified")]
    NotIdentified,
    #[error("unknown session or mismatched entry handle")]
    UnknownSession,
    #[error("session detached")]
    Detached,
    #[error("session belongs to another plugin")]
    NotOwner,
    #[error("core namespace root must be an object")]
    CoreRoot,
    #[error("state document exceeds its byte or key budget")]
    DocumentBudget,
    #[error("resources must be an object or null")]
    ResourceMap,
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
    pub reference: Option<Arc<crate::resources::ReferenceIdentity>>,
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
    references: &super::ReferenceStore,
) -> Result<Vec<Validated>, CommitError> {
    let mut validated: Vec<Validated> = Vec::with_capacity(entries.len());
    let mut lookup = ReadIndex::default();
    for entry in entries {
        if entry.handle != old.key.0 {
            return Err(CommitError::UnknownSession);
        }
        let pointer = Pointer::parse(entry.pointer)?;
        permitted(grant, entry.ns, &pointer)?;
        let mut reference = None;
        let mut value = match entry.value {
            PublishValueRef::Null => StateValue::Null,
            PublishValueRef::Bool(v) => StateValue::Bool(v),
            PublishValueRef::I64(v) => StateValue::I64(v),
            PublishValueRef::U64(v) => StateValue::U64(v),
            PublishValueRef::F64(v) => StateValue::F64(v),
            PublishValueRef::String(v) => StateValue::String(Arc::from(v)),
            PublishValueRef::Bytes(v) => StateValue::Bytes(Arc::from(v)),
            PublishValueRef::Cbor(v) => StateValue::from_cbor(v)?,
            PublishValueRef::ResourceRef(bytes) | PublishValueRef::TreeRef(bytes) => {
                let class = if matches!(entry.value, PublishValueRef::TreeRef(_)) {
                    crate::resources::ReferenceClass::Tree
                } else {
                    crate::resources::ReferenceClass::Resource
                };
                let (value, identity) =
                    references.project(grant, entry.ns, &pointer, class, bytes)?;
                reference = Some(identity);
                value
            }
        };
        if pointer.is_root() && super::CORE_NAMESPACES.contains(&entry.ns) {
            normalize_core_root(grant, entry.ns, &mut value)?;
        } else if entry.ns == "vosu.chart"
            && pointer.tokens().len() == 1
            && pointer.tokens()[0].as_ref() == "resources"
        {
            validate_protected(grant, entry.ns, "resources", &value)?;
            if matches!(value, StateValue::Null | StateValue::Object(_)) {
                value = empty_object();
            }
        }
        if reference.is_none() {
            let tokens = pointer.tokens();
            let protected_leaf = (entry.ns == "vosu.chart"
                && tokens.first().is_some_and(|t| t.as_ref() == "resources")
                && tokens.len() > 1)
                || (entry.ns == "vosu.skin"
                    && tokens.first().is_some_and(|t| t.as_ref() == "tree"));
            if protected_leaf
                && (!matches!(value, StateValue::Null)
                    || (entry.ns == "vosu.chart" && tokens.len() != 2)
                    || (entry.ns == "vosu.skin" && tokens.len() != 1))
            {
                return Err(CommitError::InvalidReference);
            }
        }
        // abi:212-214: missing intermediates become objects. Null skeleton
        // fields likewise mean absent, so nested publication can initialize
        // them; existing non-null scalars and arrays must never be traversed.
        let mut at = old.namespace(entry.ns);
        for token in pointer.tokens() {
            at = match at {
                None | Some(StateValue::Null) => None,
                Some(StateValue::Object(object)) => lookup.get(object, token),
                Some(_) => return Err(CommitError::ArrayDescent(entry.pointer.into())),
            };
        }
        validated.push(Validated {
            ns: Arc::from(entry.ns),
            pointer,
            value,
            reference,
        });
    }
    let mut sorted: Vec<_> = validated.iter().collect();
    sorted.sort_unstable_by(|a, b| {
        a.ns.cmp(&b.ns)
            .then_with(|| a.pointer.tokens().cmp(b.pointer.tokens()))
    });
    for pair in sorted.windows(2) {
        let [a, b] = pair else {
            unreachable!("two-entry window")
        };
        if a.ns == b.ns && a.pointer.overlaps(&b.pointer) {
            return Err(CommitError::Overlap {
                ns: a.ns.to_string(),
                a: a.pointer.to_string(),
                b: b.pointer.to_string(),
            });
        }
    }
    Ok(validated)
}

fn validate_protected(
    grant: &PublisherGrant,
    ns: &str,
    field: &str,
    value: &StateValue,
) -> Result<(), CommitError> {
    // Empty reference skeletons need no kind grant. Check every member, not
    // just get()'s first match, even if a caller constructs duplicate keys.
    if matches!(value, StateValue::Null)
        || (ns == "vosu.chart" && matches!(value, StateValue::Object(o) if o.is_empty()))
    {
        return Ok(());
    }
    if ns == "vosu.chart" && !matches!(value, StateValue::Object(_)) {
        return Err(CommitError::ResourceMap);
    }
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
        if object
            .iter()
            .any(|(_, child)| !matches!(child, StateValue::Null))
        {
            return Err(CommitError::InvalidReference);
        }
    }
    if ns == "vosu.skin" {
        return Err(CommitError::InvalidReference);
    }
    Ok(())
}

pub(super) fn normalize_core_root(
    grant: &PublisherGrant,
    ns: &str,
    value: &mut StateValue,
) -> Result<(), CommitError> {
    let StateValue::Object(object) = value else {
        return Err(CommitError::CoreRoot);
    };
    let field = match ns {
        "vosu.chart" => "resources",
        "vosu.skin" => "tree",
        _ => return Ok(()),
    };
    for (key, child) in object.iter().filter(|(key, _)| *key == field) {
        validate_protected(grant, ns, key, child)?;
    }
    let entries = Arc::make_mut(object).entries_mut();
    let mut found = false;
    for (_, child) in entries.iter_mut().filter(|(key, _)| key.as_ref() == field) {
        found = true;
        if ns == "vosu.chart" {
            *child = empty_object();
        }
    }
    if !found {
        entries.push((
            Arc::from(field),
            if ns == "vosu.chart" {
                empty_object()
            } else {
                StateValue::Null
            },
        ));
    }
    Ok(())
}

/// Lazily index wide old objects once. Borrowed keys/values stay alive in the
/// immutable previous snapshot throughout validation.
#[derive(Default)]
struct ReadIndex<'a>(HashMap<usize, HashMap<&'a str, &'a StateValue>>);
impl<'a> ReadIndex<'a> {
    fn get(&mut self, object: &'a Arc<StateObject>, key: &str) -> Option<&'a StateValue> {
        if object.len() < 32 {
            return object.get(key);
        }
        self.0
            .entry(Arc::as_ptr(object) as usize)
            .or_insert_with(|| object.iter().collect())
            .get(key)
            .copied()
    }
}

pub(super) fn empty_object() -> StateValue {
    StateValue::Object(Arc::new(StateObject::default()))
}

/// JSON-projection bytes per namespace and object keys (including nested
/// objects/arrays). Accounting does not allocate encoded document buffers.
#[derive(Clone, Copy, Default, Debug)]
pub(super) struct Usage {
    pub bytes: i64,
    pub keys: i64,
}
impl Usage {
    fn add(&mut self, other: Self) {
        self.bytes += other.bytes;
        self.keys += other.keys;
    }
    fn difference(self, old: Self) -> Self {
        Self {
            bytes: self.bytes - old.bytes,
            keys: self.keys - old.keys,
        }
    }
    pub fn measure(value: &StateValue) -> Self {
        let mut usage = Self::default();
        match value {
            StateValue::Null => usage.bytes = 4,
            StateValue::Bool(v) => usage.bytes = if *v { 4 } else { 5 },
            StateValue::I64(v) => {
                let _ = write!(usage, "{v}");
            }
            StateValue::U64(v) => {
                let _ = write!(usage, "{v}");
            }
            StateValue::F64(v) if v.is_finite() => {
                let _ = write!(usage, "{v}");
            }
            StateValue::F64(_) => usage.bytes = 4,
            StateValue::String(s) => usage.bytes = string_bytes(s),
            StateValue::Bytes(b) => usage.bytes = (2 + b.len().div_ceil(3) * 4) as i64,
            StateValue::Array(items) => {
                usage.bytes = 2 + items.len().saturating_sub(1) as i64;
                for value in items.iter() {
                    usage.add(Self::measure(value));
                }
            }
            StateValue::Object(object) => {
                usage.bytes = 2 + object.len().saturating_sub(1) as i64;
                usage.keys = object.len() as i64;
                for (key, value) in object.iter() {
                    usage.bytes += string_bytes(key) + 1;
                    usage.add(Self::measure(value));
                }
            }
        }
        usage
    }
    pub fn apply(&mut self, delta: Self) {
        self.add(delta);
    }
}
impl std::fmt::Write for Usage {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.bytes += s.len() as i64;
        Ok(())
    }
}
fn string_bytes(s: &str) -> i64 {
    2 + s
        .bytes()
        .map(|b| match b {
            b'"' | b'\\' | b'\n' | b'\r' | b'\t' => 2,
            0..=31 => 6,
            _ => 1,
        })
        .sum::<i64>()
}

/// Read-only virtual path accounting: shared missing/null ancestors are
/// charged once, and sibling insertions/removals track their comma changes.
/// No namespace tree is copied or mutated before this check succeeds.
pub(super) fn budget(
    old: &SessionState,
    entries: &[Validated],
    previous: &HashMap<Arc<str>, Usage>,
    max_bytes: i64,
    max_keys: i64,
) -> Result<HashMap<Arc<str>, Usage>, CommitError> {
    struct Node<'a> {
        old: Option<&'a StateValue>,
        children: HashMap<&'a str, usize>,
        fields: usize,
        initialized: bool,
    }
    impl<'a> Node<'a> {
        fn new(old: Option<&'a StateValue>) -> Self {
            Self {
                old,
                children: HashMap::new(),
                fields: match old {
                    Some(StateValue::Object(o)) => o.len(),
                    _ => 0,
                },
                initialized: false,
            }
        }
    }
    let mut usage = previous.clone();
    let mut roots = HashMap::new();
    let mut nodes = Vec::new();
    let mut lookup = ReadIndex::default();
    for entry in entries {
        let total = usage
            .entry(entry.ns.clone())
            .or_insert(Usage { bytes: 4, keys: 0 });
        let mut index = *roots.entry(entry.ns.as_ref()).or_insert_with(|| {
            nodes.push(Node::new(old.namespace(&entry.ns)));
            nodes.len() - 1
        });
        let tokens = entry.pointer.tokens();
        for (depth, token) in tokens.iter().enumerate() {
            if !nodes[index].initialized {
                if matches!(nodes[index].old, None | Some(StateValue::Null)) {
                    total.bytes -= 2; // null/placeholder -> empty object
                }
                nodes[index].initialized = true;
            }
            let old_child = match nodes[index].old {
                Some(StateValue::Object(object)) => lookup.get(object, token),
                _ => None,
            };
            if depth + 1 == tokens.len() && removes_member(entry) {
                if let Some(value) = old_child {
                    total.bytes -= string_bytes(token) + 1 + i64::from(nodes[index].fields > 1);
                    total.keys -= 1;
                    total.apply(Usage::default().difference(Usage::measure(value)));
                    nodes[index].fields -= 1;
                }
                break;
            }
            let child = if let Some(child) = nodes[index].children.get(token.as_ref()) {
                *child
            } else {
                if old_child.is_none() {
                    total.bytes += string_bytes(token) + 1 + 4 + i64::from(nodes[index].fields > 0);
                    total.keys += 1;
                    nodes[index].fields += 1;
                }
                let child = nodes.len();
                nodes.push(Node::new(old_child));
                nodes[index].children.insert(token, child);
                child
            };
            index = child;
            if depth + 1 == tokens.len() {
                total.apply(
                    Usage::measure(&entry.value)
                        .difference(Usage::measure(old_child.unwrap_or(&StateValue::Null))),
                );
            }
        }
        if tokens.is_empty() {
            total.apply(Usage::measure(&entry.value).difference(Usage::measure(
                nodes[index].old.unwrap_or(&StateValue::Null),
            )));
        }
    }
    if usage.values().any(|u| u.bytes > max_bytes)
        || usage.values().map(|u| u.keys).sum::<i64>() > max_keys
    {
        return Err(CommitError::DocumentBudget);
    }
    Ok(usage)
}

pub(super) fn removes_member(entry: &Validated) -> bool {
    entry.ns.as_ref() == "vosu.chart"
        && entry.pointer.tokens().len() == 2
        && entry.pointer.tokens()[0].as_ref() == "resources"
        && matches!(entry.value, StateValue::Null)
}

/// Indices belong to one commit. Only ancestors are indexed; non-overlap
/// validation guarantees none is replaced while a later write still uses it.
#[derive(Default)]
pub(super) struct WriteIndex(HashMap<usize, HashMap<Arc<str>, usize>>);

pub(super) fn write_path(
    at: &mut StateValue,
    tokens: &[Box<str>],
    value: StateValue,
    remove: bool,
    lookup: &mut WriteIndex,
) {
    let Some((head, tail)) = tokens.split_first() else {
        *at = value;
        return;
    };
    if matches!(at, StateValue::Null) {
        *at = empty_object();
    }
    let StateValue::Object(object) = at else {
        unreachable!("validated object descent")
    };
    let object = Arc::make_mut(object);
    let id = std::ptr::from_ref(object) as usize;
    let entries = object.entries_mut();
    let mut index = if entries.len() < 32 {
        entries
            .iter()
            .position(|(k, _)| k.as_ref() == head.as_ref())
    } else {
        lookup
            .0
            .entry(id)
            .or_insert_with(|| {
                entries
                    .iter()
                    .enumerate()
                    .map(|(i, (k, _))| (k.clone(), i))
                    .collect()
            })
            .get(head.as_ref())
            .copied()
    };
    if tail.is_empty() && remove {
        if let Some(index) = index {
            entries.remove(index);
            lookup.0.remove(&id); // removal shifts later indices
        }
        return;
    }
    if index.is_none() {
        let key: Arc<str> = Arc::from(head.as_ref());
        index = Some(entries.len());
        if let Some(keys) = lookup.0.get_mut(&id) {
            keys.insert(key.clone(), entries.len());
        }
        entries.push((key, StateValue::Null));
    }
    write_path(
        &mut entries[index.expect("existing or inserted")].1,
        tail,
        value,
        remove,
        lookup,
    );
}
