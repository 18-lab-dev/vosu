//! Bounded serialization of only the values legacy filters can observe.
use super::*;
use std::cell::{Cell, RefCell};

/// The part of a document that a filter list can observe. A property named by
/// a `Field` filter is serialized in full; one reached only through `Nested`
/// filters keeps just the properties those filters name. Every lookup that
/// `project_filters` performs therefore sees the same value as in the full
/// document, while the unobserved remainder is never serialized.
///
/// Pruning skips unobserved subtrees entirely, so their serialization cannot
/// fail. That is not a behavioral difference here: the wire DTOs serialize
/// infallibly (the unfiltered encoders write the same DTOs without errors).
pub enum Selection<'a> {
    Full,
    Partial(Selected<'a>),
}

#[derive(Default)]
pub struct Selected<'a> {
    pub(super) keys: Vec<SelectedKey<'a>>,
}

pub(super) struct SelectedKey<'a> {
    pub(super) key: &'a str,
    pub(super) index: Option<u32>,
    pub(super) selection: Selection<'a>,
}

impl<'a> Selected<'a> {
    pub fn from_filters(filters: &'a [Filter]) -> Self {
        let mut selected = Self::default();
        selected.add(filters);
        selected
    }

    pub fn add_pointer(&mut self, pointer: &'a [String]) {
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

    pub fn entry(&mut self, key: &'a str) -> &mut Selection<'a> {
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
pub fn serialize_selected<T: Serialize + ?Sized>(
    value: &T,
    selected: &Selected<'_>,
) -> Result<ProjectionValue, serde_json::Error> {
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
pub(super) fn serialize_accounted<T: Serialize + ?Sized>(
    value: &T,
    selected: &Selected<'_>,
    account: &Account,
) -> Result<ProjectionValue, serde_json::Error> {
    value.serialize(ProjectionSerializer {
        typed: true,
        selected: Some(selected),
        account,
        root: true,
    })
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
pub(super) struct Account {
    limit: usize,
    charged: Cell<usize>,
    recorded: Cell<usize>,
    properties: RefCell<[(&'static str, usize); RECORDED_PROPERTIES]>,
}

impl Account {
    pub(super) fn new(limit: usize) -> Self {
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

    pub(super) fn exceeded(&self) -> bool {
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
    pub(super) fn into_properties(self) -> impl Iterator<Item = (&'static str, usize)> {
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
