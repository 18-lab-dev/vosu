use std::borrow::Cow;
use std::cell::{Cell, RefCell};

use super::*;

pub(super) struct SnapshotPathResolver<'a> {
    pub(super) executable: PathBuf,
    pub(super) launch: PathBuf,
    pub(super) environment: &'a EnvironmentSnapshot,
}

impl PathResolver for SnapshotPathResolver<'_> {
    fn current_exe(&self) -> io::Result<PathBuf> {
        Ok(self.executable.clone())
    }

    fn launch_dir(&self) -> io::Result<PathBuf> {
        Ok(self.launch.clone())
    }

    fn env_var(&self, name: &str) -> Option<OsString> {
        self.environment.get(name).map(OsString::from)
    }
}

/// Strongly typed bridge from immutable internal snapshots to public wire
/// documents. The four schema modules provide the production implementation.
pub trait SnapshotEncoder: Send + Sync + 'static {
    fn encode(
        &self,
        snapshot: &CombinedSnapshot,
        schema: ApiSchema,
        filters: &[Filter],
    ) -> Result<Bytes, ApiError>;
}

/// Production encoder dispatching to the four frozen wire builders.
#[derive(Default)]
pub struct ProductionSnapshotEncoder;

impl ProductionSnapshotEncoder {
    /// Builds only the subtrees selected by decoded RFC 6901 pointers. Values at
    /// those pointers match the complete V2 document, including JS number rules.
    pub fn subscription_document(
        snapshot: &CombinedSnapshot,
        pointers: &[&[String]],
    ) -> Result<serde_json::Value, ApiError> {
        if pointers.iter().any(|pointer| pointer.is_empty()) {
            return serde_json::from_slice(&crate::api::v2::encode(snapshot)?)
                .map_err(|error| ApiError::serialization(error.to_string()));
        }
        let mut selection = Selected::default();
        for pointer in pointers {
            selection.add_pointer(pointer);
        }
        let response = crate::api::v2::subscription_response(snapshot)?;
        let selected = serialize_selected(&response, &selection)?;
        subscription_json(selected)
    }
}

/// Preserve serde_json's insertion order, rather than the legacy filter API's
/// JavaScript property ordering. Normalize only numbers the selection can observe.
fn subscription_json(value: ProjectionValue) -> Result<serde_json::Value, ApiError> {
    use serde_json::Value;
    Ok(match value {
        ProjectionValue::Null | ProjectionValue::NonFiniteNumber => Value::Null,
        ProjectionValue::Bool(value) => Value::Bool(value),
        ProjectionValue::Number(mut number) => {
            if let Some(value) = number.as_f64().filter(|_| number.is_f64()) {
                number = crate::calculation::js_json_number(value).parse().map_err(
                    |error: serde_json::Error| ApiError::serialization(error.to_string()),
                )?;
            }
            Value::Number(number)
        }
        ProjectionValue::String(value) => Value::String(value),
        ProjectionValue::Array(values) => Value::Array(
            values
                .into_iter()
                .map(subscription_json)
                .collect::<Result<_, _>>()?,
        ),
        // Only descendants were selected; the array itself is unobservable.
        // Numeric-key objects resolve the same pointers without allocating null
        // placeholders for an entire history to expose a few indexed samples.
        ProjectionValue::SparseArray { elements, .. } => Value::Object(
            elements
                .into_iter()
                .map(|(index, value)| Ok((index.to_string(), subscription_json(value)?)))
                .collect::<Result<_, ApiError>>()?,
        ),
        ProjectionValue::Object(entries) => Value::Object(
            entries
                .into_iter()
                .map(|(key, value)| Ok((key.into_owned(), subscription_json(value)?)))
                .collect::<Result<_, ApiError>>()?,
        ),
        ProjectionValue::Utf16CodeUnit(_) => {
            unreachable!("UTF-16 property projections are not produced by serialize_selected")
        }
    })
}

impl SnapshotEncoder for ProductionSnapshotEncoder {
    fn encode(
        &self,
        snapshot: &CombinedSnapshot,
        schema: ApiSchema,
        filters: &[Filter],
    ) -> Result<Bytes, ApiError> {
        if filters.is_empty() {
            return match schema {
                ApiSchema::V1 => crate::api::v1::encode(snapshot),
                ApiSchema::V2 => crate::api::v2::encode(snapshot),
                ApiSchema::StreamCompanion => crate::api::sc::encode(snapshot),
                ApiSchema::V2Precise => crate::api::precise::encode(snapshot),
            };
        }
        match schema {
            ApiSchema::V1 => encode_selection(snapshot, filters, crate::api::v1::build),
            ApiSchema::V2 => encode_selection(snapshot, filters, crate::api::v2::build),
            ApiSchema::StreamCompanion => {
                encode_selection(snapshot, filters, crate::api::sc::build)
            }
            ApiSchema::V2Precise => encode_selection(snapshot, filters, crate::api::precise::build),
        }
    }
}

/// A wire document built from one publication, kept for the rest of a
/// broadcast pass. It holds that publication's manager and focused-state
/// `Arc`s, so pointer equality with them identifies the same immutable
/// inputs; an address cannot be reused while the document is alive.
struct PassDocument<D> {
    manager: Arc<crate::api::ManagerSnapshot>,
    focused: Option<Arc<crate::state::StateBundleSnapshot>>,
    document: SharedDocument<D>,
}

impl<D> PassDocument<D> {
    fn built_from(&self, snapshot: &CombinedSnapshot) -> bool {
        Arc::ptr_eq(&self.manager, snapshot.manager())
            && match (&self.focused, snapshot.focused()) {
                (Some(kept), Some(focused)) => Arc::ptr_eq(kept, focused),
                (None, None) => true,
                _ => false,
            }
    }
}

/// A wire document and what the plans projected from it so far share.
pub(super) struct SharedDocument<D> {
    document: D,
    shared: SharedProperties,
    /// Every frame is written here, then copied out at its exact length.
    scratch: Vec<u8>,
}

impl<D: Serialize> SharedDocument<D> {
    pub(super) fn new(document: D) -> Self {
        Self {
            document,
            shared: SharedProperties::default(),
            scratch: Vec::new(),
        }
    }

    /// Whether the document property `key` is now projected from its whole
    /// value rather than walked for each plan.
    #[cfg(test)]
    pub(super) fn is_shared(&self, key: &str) -> bool {
        self.shared.whole(key).is_some()
    }

    /// The JSON the projection of the document through `filters` encodes to.
    pub(super) fn encode(&mut self, filters: &[Filter]) -> Result<Bytes, ApiError> {
        // Whether some selected property still needs a walk of its own.
        let mut walk = false;
        for (position, filter) in filters.iter().enumerate() {
            let key = filter_field(filter);
            if filters[..position]
                .iter()
                .any(|earlier| filter_field(earlier) == key)
            {
                continue;
            }
            if let Some((key, budget)) = self.shared.budget(key) {
                self.share(key, budget)?;
            }
            walk |= self.shared.whole(key).is_none();
        }
        let partial = if !walk {
            ProjectionValue::Object(Vec::new())
        } else {
            let mut selected = Selected::from_filters(filters);
            selected
                .keys
                .retain(|entry| self.shared.whole(entry.key).is_none());
            let account = Account::new(usize::MAX);
            let partial = serialize_accounted(&self.document, &selected, &account)?;
            self.shared.spend(account.into_properties());
            partial
        };
        let shared = &self.shared;
        encode_root(&mut self.scratch, filters, &|key| match shared.whole(key) {
            Some(whole) => whole.as_ref().map(Cow::Borrowed),
            None => javascript_property_ref(&partial, key),
        })
    }

    /// Serializes the document property `key` whole, unless that charges more
    /// than `budget`.
    fn share(&mut self, key: &'static str, budget: usize) -> Result<(), ApiError> {
        let whole = Selected {
            keys: vec![SelectedKey {
                key,
                index: javascript_array_index(key),
                selection: Selection::Full,
            }],
        };
        let account = Account::new(budget);
        let result = serialize_accounted(&self.document, &whole, &account);
        // Checked first, so a value is refused even if some serializer
        // swallowed the error of the charge that exceeded the budget.
        if account.exceeded() {
            self.shared.refuse(key, budget);
            return Ok(());
        }
        let value = match result {
            Ok(value) => value,
            Err(_) => {
                // A full property may fail even when a selected subtree succeeds.
                // Never attempt to share it again during this broadcast pass.
                self.shared.refuse(key, usize::MAX);
                return Ok(());
            }
        };
        let value = match value {
            // Only `key` was selected, so it is the sole entry.
            ProjectionValue::Object(mut entries) => entries.pop().map(|(_, value)| value),
            document => javascript_property_ref(&document, key).map(Cow::into_owned),
        };
        self.shared.store(key, value);
        Ok(())
    }
}

/// The document's own properties that the plans of one pass select. Each
/// distinct plan serializes only what it selects, so a property that many
/// plans reach is walked again for each. Serializing it whole once lets every
/// later plan project from that instead, which pays once the walks would
/// have cost more.
///
/// Neither the property's whole size nor the cost of a walk is known in
/// advance, so both are measured as `Account` charges. After a property's
/// first walk, it is serialized whole with a budget of twice the largest walk
/// so far; later attempts are budgeted at `1 / SHARE_RATIO` of all its walk
/// charges. An attempt stops at its budget and the next waits until the
/// budget has doubled, so the work and memory sharing costs stay a fixed
/// fraction of what the walks themselves charged, plus two walks.
#[derive(Default)]
struct SharedProperties {
    properties: Vec<SharedProperty>,
}

const SHARE_RATIO: usize = 16;

struct SharedProperty {
    key: &'static str,
    /// What walks that selected part of the property charged beneath it.
    spent: usize,
    /// The most one walk charged.
    largest_walk: usize,
    /// The budget of the last attempt, which the property exceeded.
    refused: usize,
    /// The property's value in the full document, once serialized whole.
    whole: Option<Option<ProjectionValue>>,
}

impl SharedProperties {
    fn property(&self, key: &str) -> Option<&SharedProperty> {
        self.properties.iter().find(|property| property.key == key)
    }

    fn property_mut(&mut self, key: &str) -> Option<&mut SharedProperty> {
        self.properties
            .iter_mut()
            .find(|property| property.key == key)
    }

    /// The property and the budget to serialize it whole with, if that is due.
    fn budget(&self, key: &str) -> Option<(&'static str, usize)> {
        let property = self.property(key)?;
        let budget = property
            .largest_walk
            .saturating_mul(2)
            .max(property.spent / SHARE_RATIO);
        (property.whole.is_none()
            && property.refused != usize::MAX
            && budget >= property.refused.saturating_mul(2))
        .then_some((property.key, budget))
    }

    fn spend(&mut self, walked: impl Iterator<Item = (&'static str, usize)>) {
        for (key, charged) in walked {
            match self.property_mut(key) {
                Some(property) => {
                    property.spent = property.spent.saturating_add(charged);
                    property.largest_walk = property.largest_walk.max(charged);
                }
                None => self.properties.push(SharedProperty {
                    key,
                    spent: charged,
                    largest_walk: charged,
                    refused: 0,
                    whole: None,
                }),
            }
        }
    }

    fn store(&mut self, key: &str, value: Option<ProjectionValue>) {
        if let Some(property) = self.property_mut(key) {
            property.whole = Some(value);
        }
    }

    fn refuse(&mut self, key: &str, budget: usize) {
        if let Some(property) = self.property_mut(key) {
            property.refused = budget;
        }
    }

    /// The whole value of a shared property, `None` inside where the full
    /// document lacks it.
    fn whole(&self, key: &str) -> Option<&Option<ProjectionValue>> {
        self.property(key)?.whole.as_ref()
    }
}

/// Encodes the projection of the document `build` derives from `snapshot`.
/// Every client of a broadcast pass projects the same snapshot, so within a
/// pass the document is built once and reused (the slot type is per document
/// type, hence per schema). Building depends only on the snapshot's immutable
/// manager and focused state.
fn encode_selection<D: Serialize + 'static>(
    snapshot: &CombinedSnapshot,
    filters: &[Filter],
    build: fn(&CombinedSnapshot) -> Result<D, ApiError>,
) -> Result<Bytes, ApiError> {
    crate::server::websocket::with_pass_local(|slot: &mut Option<PassDocument<D>>| {
        if !slot.as_ref().is_some_and(|kept| kept.built_from(snapshot)) {
            // Released before building; it belongs to another snapshot.
            *slot = None;
            *slot = Some(PassDocument {
                manager: Arc::clone(snapshot.manager()),
                focused: snapshot.focused().cloned(),
                document: SharedDocument::new(build(snapshot)?),
            });
        }
        let kept = slot.as_mut().expect("document was just stored");
        kept.document.encode(filters)
    })
    .unwrap_or_else(|| SharedDocument::new(build(snapshot)?).encode(filters))
}

/// The part of a document that a filter list can observe. A property named by
/// a `Field` filter is serialized in full; one reached only through `Nested`
/// filters keeps just the properties those filters name. Every lookup that
/// `project_filters` performs therefore sees the same value as in the full
/// document, while the unobserved remainder is never serialized.
///
/// Pruning skips unobserved subtrees entirely, so their serialization cannot
/// fail. That is not a behavioral difference here: the wire DTOs serialize
/// infallibly (the unfiltered encoders write the same DTOs without errors).
pub(super) enum Selection<'a> {
    Full,
    Partial(Selected<'a>),
}

#[derive(Default)]
pub(super) struct Selected<'a> {
    keys: Vec<SelectedKey<'a>>,
}

struct SelectedKey<'a> {
    key: &'a str,
    index: Option<u32>,
    selection: Selection<'a>,
}

impl<'a> Selected<'a> {
    pub(super) fn from_filters(filters: &'a [Filter]) -> Self {
        let mut selected = Self::default();
        selected.add(filters);
        selected
    }

    fn add_pointer(&mut self, pointer: &'a [String]) {
        let Some((key, rest)) = pointer.split_first() else {
            return;
        };
        let selection = self.entry(key);
        if rest.is_empty() {
            *selection = Selection::Full;
        } else if let Selection::Partial(selected) = selection {
            selected.add_pointer(rest);
        }
    }

    fn add(&mut self, filters: &'a [Filter]) {
        for filter in filters {
            match filter {
                Filter::Field(field) => *self.entry(field) = Selection::Full,
                Filter::Nested { field, keys } => {
                    if let Selection::Partial(selected) = self.entry(field) {
                        selected.add(keys);
                    }
                }
            }
        }
    }

    fn entry(&mut self, key: &'a str) -> &mut Selection<'a> {
        let position = match self.keys.iter().position(|entry| entry.key == key) {
            Some(position) => position,
            None => {
                self.keys.push(SelectedKey {
                    key,
                    index: javascript_array_index(key),
                    selection: Selection::Partial(Self::default()),
                });
                self.keys.len() - 1
            }
        };
        &mut self.keys[position].selection
    }

    fn property(&self, key: &str) -> Option<&Selection<'a>> {
        self.keys
            .iter()
            .find_map(|entry| (entry.key == key).then_some(&entry.selection))
    }

    fn element(&self, index: usize) -> Option<&Selection<'a>> {
        self.keys.iter().find_map(|entry| {
            entry
                .index
                .is_some_and(|candidate| candidate as usize == index)
                .then_some(&entry.selection)
        })
    }
}

/// `serialize_accounted` without a budget.
pub(super) fn serialize_selected<T: Serialize + ?Sized>(
    value: &T,
    selected: &Selected<'_>,
) -> Result<ProjectionValue, ApiError> {
    serialize_accounted(value, selected, &Account::new(usize::MAX))
}

/// Serializes `value` straight into the projection tree, restricted to
/// `selected`. The result is what `serialize_projection_value` derives from
/// the serde_json and serde_value trees of the same value: serde_json's number,
/// map-key, enum and `RawValue` conventions, with nonfinite floats kept
/// distinct from `null` wherever serde_value's tree is reached by string keys.
///
/// Not reproduced: a single map holding keys of different types that convert
/// to the same JSON string, which no wire DTO can contain.
fn serialize_accounted<T: Serialize + ?Sized>(
    value: &T,
    selected: &Selected<'_>,
    account: &Account,
) -> Result<ProjectionValue, ApiError> {
    value
        .serialize(ProjectionSerializer {
            typed: true,
            selected: Some(selected),
            account,
            root: true,
        })
        .map_err(|error| ApiError::serialization(error.to_string()))
}

/// Document properties beyond this many per serialization go unrecorded.
const RECORDED_PROPERTIES: usize = 8;
/// String and `RawValue` text charged as one entry per this many bytes.
const TEXT_BYTES_PER_ENTRY: usize = 8;

fn text_entries(bytes: usize) -> usize {
    bytes / TEXT_BYTES_PER_ENTRY
}

/// The work of a serialization beneath the document, in object properties
/// and array elements: each container is charged its length once, when it is
/// opened, whether its entries are then kept or skipped. Opening one past
/// `limit` fails the serialization. What is charged beneath each document
/// property is also recorded for that property.
struct Account {
    limit: usize,
    charged: Cell<usize>,
    recorded: Cell<usize>,
    properties: RefCell<[(&'static str, usize); RECORDED_PROPERTIES]>,
}

impl Account {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            charged: Cell::new(0),
            recorded: Cell::new(0),
            properties: RefCell::new([("", 0); RECORDED_PROPERTIES]),
        }
    }

    fn charge(&self, entries: usize) -> Result<(), SerializeError> {
        let charged = self.charged.get().saturating_add(entries);
        self.charged.set(charged);
        if charged > self.limit {
            return Err(budget_exceeded());
        }
        Ok(())
    }

    fn exceeded(&self) -> bool {
        self.charged.get() > self.limit
    }

    fn record(&self, key: &'static str, charged: usize) {
        let recorded = self.recorded.get();
        if charged != 0 && recorded < RECORDED_PROPERTIES {
            self.properties.borrow_mut()[recorded] = (key, charged);
            self.recorded.set(recorded + 1);
        }
    }

    /// What each document property was charged, in the order serialized.
    fn into_properties(self) -> impl Iterator<Item = (&'static str, usize)> {
        let recorded = self.recorded.get();
        self.properties.into_inner().into_iter().take(recorded)
    }
}

#[cold]
fn budget_exceeded() -> SerializeError {
    serialize_error("projection budget exceeded")
}

type SerializeError = serde_json::Error;

fn serialize_error(message: &'static str) -> SerializeError {
    <SerializeError as serde::ser::Error>::custom(message)
}

/// serde_json's private marker for `RawValue`, whose text it parses as JSON.
const RAW_VALUE_TOKEN: &str = "$serde_json::private::RawValue";

#[derive(Clone, Copy)]
struct ProjectionSerializer<'a> {
    /// Whether serde_value still reaches this position through string keys.
    /// Beneath other map keys or inside a `RawValue` only the JSON tree is
    /// consulted, so a nonfinite float becomes plain `null`.
    typed: bool,
    /// `None` serializes the whole value.
    selected: Option<&'a Selected<'a>>,
    account: &'a Account,
    /// Whether this is the document itself rather than a value inside it.
    root: bool,
}

impl<'a> ProjectionSerializer<'a> {
    /// The serializer for a whole value inside this one.
    fn full(self, typed: bool) -> Self {
        Self {
            typed,
            selected: None,
            account: self.account,
            root: false,
        }
    }

    /// Charges a container of `entries` opened beneath the document.
    fn open(self, entries: usize) -> Result<Self, SerializeError> {
        if !self.root {
            self.account.charge(entries)?;
        }
        Ok(self)
    }

    /// The serializer for a value inside this one, restricted to `selection`.
    fn select(self, typed: bool, selection: &'a Selection<'a>) -> Self {
        Self {
            selected: match selection {
                Selection::Full => None,
                Selection::Partial(selected) => Some(selected),
            },
            ..self.full(typed)
        }
    }

    fn float(self, value: serde_json::Value) -> ProjectionValue {
        match value {
            serde_json::Value::Number(number) => ProjectionValue::Number(number),
            _ if self.typed => ProjectionValue::NonFiniteNumber,
            _ => ProjectionValue::Null,
        }
    }

    fn object(self, capacity: usize) -> ObjectBuilder<'a> {
        // A partial selection keeps at most the properties it names.
        let capacity = self
            .selected
            .map_or(capacity, |selected| capacity.min(selected.keys.len()));
        ObjectBuilder {
            serializer: self,
            entries: Vec::with_capacity(capacity),
            key: None,
            charge_each: false,
        }
    }

    fn array(self, capacity: usize) -> ArrayBuilder<'a> {
        ArrayBuilder {
            serializer: self,
            values: Vec::with_capacity(if self.selected.is_some() { 0 } else { capacity }),
            selected: Vec::new(),
            length: 0,
            charge_each: false,
        }
    }
}

fn integer(value: impl Into<serde_json::Number>) -> Result<ProjectionValue, SerializeError> {
    Ok(ProjectionValue::Number(value.into()))
}

impl<'a> serde::Serializer for ProjectionSerializer<'a> {
    type Ok = ProjectionValue;
    type Error = SerializeError;
    type SerializeSeq = ArrayBuilder<'a>;
    type SerializeTuple = ArrayBuilder<'a>;
    type SerializeTupleStruct = ArrayBuilder<'a>;
    type SerializeTupleVariant = VariantBuilder<ArrayBuilder<'a>>;
    type SerializeMap = ObjectBuilder<'a>;
    type SerializeStruct = StructBuilder<'a>;
    type SerializeStructVariant = VariantBuilder<ObjectBuilder<'a>>;

    fn serialize_bool(self, value: bool) -> Result<ProjectionValue, SerializeError> {
        Ok(ProjectionValue::Bool(value))
    }

    fn serialize_i8(self, value: i8) -> Result<ProjectionValue, SerializeError> {
        integer(i64::from(value))
    }

    fn serialize_i16(self, value: i16) -> Result<ProjectionValue, SerializeError> {
        integer(i64::from(value))
    }

    fn serialize_i32(self, value: i32) -> Result<ProjectionValue, SerializeError> {
        integer(i64::from(value))
    }

    fn serialize_i64(self, value: i64) -> Result<ProjectionValue, SerializeError> {
        integer(value)
    }

    fn serialize_u8(self, value: u8) -> Result<ProjectionValue, SerializeError> {
        integer(u64::from(value))
    }

    fn serialize_u16(self, value: u16) -> Result<ProjectionValue, SerializeError> {
        integer(u64::from(value))
    }

    fn serialize_u32(self, value: u32) -> Result<ProjectionValue, SerializeError> {
        integer(u64::from(value))
    }

    fn serialize_u64(self, value: u64) -> Result<ProjectionValue, SerializeError> {
        integer(value)
    }

    // serde_value has no 128-bit integers, so these never projected.
    fn serialize_i128(self, _: i128) -> Result<ProjectionValue, SerializeError> {
        Err(serialize_error("i128 is not supported"))
    }

    fn serialize_u128(self, _: u128) -> Result<ProjectionValue, SerializeError> {
        Err(serialize_error("u128 is not supported"))
    }

    fn serialize_f32(self, value: f32) -> Result<ProjectionValue, SerializeError> {
        Ok(self.float(serde_json::Value::from(value)))
    }

    fn serialize_f64(self, value: f64) -> Result<ProjectionValue, SerializeError> {
        Ok(self.float(serde_json::Value::from(value)))
    }

    fn serialize_char(self, value: char) -> Result<ProjectionValue, SerializeError> {
        Ok(ProjectionValue::String(value.to_string()))
    }

    fn serialize_str(self, value: &str) -> Result<ProjectionValue, SerializeError> {
        // Charged before the text is copied, like a container's length.
        self.open(text_entries(value.len()))?;
        Ok(ProjectionValue::String(value.to_owned()))
    }

    // serde_value keeps bytes apart from sequences; they are integers anyway.
    fn serialize_bytes(self, value: &[u8]) -> Result<ProjectionValue, SerializeError> {
        self.open(value.len())?;
        Ok(ProjectionValue::Array(
            value
                .iter()
                .map(|&byte| ProjectionValue::Number(byte.into()))
                .collect(),
        ))
    }

    fn serialize_none(self) -> Result<ProjectionValue, SerializeError> {
        Ok(ProjectionValue::Null)
    }

    fn serialize_some<T: Serialize + ?Sized>(
        self,
        value: &T,
    ) -> Result<ProjectionValue, SerializeError> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<ProjectionValue, SerializeError> {
        Ok(ProjectionValue::Null)
    }

    fn serialize_unit_struct(self, _: &'static str) -> Result<ProjectionValue, SerializeError> {
        Ok(ProjectionValue::Null)
    }

    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<ProjectionValue, SerializeError> {
        Ok(ProjectionValue::String(variant.to_owned()))
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        value: &T,
    ) -> Result<ProjectionValue, SerializeError> {
        value.serialize(self)
    }

    // Enum variants are serialized whole; only plain structs, maps and
    // sequences are pruned.
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<ProjectionValue, SerializeError> {
        Ok(ProjectionValue::Object(vec![(
            Cow::Borrowed(variant),
            value.serialize(self.full(self.typed))?,
        )]))
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<ArrayBuilder<'a>, SerializeError> {
        let mut array = self.open(len.unwrap_or(0))?.array(len.unwrap_or(0));
        array.charge_each = len.is_none();
        Ok(array)
    }

    fn serialize_tuple(self, len: usize) -> Result<ArrayBuilder<'a>, SerializeError> {
        Ok(self.open(len)?.array(len))
    }

    fn serialize_tuple_struct(
        self,
        _: &'static str,
        len: usize,
    ) -> Result<ArrayBuilder<'a>, SerializeError> {
        Ok(self.open(len)?.array(len))
    }

    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<VariantBuilder<ArrayBuilder<'a>>, SerializeError> {
        Ok(VariantBuilder {
            variant,
            inner: self.full(self.typed).open(len)?.array(len),
        })
    }

    fn serialize_map(self, len: Option<usize>) -> Result<ObjectBuilder<'a>, SerializeError> {
        let mut object = self.open(len.unwrap_or(0))?.object(len.unwrap_or(0));
        object.charge_each = len.is_none();
        Ok(object)
    }

    fn serialize_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<StructBuilder<'a>, SerializeError> {
        Ok(if name == RAW_VALUE_TOKEN {
            StructBuilder::Raw(self, None)
        } else {
            StructBuilder::Object(self.open(len)?.object(len))
        })
    }

    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<VariantBuilder<ObjectBuilder<'a>>, SerializeError> {
        Ok(VariantBuilder {
            variant,
            inner: self.full(self.typed).open(len)?.object(len),
        })
    }
}

struct ArrayBuilder<'a> {
    serializer: ProjectionSerializer<'a>,
    /// Every element, when the array is serialized whole.
    values: Vec<ProjectionValue>,
    /// Under a partial selection, only the elements filters name.
    selected: Vec<(usize, ProjectionValue)>,
    length: usize,
    /// Whether elements are charged one by one, the length being unknown.
    charge_each: bool,
}

impl ArrayBuilder<'_> {
    fn push<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SerializeError> {
        if self.charge_each {
            self.serializer.open(1)?;
        }
        let typed = self.serializer.typed;
        let index = self.length;
        self.length += 1;
        match self.serializer.selected {
            None => self
                .values
                .push(value.serialize(self.serializer.full(typed))?),
            // An element no filter names only counts toward `length`.
            Some(selected) => {
                if let Some(selection) = selected.element(index) {
                    let value = value.serialize(self.serializer.select(typed, selection))?;
                    self.selected.push((index, value));
                }
            }
        }
        Ok(())
    }

    fn finish(self) -> ProjectionValue {
        match self.serializer.selected {
            None => ProjectionValue::Array(self.values),
            Some(_) => ProjectionValue::SparseArray {
                length: self.length,
                elements: self.selected,
            },
        }
    }
}

impl serde::ser::SerializeSeq for ArrayBuilder<'_> {
    type Ok = ProjectionValue;
    type Error = SerializeError;

    fn serialize_element<T: Serialize + ?Sized>(
        &mut self,
        value: &T,
    ) -> Result<(), SerializeError> {
        self.push(value)
    }

    fn end(self) -> Result<ProjectionValue, SerializeError> {
        Ok(self.finish())
    }
}

impl serde::ser::SerializeTuple for ArrayBuilder<'_> {
    type Ok = ProjectionValue;
    type Error = SerializeError;

    fn serialize_element<T: Serialize + ?Sized>(
        &mut self,
        value: &T,
    ) -> Result<(), SerializeError> {
        self.push(value)
    }

    fn end(self) -> Result<ProjectionValue, SerializeError> {
        Ok(self.finish())
    }
}

impl serde::ser::SerializeTupleStruct for ArrayBuilder<'_> {
    type Ok = ProjectionValue;
    type Error = SerializeError;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SerializeError> {
        self.push(value)
    }

    fn end(self) -> Result<ProjectionValue, SerializeError> {
        Ok(self.finish())
    }
}

enum PendingKey<'a> {
    Skip,
    Keep(ObjectKey, ProjectionSerializer<'a>),
}

struct ObjectBuilder<'a> {
    serializer: ProjectionSerializer<'a>,
    entries: Vec<(ObjectKey, ProjectionValue)>,
    key: Option<PendingKey<'a>>,
    /// Whether entries are charged one by one, the length being unknown.
    charge_each: bool,
}

impl<'a> ObjectBuilder<'a> {
    /// The serializer for a property, or `None` when no filter observes it.
    fn property(&self, key: &str, string_key: bool) -> Option<ProjectionSerializer<'a>> {
        let typed = self.serializer.typed && string_key;
        match self.serializer.selected {
            None => Some(self.serializer.full(typed)),
            Some(selected) => selected
                .property(key)
                .map(|selection| self.serializer.select(typed, selection)),
        }
    }

    /// Like serde_json's insertion-ordered map, a repeated key keeps its first
    /// position and takes the last value.
    fn insert(&mut self, key: ObjectKey, value: ProjectionValue) {
        match self
            .entries
            .iter_mut()
            .find(|(candidate, _)| *candidate == key)
        {
            Some((_, existing)) => *existing = value,
            None => self.entries.push((key, value)),
        }
    }

    fn field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), SerializeError> {
        let account = self.serializer.account;
        let before = account.charged.get();
        if let Some(serializer) = self.property(key, true) {
            let value = value.serialize(serializer)?;
            self.insert(Cow::Borrowed(key), value);
        }
        if self.serializer.root {
            account.record(key, account.charged.get() - before);
        }
        Ok(())
    }
}

impl serde::ser::SerializeMap for ObjectBuilder<'_> {
    type Ok = ProjectionValue;
    type Error = SerializeError;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), SerializeError> {
        if self.charge_each {
            self.serializer.open(1)?;
        }
        let (key, string_key) = map_key(key, self.serializer)?;
        self.key = Some(match self.property(&key, string_key) {
            Some(serializer) => PendingKey::Keep(key, serializer),
            None => PendingKey::Skip,
        });
        Ok(())
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SerializeError> {
        match self.key.take() {
            Some(PendingKey::Keep(key, serializer)) => {
                let value = value.serialize(serializer)?;
                self.insert(key, value);
                Ok(())
            }
            Some(PendingKey::Skip) => Ok(()),
            None => Err(serialize_error(
                "serialize_value called before serialize_key",
            )),
        }
    }

    fn end(self) -> Result<ProjectionValue, SerializeError> {
        Ok(ProjectionValue::Object(self.entries))
    }
}

enum StructBuilder<'a> {
    Object(ObjectBuilder<'a>),
    /// A `RawValue`: its text is parsed and projected as plain JSON.
    Raw(ProjectionSerializer<'a>, Option<ProjectionValue>),
}

impl serde::ser::SerializeStruct for StructBuilder<'_> {
    type Ok = ProjectionValue;
    type Error = SerializeError;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), SerializeError> {
        match self {
            Self::Object(object) => object.field(key, value),
            Self::Raw(serializer, raw) if key == RAW_VALUE_TOKEN => {
                // Charged before the text is copied, so an over-budget raw
                // document is refused without cloning it.
                let KeyKind::Str(text) = value.serialize(KeyProbe(*serializer))? else {
                    return Err(serialize_error("expected raw JSON text"));
                };
                *raw = Some(projection_value_from_json(serde_json::from_str(&text)?));
                Ok(())
            }
            Self::Raw(..) => Err(serialize_error("unexpected raw value field")),
        }
    }

    fn end(self) -> Result<ProjectionValue, SerializeError> {
        match self {
            Self::Object(object) => Ok(ProjectionValue::Object(object.entries)),
            Self::Raw(_, raw) => raw.ok_or_else(|| serialize_error("raw value was not emitted")),
        }
    }
}

struct VariantBuilder<B> {
    variant: &'static str,
    inner: B,
}

impl serde::ser::SerializeTupleVariant for VariantBuilder<ArrayBuilder<'_>> {
    type Ok = ProjectionValue;
    type Error = SerializeError;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SerializeError> {
        self.inner.push(value)
    }

    fn end(self) -> Result<ProjectionValue, SerializeError> {
        Ok(ProjectionValue::Object(vec![(
            Cow::Borrowed(self.variant),
            self.inner.finish(),
        )]))
    }
}

impl serde::ser::SerializeStructVariant for VariantBuilder<ObjectBuilder<'_>> {
    type Ok = ProjectionValue;
    type Error = SerializeError;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), SerializeError> {
        self.inner.field(key, value)
    }

    fn end(self) -> Result<ProjectionValue, SerializeError> {
        Ok(ProjectionValue::Object(vec![(
            Cow::Borrowed(self.variant),
            ProjectionValue::Object(self.inner.entries),
        )]))
    }
}

/// The JSON object key for a map key, and whether serde_value records it as a
/// string. Only string and unit-variant keys are; every other accepted key
/// type goes through serde_json's own key conversion. A string key is charged
/// to `serializer` before it is copied.
fn map_key<T: Serialize + ?Sized>(
    key: &T,
    serializer: ProjectionSerializer<'_>,
) -> Result<(ObjectKey, bool), SerializeError> {
    struct SingleKey<'k, K: ?Sized>(&'k K);

    impl<K: Serialize + ?Sized> Serialize for SingleKey<'_, K> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeMap as _;
            let mut map = serializer.serialize_map(Some(1))?;
            map.serialize_entry(self.0, &())?;
            map.end()
        }
    }

    match key.serialize(KeyProbe(serializer)) {
        Ok(KeyKind::Str(key)) => Ok((Cow::Owned(key), true)),
        Ok(KeyKind::UnitVariant(variant)) => Ok((Cow::Borrowed(variant), true)),
        // Exceeding the budget is not a key type to fall back from.
        Err(error) if serializer.account.exceeded() => Err(error),
        Ok(KeyKind::Other) | Err(_) => match serde_json::to_value(SingleKey(key))? {
            serde_json::Value::Object(map) => map
                .into_iter()
                .next()
                .map(|(key, _)| (Cow::Owned(key), false))
                .ok_or_else(|| serialize_error("map key was not emitted")),
            _ => Err(serialize_error("map key was not emitted")),
        },
    }
}

enum KeyKind {
    Str(String),
    UnitVariant(&'static str),
    Other,
}

/// Classifies a value by the one serde call that encodes it, without recursing.
/// A string is charged to the serializer before it is copied.
struct KeyProbe<'a>(ProjectionSerializer<'a>);

macro_rules! other_key {
    ($($method:ident($type:ty)),* $(,)?) => {
        $(fn $method(self, _: $type) -> Result<KeyKind, SerializeError> {
            Ok(KeyKind::Other)
        })*
    };
}

type NotAKey = serde::ser::Impossible<KeyKind, SerializeError>;

impl serde::Serializer for KeyProbe<'_> {
    type Ok = KeyKind;
    type Error = SerializeError;
    type SerializeSeq = NotAKey;
    type SerializeTuple = NotAKey;
    type SerializeTupleStruct = NotAKey;
    type SerializeTupleVariant = NotAKey;
    type SerializeMap = NotAKey;
    type SerializeStruct = NotAKey;
    type SerializeStructVariant = NotAKey;

    other_key!(
        serialize_bool(bool),
        serialize_i8(i8),
        serialize_i16(i16),
        serialize_i32(i32),
        serialize_i64(i64),
        serialize_i128(i128),
        serialize_u8(u8),
        serialize_u16(u16),
        serialize_u32(u32),
        serialize_u64(u64),
        serialize_u128(u128),
        serialize_f32(f32),
        serialize_f64(f64),
        serialize_char(char),
        serialize_bytes(&[u8]),
        serialize_unit_struct(&'static str),
    );

    fn serialize_str(self, value: &str) -> Result<KeyKind, SerializeError> {
        self.0.open(text_entries(value.len()))?;
        Ok(KeyKind::Str(value.to_owned()))
    }

    fn serialize_none(self) -> Result<KeyKind, SerializeError> {
        Ok(KeyKind::Other)
    }

    fn serialize_some<T: Serialize + ?Sized>(self, _: &T) -> Result<KeyKind, SerializeError> {
        Ok(KeyKind::Other)
    }

    fn serialize_unit(self) -> Result<KeyKind, SerializeError> {
        Ok(KeyKind::Other)
    }

    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<KeyKind, SerializeError> {
        Ok(KeyKind::UnitVariant(variant))
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: &T,
    ) -> Result<KeyKind, SerializeError> {
        Ok(KeyKind::Other)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: &T,
    ) -> Result<KeyKind, SerializeError> {
        Ok(KeyKind::Other)
    }

    fn serialize_seq(self, _: Option<usize>) -> Result<NotAKey, SerializeError> {
        Err(serialize_error("not a key"))
    }

    fn serialize_tuple(self, _: usize) -> Result<NotAKey, SerializeError> {
        Err(serialize_error("not a key"))
    }

    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<NotAKey, SerializeError> {
        Err(serialize_error("not a key"))
    }

    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<NotAKey, SerializeError> {
        Err(serialize_error("not a key"))
    }

    fn serialize_map(self, _: Option<usize>) -> Result<NotAKey, SerializeError> {
        Err(serialize_error("not a key"))
    }

    fn serialize_struct(self, _: &'static str, _: usize) -> Result<NotAKey, SerializeError> {
        Err(serialize_error("not a key"))
    }

    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<NotAKey, SerializeError> {
        Err(serialize_error("not a key"))
    }
}

/// An object property name: borrowed for DTO fields and enum variants, which
/// are `'static`, and owned only for map keys and parsed JSON.
pub(super) type ObjectKey = Cow<'static, str>;

#[derive(Clone, Debug, PartialEq)]
pub(super) enum ProjectionValue {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(String),
    Utf16CodeUnit(u16),
    Array(Vec<Self>),
    /// An array reached only through nested filters: its length and, in
    /// ascending index order, the elements those filters name. Every other
    /// element is unobserved and reads as `null`.
    SparseArray {
        length: usize,
        elements: Vec<(usize, Self)>,
    },
    Object(Vec<(ObjectKey, Self)>),
    NonFiniteNumber,
}

/// Reference semantics for `serialize_selected`: the full serde_json tree, with
/// nonfinite floats recovered from the serde_value tree of the same value.
#[cfg(test)]
pub(super) fn serialize_projection_value<T>(value: &T) -> Result<ProjectionValue, ApiError>
where
    T: Serialize,
{
    let json =
        serde_json::to_value(value).map_err(|error| ApiError::serialization(error.to_string()))?;
    let typed =
        serde_value::to_value(value).map_err(|error| ApiError::serialization(error.to_string()))?;
    Ok(projection_value_from_serializations(json, &typed))
}

#[cfg(test)]
pub(super) fn projection_value_from_serializations(
    json: serde_json::Value,
    typed: &serde_value::Value,
) -> ProjectionValue {
    let typed = unwrap_serde_value(typed);
    match json {
        serde_json::Value::Null
            if matches!(
                typed,
                serde_value::Value::F32(value) if !value.is_finite()
            ) || matches!(
                typed,
                serde_value::Value::F64(value) if !value.is_finite()
            ) =>
        {
            ProjectionValue::NonFiniteNumber
        }
        serde_json::Value::Null => ProjectionValue::Null,
        serde_json::Value::Bool(value) => ProjectionValue::Bool(value),
        serde_json::Value::Number(value) => ProjectionValue::Number(value),
        serde_json::Value::String(value) => ProjectionValue::String(value),
        serde_json::Value::Array(values) => {
            let typed_values = match typed {
                serde_value::Value::Seq(values) => Some(values.as_slice()),
                _ => None,
            };
            ProjectionValue::Array(
                values
                    .into_iter()
                    .enumerate()
                    .map(
                        |(index, value)| match typed_values.and_then(|values| values.get(index)) {
                            Some(typed) => projection_value_from_serializations(value, typed),
                            None => projection_value_from_json(value),
                        },
                    )
                    .collect(),
            )
        }
        serde_json::Value::Object(object) => {
            let typed_object = match typed {
                serde_value::Value::Map(object) => Some(object),
                _ => None,
            };
            ProjectionValue::Object(
                object
                    .into_iter()
                    .map(|(key, value)| {
                        let typed_value = typed_object.and_then(|object| {
                            object.iter().find_map(|(candidate, value)| {
                                matches!(candidate, serde_value::Value::String(candidate) if candidate == &key)
                                    .then_some(value)
                            })
                        });
                        let value = match typed_value {
                            Some(typed) => projection_value_from_serializations(value, typed),
                            None => projection_value_from_json(value),
                        };
                        (Cow::Owned(key), value)
                    })
                    .collect(),
            )
        }
    }
}

#[cfg(test)]
pub(super) fn unwrap_serde_value(mut value: &serde_value::Value) -> &serde_value::Value {
    loop {
        value = match value {
            serde_value::Value::Newtype(inner) | serde_value::Value::Option(Some(inner)) => inner,
            _ => return value,
        };
    }
}

pub(super) fn projection_value_from_json(value: serde_json::Value) -> ProjectionValue {
    match value {
        serde_json::Value::Null => ProjectionValue::Null,
        serde_json::Value::Bool(value) => ProjectionValue::Bool(value),
        serde_json::Value::Number(value) => ProjectionValue::Number(value),
        serde_json::Value::String(value) => ProjectionValue::String(value),
        serde_json::Value::Array(values) => {
            ProjectionValue::Array(values.into_iter().map(projection_value_from_json).collect())
        }
        serde_json::Value::Object(object) => ProjectionValue::Object(
            object
                .into_iter()
                .map(|(key, value)| (Cow::Owned(key), projection_value_from_json(value)))
                .collect(),
        ),
    }
}

#[cfg(test)]
pub(super) fn encode_javascript_json(value: &ProjectionValue) -> Result<Bytes, ApiError> {
    let mut bytes = Vec::new();
    write_projection_json(&mut bytes, value)
        .map_err(|error| ApiError::serialization(error.to_string()))?;
    Ok(Bytes::from(bytes))
}

/// Writes the projection of `value` through `filters`: the bytes
/// `encode_javascript_json(&project_filters(value, filters))` produces.
#[cfg(test)]
pub(super) fn encode_projection(
    value: &ProjectionValue,
    filters: &[Filter],
) -> Result<Bytes, ApiError> {
    encode_root(&mut Vec::new(), filters, &|key| {
        javascript_property_ref(value, key)
    })
}

/// Writes the projection, through `filters`, of the object whose JavaScript
/// properties `property` reads, into `scratch`, without materializing or
/// cloning the projected tree.
fn encode_root<'v>(
    scratch: &mut Vec<u8>,
    filters: &[Filter],
    property: &dyn Fn(&str) -> Option<Cow<'v, ProjectionValue>>,
) -> Result<Bytes, ApiError> {
    scratch.clear();
    write_projection(scratch, filters, property)
        .map_err(|error| ApiError::serialization(error.to_string()))?;
    Ok(Bytes::copy_from_slice(scratch))
}

/// Writes `text` as a JSON string, escaped exactly as serde_json escapes it.
/// Text that needs no escaping, as almost every key and value does, is copied
/// between quotes directly.
fn write_string(bytes: &mut Vec<u8>, text: &str) -> io::Result<()> {
    if text
        .bytes()
        .all(|byte| byte >= 0x20 && byte != b'"' && byte != b'\\')
    {
        bytes.reserve(text.len() + 2);
        bytes.push(b'"');
        bytes.extend_from_slice(text.as_bytes());
        bytes.push(b'"');
        return Ok(());
    }
    Ok(serde_json::to_writer(bytes, text)?)
}

/// A property `write_projection` emits: written whole, or projected through
/// the nested filters that reach it.
enum Projected<'v, 'f> {
    Whole(Cow<'v, ProjectionValue>),
    Filtered(Cow<'v, ProjectionValue>, &'f [Filter]),
}

impl Projected<'_, '_> {
    /// The property one filter projects, if it emits one.
    fn resolve<'v, 'f>(
        filter: &'f Filter,
        property: &dyn Fn(&str) -> Option<Cow<'v, ProjectionValue>>,
    ) -> Option<(&'f str, Projected<'v, 'f>)> {
        match filter {
            Filter::Field(field) => Some((field, Projected::Whole(property(field)?))),
            Filter::Nested { field, keys } => match property(field)? {
                property if matches!(*property, ProjectionValue::Null) => None,
                property => Some((field, Projected::Filtered(property, keys))),
            },
        }
    }

    fn write(&self, bytes: &mut Vec<u8>) -> io::Result<()> {
        match self {
            Projected::Whole(value) => write_projection_json(bytes, value),
            Projected::Filtered(value, keys) => {
                write_projection(bytes, keys, &|key| javascript_property_ref(value, key))
            }
        }
    }
}

/// Up to this many filters are checked for writing in their own order.
const IN_ORDER_FILTERS: usize = 8;

fn filter_field(filter: &Filter) -> &str {
    let (Filter::Field(field) | Filter::Nested { field, .. }) = filter;
    field
}

/// Whether the projection through `filters` emits its properties in filter
/// order: no field repeats, and no array-index key has to move ahead of
/// another property.
fn in_filter_order(filters: &[Filter]) -> bool {
    match filters {
        [] | [_] => true,
        _ if filters.len() > IN_ORDER_FILTERS => false,
        _ => filters.iter().enumerate().all(|(position, filter)| {
            let field = filter_field(filter);
            javascript_array_index(field).is_none()
                && filters[..position]
                    .iter()
                    .all(|earlier| filter_field(earlier) != field)
        }),
    }
}

/// Writes the object `filters` project from the value whose JavaScript
/// properties `property` reads.
fn write_projection<'v>(
    bytes: &mut Vec<u8>,
    filters: &[Filter],
    property: &dyn Fn(&str) -> Option<Cow<'v, ProjectionValue>>,
) -> io::Result<()> {
    if in_filter_order(filters) {
        bytes.push(b'{');
        let mut written = false;
        for (field, projected) in filters
            .iter()
            .filter_map(|filter| Projected::resolve(filter, property))
        {
            if written {
                bytes.push(b',');
            }
            written = true;
            write_string(bytes, field)?;
            bytes.push(b':');
            projected.write(bytes)?;
        }
        bytes.push(b'}');
        return Ok(());
    }
    let mut entries: Vec<(&str, Projected<'_, '_>)> = Vec::with_capacity(filters.len());
    for (field, projected) in filters
        .iter()
        .filter_map(|filter| Projected::resolve(filter, property))
    {
        // A repeated field keeps its first position and takes the last value.
        match entries.iter_mut().find(|(key, _)| *key == field) {
            Some((_, existing)) => *existing = projected,
            None => entries.push((field, projected)),
        }
    }
    write_object(bytes, &entries, |bytes, projected| projected.write(bytes))
}

/// Writes an object in JavaScript property order, as
/// `canonicalize_javascript_keys` arranges it: array-index keys ascending,
/// then every other key in insertion order.
fn write_object<K: AsRef<str>, V>(
    bytes: &mut Vec<u8>,
    entries: &[(K, V)],
    mut write_value: impl FnMut(&mut Vec<u8>, &V) -> io::Result<()>,
) -> io::Result<()> {
    // Up to this many index keys are ordered by repeated minimum search,
    // which needs no allocation; more are sorted.
    const SCANNED_INDICES: usize = 8;
    let index = |(key, _): &(K, V)| javascript_array_index(key.as_ref());
    let index_count = entries
        .iter()
        .filter(|entry| index(entry).is_some())
        .count();
    let mut written = 0;
    let mut write_entry = |bytes: &mut Vec<u8>, (key, value): &(K, V)| -> io::Result<()> {
        if written != 0 {
            bytes.push(b',');
        }
        written += 1;
        write_string(bytes, key.as_ref())?;
        bytes.push(b':');
        write_value(bytes, value)
    };
    bytes.push(b'{');
    if index_count <= SCANNED_INDICES {
        // Ordered by (index, position) like the stable sort below, so every
        // counted entry is found exactly once.
        let mut previous = None;
        for _ in 0..index_count {
            let (order, entry) = entries
                .iter()
                .enumerate()
                .filter_map(|(position, entry)| {
                    index(entry).map(|index| ((index, position), entry))
                })
                .filter(|(order, _)| previous.is_none_or(|previous| *order > previous))
                .min_by_key(|(order, _)| *order)
                .expect("a counted index key remains");
            previous = Some(order);
            write_entry(bytes, entry)?;
        }
    } else {
        let mut indices: Vec<_> = entries
            .iter()
            .filter_map(|entry| index(entry).map(|index| (index, entry)))
            .collect();
        indices.sort_by_key(|&(index, _)| index);
        for (_, entry) in indices {
            write_entry(bytes, entry)?;
        }
    }
    for entry in entries
        .iter()
        .filter(|entry| index_count == 0 || index(entry).is_none())
    {
        write_entry(bytes, entry)?;
    }
    bytes.push(b'}');
    Ok(())
}

/// Writes `value` as JSON with every object in JavaScript property order.
pub(super) fn write_projection_json(
    bytes: &mut Vec<u8>,
    value: &ProjectionValue,
) -> io::Result<()> {
    match value {
        ProjectionValue::Null | ProjectionValue::NonFiniteNumber => {
            bytes.extend_from_slice(b"null")
        }
        ProjectionValue::Bool(value) => {
            bytes.extend_from_slice(if *value { b"true" } else { b"false" })
        }
        ProjectionValue::Number(value) => {
            if let Some(value) = value.as_i64() {
                write_integer(bytes, value < 0, value.unsigned_abs());
            } else if let Some(value) = value.as_u64() {
                write_integer(bytes, false, value);
            } else {
                let value = value.as_f64().expect("JSON number");
                // JavaScript prints an integral number of smaller magnitude
                // as its plain digits; zero, of either sign, as `0`.
                if value.fract() == 0.0 && value.abs() < MAX_PLAIN_INTEGER {
                    write_integer(bytes, value < 0.0, value.abs() as u64);
                } else {
                    crate::calculation::write_js_json_number(bytes, value)?;
                }
            }
        }
        ProjectionValue::String(value) => write_string(bytes, value)?,
        ProjectionValue::Utf16CodeUnit(unit) => {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            bytes.extend_from_slice(b"\"\\u");
            bytes.push(HEX[usize::from(unit >> 12)]);
            bytes.push(HEX[usize::from((unit >> 8) & 0xf)]);
            bytes.push(HEX[usize::from((unit >> 4) & 0xf)]);
            bytes.push(HEX[usize::from(unit & 0xf)]);
            bytes.push(b'"');
        }
        ProjectionValue::Array(values) => {
            bytes.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    bytes.push(b',');
                }
                write_projection_json(bytes, value)?;
            }
            bytes.push(b']');
        }
        // Never reached by a projection (a `Field` filter selects the whole
        // array); written like the dense array it stands for.
        ProjectionValue::SparseArray { length, elements } => {
            let mut elements = elements.iter().peekable();
            bytes.push(b'[');
            for index in 0..*length {
                if index != 0 {
                    bytes.push(b',');
                }
                match elements.next_if(|(position, _)| *position == index) {
                    Some((_, value)) => write_projection_json(bytes, value)?,
                    None => bytes.extend_from_slice(b"null"),
                }
            }
            bytes.push(b']');
        }
        ProjectionValue::Object(entries) => write_object(bytes, entries, write_projection_json)?,
    }
    Ok(())
}

/// 2^53: every integral number below it in magnitude is exactly an integer.
const MAX_PLAIN_INTEGER: f64 = 9_007_199_254_740_992.0;

/// Writes the decimal digits of `magnitude`, preceded by `-` when
/// `negative`.
fn write_integer(bytes: &mut Vec<u8>, negative: bool, mut magnitude: u64) {
    let mut digits = [0; 20];
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (magnitude % 10) as u8;
        magnitude /= 10;
        if magnitude == 0 {
            break;
        }
    }
    if negative {
        bytes.push(b'-');
    }
    bytes.extend_from_slice(&digits[start..]);
}

#[cfg(test)]
pub(super) fn project_filters(value: &ProjectionValue, filters: &[Filter]) -> ProjectionValue {
    let mut projected = Vec::new();
    for filter in filters {
        match filter {
            Filter::Field(field) => {
                if let Some(value) = javascript_property(value, field) {
                    if let Some((_, existing)) = projected.iter_mut().find(|(key, _)| key == field)
                    {
                        *existing = value;
                    } else {
                        projected.push((Cow::Owned(field.clone()), value));
                    }
                }
            }
            Filter::Nested { field, keys } => {
                if let Some(value) = javascript_property(value, field)
                    .filter(|value| value != &ProjectionValue::Null)
                {
                    let value = project_filters(&value, keys);
                    if let Some((_, existing)) = projected.iter_mut().find(|(key, _)| key == field)
                    {
                        *existing = value;
                    } else {
                        projected.push((Cow::Owned(field.clone()), value));
                    }
                }
            }
        }
    }
    ProjectionValue::Object(projected)
}

#[cfg(test)]
pub(super) fn javascript_property(value: &ProjectionValue, key: &str) -> Option<ProjectionValue> {
    javascript_property_ref(value, key).map(Cow::into_owned)
}

/// The value JavaScript reads for `value[key]`, borrowed where it exists in
/// `value` and synthesized for `length` and string code units.
pub(super) fn javascript_property_ref<'v>(
    value: &'v ProjectionValue,
    key: &str,
) -> Option<Cow<'v, ProjectionValue>> {
    match value {
        ProjectionValue::Object(object) => object
            .iter()
            .find_map(|(candidate, value)| (candidate == key).then_some(Cow::Borrowed(value))),
        ProjectionValue::Array(array) if key == "length" => Some(Cow::Owned(
            ProjectionValue::Number(serde_json::Number::from(array.len())),
        )),
        ProjectionValue::Array(array) => javascript_array_index(key)
            .and_then(|index| usize::try_from(index).ok())
            .and_then(|index| array.get(index))
            .map(Cow::Borrowed),
        ProjectionValue::SparseArray { length, .. } if key == "length" => Some(Cow::Owned(
            ProjectionValue::Number(serde_json::Number::from(*length)),
        )),
        ProjectionValue::SparseArray { length, elements } => javascript_array_index(key)
            .and_then(|index| usize::try_from(index).ok())
            .filter(|index| index < length)
            .map(|index| {
                elements
                    .iter()
                    .find_map(|(position, value)| (*position == index).then_some(value))
                    .map_or(Cow::Owned(ProjectionValue::Null), Cow::Borrowed)
            }),
        ProjectionValue::String(string) if key == "length" => Some(Cow::Owned(
            ProjectionValue::Number(serde_json::Number::from(string.encode_utf16().count())),
        )),
        ProjectionValue::String(string) => javascript_array_index(key)
            .and_then(|index| usize::try_from(index).ok())
            .and_then(|index| string.encode_utf16().nth(index))
            .map(|unit| {
                Cow::Owned(
                    char::from_u32(u32::from(unit))
                        .map_or(ProjectionValue::Utf16CodeUnit(unit), |character| {
                            ProjectionValue::String(character.to_string())
                        }),
                )
            }),
        ProjectionValue::Utf16CodeUnit(_) if key == "length" => Some(Cow::Owned(
            ProjectionValue::Number(serde_json::Number::from(1)),
        )),
        ProjectionValue::Utf16CodeUnit(_) if javascript_array_index(key) == Some(0) => {
            Some(Cow::Borrowed(value))
        }
        _ => None,
    }
}

/// The array index `key` names: the canonical decimal form of an integer
/// below 2^32 - 1, so no sign, leading zero or other spelling.
pub(super) fn javascript_array_index(key: &str) -> Option<u32> {
    if key.is_empty()
        || (key.len() > 1 && key.starts_with('0'))
        || !key.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let index = key.parse::<u32>().ok()?;
    (index != u32::MAX).then_some(index)
}

#[cfg(test)]
pub(super) fn canonicalize_javascript_keys(value: ProjectionValue) -> ProjectionValue {
    match value {
        ProjectionValue::Object(object) => {
            let mut indices = Vec::new();
            let mut others = Vec::new();
            for (key, value) in object {
                let value = canonicalize_javascript_keys(value);
                if let Some(index) = javascript_array_index(&key) {
                    indices.push((index, key, value));
                } else {
                    others.push((key, value));
                }
            }
            indices.sort_by_key(|(index, _, _)| *index);
            ProjectionValue::Object(
                indices
                    .into_iter()
                    .map(|(_, key, value)| (key, value))
                    .chain(others)
                    .collect(),
            )
        }
        ProjectionValue::Array(values) => ProjectionValue::Array(
            values
                .into_iter()
                .map(canonicalize_javascript_keys)
                .collect(),
        ),
        ProjectionValue::SparseArray { length, elements } => ProjectionValue::SparseArray {
            length,
            elements: elements
                .into_iter()
                .map(|(index, value)| (index, canonicalize_javascript_keys(value)))
                .collect(),
        },
        value => value,
    }
}

/// Request authorization policy whose immutable snapshot is replaced on config
/// reload. Requests already in flight keep their loaded policy; subsequent
/// requests observe the new allow-list and server host without a listener
/// restart.
pub struct ReloadableAuthorizer {
    policy: ArcSwap<AuthorizationPolicy>,
}

impl ReloadableAuthorizer {
    pub fn new(allowed_ips: &str, server_host: &str) -> Self {
        Self {
            policy: ArcSwap::from_pointee(AuthorizationPolicy::new(allowed_ips, server_host)),
        }
    }

    pub fn reload(&self, allowed_ips: &str, server_host: &str) {
        self.policy
            .store(Arc::new(AuthorizationPolicy::new(allowed_ips, server_host)));
    }
}

impl RequestAuthorizer for ReloadableAuthorizer {
    fn authorize(
        &self,
        peer: std::net::IpAddr,
        headers: RequestHeaders<'_>,
    ) -> AuthorizationDecision {
        self.policy.load().authorize(peer, headers)
    }
}

/// Concrete HTTP and WebSocket state provider over one atomic snapshot owner.
///
/// API consumers read `snapshots`. The selected-field subscription reads
/// `live`, which also carries the samples taken between API cadence points.
pub struct SnapshotProviders {
    snapshots: Arc<SnapshotManager>,
    live: SnapshotManager,
    encoder: Arc<dyn SnapshotEncoder>,
    regular_event: watch::Sender<u64>,
    precise_event: watch::Sender<u64>,
    live_event: watch::Sender<u64>,
    regular_poll_ms: AtomicU64,
    precise_poll_ms: AtomicU64,
}

impl SnapshotProviders {
    pub fn new(initial: Arc<ManagerSnapshot>, encoder: Arc<dyn SnapshotEncoder>) -> Self {
        let revision = initial.revision();
        let (regular_event, _) = watch::channel(revision);
        let (precise_event, _) = watch::channel(revision);
        let (live_event, _) = watch::channel(revision);
        Self {
            snapshots: Arc::new(SnapshotManager::new(Arc::clone(&initial))),
            live: SnapshotManager::new(initial),
            encoder,
            regular_event,
            precise_event,
            live_event,
            regular_poll_ms: AtomicU64::new(150),
            precise_poll_ms: AtomicU64::new(10),
        }
    }

    pub fn snapshot_manager(&self) -> Arc<SnapshotManager> {
        Arc::clone(&self.snapshots)
    }

    pub fn set_poll_rates(&self, regular: Duration, precise: Duration) {
        self.regular_poll_ms.store(
            regular.as_millis().try_into().unwrap_or(u64::MAX),
            Ordering::Release,
        );
        self.precise_poll_ms.store(
            precise.as_millis().try_into().unwrap_or(u64::MAX),
            Ordering::Release,
        );
    }

    /// Publishes one atomic state point to API and live consumers alike.
    pub fn publish(&self, manager: Arc<ManagerSnapshot>) -> Arc<CombinedSnapshot> {
        let snapshot = self.publish_api(Arc::clone(&manager));
        self.publish_live(manager);
        snapshot
    }

    /// Publishes one atomic API state point. Precise consumers depend on every
    /// component and are always woken. Regular schemas are not woken for a
    /// precise-only replacement, matching the source's independent cadence.
    pub fn publish_api(&self, manager: Arc<ManagerSnapshot>) -> Arc<CombinedSnapshot> {
        let revision = manager.revision();
        let previous = self.snapshots.load();
        let snapshot = self.snapshots.publish(manager);
        let regular_same = match (previous.regular(), snapshot.regular()) {
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            (None, None) => true,
            _ => false,
        };
        let precise_same = match (previous.precise(), snapshot.precise()) {
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            (None, None) => true,
            _ => false,
        };
        if !regular_same || precise_same {
            self.regular_event.send_replace(revision);
        }
        self.precise_event.send_replace(revision);
        snapshot
    }

    /// Publishes one atomic state point for the selected-field subscription only.
    pub fn publish_live(&self, manager: Arc<ManagerSnapshot>) {
        let revision = manager.revision();
        self.live.publish(manager);
        self.live_event.send_replace(revision);
    }

    pub fn load(&self) -> Arc<CombinedSnapshot> {
        self.snapshots.load()
    }
}

impl StateJsonProvider for SnapshotProviders {
    fn cached_json(
        &self,
        route: StateRoute,
    ) -> crate::server::routes::ProviderFuture<'_, CachedJson> {
        Box::pin(async move {
            let schema = api_schema(route);
            let snapshot = self.snapshots.load();
            if snapshot.focused().is_none() {
                return CachedJson::NotReady;
            }
            match self.encoder.encode(&snapshot, schema, &[]) {
                Ok(bytes) => CachedJson::Ready(bytes),
                Err(ApiError::NotReady) => CachedJson::NotReady,
                Err(error) => CachedJson::Failed {
                    status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    body: Bytes::from(
                        serde_json::to_vec(&serde_json::json!({"error": error.to_string()}))
                            .expect("error JSON serializes"),
                    ),
                },
            }
        })
    }
}

struct V2SubscriptionSnapshotView {
    snapshot: Arc<CombinedSnapshot>,
}

impl V2SubscriptionSnapshot for V2SubscriptionSnapshotView {
    fn revision(&self) -> u64 {
        self.snapshot.manager().revision()
    }

    fn document(&self, pointers: &[&[String]]) -> Result<Option<serde_json::Value>, ApiError> {
        match ProductionSnapshotEncoder::subscription_document(&self.snapshot, pointers) {
            Ok(document) => Ok(Some(document)),
            Err(ApiError::NotReady) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

impl V2SubscriptionProvider for SnapshotProviders {
    fn subscribe(&self) -> watch::Receiver<u64> {
        self.live_event.subscribe()
    }

    fn capture(&self) -> Arc<dyn V2SubscriptionSnapshot> {
        Arc::new(V2SubscriptionSnapshotView {
            snapshot: self.live.load(),
        })
    }
}

pub(super) struct WebSocketSnapshotView {
    snapshot: Arc<CombinedSnapshot>,
    schema: ApiSchema,
    encoder: Arc<dyn SnapshotEncoder>,
}

impl WebSocketStateSnapshot for WebSocketSnapshotView {
    fn generation(&self) -> u64 {
        self.snapshot.manager().revision()
    }

    fn encode_unfiltered(&self) -> Result<Bytes, String> {
        self.encoder
            .encode(&self.snapshot, self.schema, &[])
            .map_err(|error| error.to_string())
    }

    fn encode_filtered(&self, filters: &[Filter]) -> Result<Bytes, String> {
        self.encoder
            .encode(&self.snapshot, self.schema, filters)
            .map_err(|error| error.to_string())
    }
}

impl WebSocketStateProvider for SnapshotProviders {
    fn poll_rate(&self, schema: WebSocketSchema) -> Duration {
        let milliseconds = if schema == WebSocketSchema::V2Precise {
            self.precise_poll_ms.load(Ordering::Acquire)
        } else {
            self.regular_poll_ms.load(Ordering::Acquire)
        };
        Duration::from_millis(milliseconds)
    }

    fn subscribe(&self, schema: WebSocketSchema) -> watch::Receiver<u64> {
        if schema == WebSocketSchema::V2Precise {
            self.precise_event.subscribe()
        } else {
            self.regular_event.subscribe()
        }
    }

    fn capture(&self, schema: WebSocketSchema) -> Option<Arc<dyn WebSocketStateSnapshot>> {
        let snapshot = self.snapshots.load();
        snapshot.focused()?;
        Some(Arc::new(WebSocketSnapshotView {
            snapshot,
            schema: api_schema_ws(schema),
            encoder: Arc::clone(&self.encoder),
        }))
    }
}

pub(super) const fn api_schema(route: StateRoute) -> ApiSchema {
    match route {
        StateRoute::V1 => ApiSchema::V1,
        StateRoute::StreamCompanion => ApiSchema::StreamCompanion,
        StateRoute::V2 => ApiSchema::V2,
        StateRoute::V2Precise => ApiSchema::V2Precise,
    }
}

pub(super) const fn api_schema_ws(schema: WebSocketSchema) -> ApiSchema {
    match schema {
        WebSocketSchema::V1 => ApiSchema::V1,
        WebSocketSchema::StreamCompanion => ApiSchema::StreamCompanion,
        WebSocketSchema::V2 => ApiSchema::V2,
        WebSocketSchema::V2Precise => ApiSchema::V2Precise,
    }
}
