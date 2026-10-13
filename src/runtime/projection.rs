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
        let selected = Selected::from_filters(filters);
        let value = match schema {
            ApiSchema::V1 => serialize_selected(&crate::api::v1::build(snapshot)?, &selected)?,
            ApiSchema::V2 => serialize_selected(&crate::api::v2::build(snapshot)?, &selected)?,
            ApiSchema::StreamCompanion => {
                serialize_selected(&crate::api::sc::build(snapshot)?, &selected)?
            }
            ApiSchema::V2Precise => {
                serialize_selected(&crate::api::precise::build(snapshot)?, &selected)?
            }
        };
        let projected = canonicalize_javascript_keys(project_filters(&value, filters));
        encode_javascript_json(&projected)
    }
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

/// Serializes `value` straight into the projection tree, restricted to
/// `selected`. The result is what `serialize_projection_value` derives from
/// the serde_json and serde_value trees of the same value: serde_json's number,
/// map-key, enum and `RawValue` conventions, with nonfinite floats kept
/// distinct from `null` wherever serde_value's tree is reached by string keys.
///
/// Not reproduced: a single map holding keys of different types that convert
/// to the same JSON string, which no wire DTO can contain.
pub(super) fn serialize_selected<T: Serialize + ?Sized>(
    value: &T,
    selected: &Selected<'_>,
) -> Result<ProjectionValue, ApiError> {
    value
        .serialize(ProjectionSerializer {
            typed: true,
            selected: Some(selected),
        })
        .map_err(|error| ApiError::serialization(error.to_string()))
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
}

impl<'a> ProjectionSerializer<'a> {
    fn full(typed: bool) -> Self {
        Self {
            typed,
            selected: None,
        }
    }

    fn select(typed: bool, selection: &'a Selection<'a>) -> Self {
        Self {
            typed,
            selected: match selection {
                Selection::Full => None,
                Selection::Partial(selected) => Some(selected),
            },
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
        ObjectBuilder {
            serializer: self,
            entries: Vec::with_capacity(capacity),
            key: None,
        }
    }

    fn array(self, capacity: usize) -> ArrayBuilder<'a> {
        ArrayBuilder {
            serializer: self,
            values: Vec::with_capacity(capacity),
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
        Ok(ProjectionValue::String(value.to_owned()))
    }

    // serde_value keeps bytes apart from sequences; they are integers anyway.
    fn serialize_bytes(self, value: &[u8]) -> Result<ProjectionValue, SerializeError> {
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
            variant.to_owned(),
            value.serialize(Self::full(self.typed))?,
        )]))
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<ArrayBuilder<'a>, SerializeError> {
        Ok(self.array(len.unwrap_or(0)))
    }

    fn serialize_tuple(self, len: usize) -> Result<ArrayBuilder<'a>, SerializeError> {
        Ok(self.array(len))
    }

    fn serialize_tuple_struct(
        self,
        _: &'static str,
        len: usize,
    ) -> Result<ArrayBuilder<'a>, SerializeError> {
        Ok(self.array(len))
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
            inner: Self::full(self.typed).array(len),
        })
    }

    fn serialize_map(self, len: Option<usize>) -> Result<ObjectBuilder<'a>, SerializeError> {
        Ok(self.object(len.unwrap_or(0)))
    }

    fn serialize_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<StructBuilder<'a>, SerializeError> {
        Ok(if name == RAW_VALUE_TOKEN {
            StructBuilder::Raw(None)
        } else {
            StructBuilder::Object(self.object(len))
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
            inner: Self::full(self.typed).object(len),
        })
    }
}

struct ArrayBuilder<'a> {
    serializer: ProjectionSerializer<'a>,
    values: Vec<ProjectionValue>,
}

impl ArrayBuilder<'_> {
    fn push<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SerializeError> {
        let typed = self.serializer.typed;
        let value = match self.serializer.selected {
            None => value.serialize(ProjectionSerializer::full(typed))?,
            Some(selected) => match selected.element(self.values.len()) {
                Some(selection) => {
                    value.serialize(ProjectionSerializer::select(typed, selection))?
                }
                // No filter reaches this element; it only keeps `length` and
                // the following indices in place.
                None => ProjectionValue::Null,
            },
        };
        self.values.push(value);
        Ok(())
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
        Ok(ProjectionValue::Array(self.values))
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
        Ok(ProjectionValue::Array(self.values))
    }
}

impl serde::ser::SerializeTupleStruct for ArrayBuilder<'_> {
    type Ok = ProjectionValue;
    type Error = SerializeError;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SerializeError> {
        self.push(value)
    }

    fn end(self) -> Result<ProjectionValue, SerializeError> {
        Ok(ProjectionValue::Array(self.values))
    }
}

enum PendingKey<'a> {
    Skip,
    Keep(String, ProjectionSerializer<'a>),
}

struct ObjectBuilder<'a> {
    serializer: ProjectionSerializer<'a>,
    entries: Vec<(String, ProjectionValue)>,
    key: Option<PendingKey<'a>>,
}

impl<'a> ObjectBuilder<'a> {
    /// The serializer for a property, or `None` when no filter observes it.
    fn property(&self, key: &str, string_key: bool) -> Option<ProjectionSerializer<'a>> {
        let typed = self.serializer.typed && string_key;
        match self.serializer.selected {
            None => Some(ProjectionSerializer::full(typed)),
            Some(selected) => selected
                .property(key)
                .map(|selection| ProjectionSerializer::select(typed, selection)),
        }
    }

    /// Like serde_json's insertion-ordered map, a repeated key keeps its first
    /// position and takes the last value.
    fn insert(&mut self, key: String, value: ProjectionValue) {
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
        if let Some(serializer) = self.property(key, true) {
            let value = value.serialize(serializer)?;
            self.insert(key.to_owned(), value);
        }
        Ok(())
    }
}

impl serde::ser::SerializeMap for ObjectBuilder<'_> {
    type Ok = ProjectionValue;
    type Error = SerializeError;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), SerializeError> {
        let (key, string_key) = map_key(key)?;
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
    Raw(Option<ProjectionValue>),
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
            Self::Raw(raw) if key == RAW_VALUE_TOKEN => {
                let KeyKind::Str(text) = value.serialize(KeyProbe)? else {
                    return Err(serialize_error("expected raw JSON text"));
                };
                *raw = Some(projection_value_from_json(serde_json::from_str(&text)?));
                Ok(())
            }
            Self::Raw(_) => Err(serialize_error("unexpected raw value field")),
        }
    }

    fn end(self) -> Result<ProjectionValue, SerializeError> {
        match self {
            Self::Object(object) => Ok(ProjectionValue::Object(object.entries)),
            Self::Raw(raw) => raw.ok_or_else(|| serialize_error("raw value was not emitted")),
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
            self.variant.to_owned(),
            ProjectionValue::Array(self.inner.values),
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
            self.variant.to_owned(),
            ProjectionValue::Object(self.inner.entries),
        )]))
    }
}

/// The JSON object key for a map key, and whether serde_value records it as a
/// string. Only string and unit-variant keys are; every other accepted key
/// type goes through serde_json's own key conversion.
fn map_key<T: Serialize + ?Sized>(key: &T) -> Result<(String, bool), SerializeError> {
    struct SingleKey<'k, K: ?Sized>(&'k K);

    impl<K: Serialize + ?Sized> Serialize for SingleKey<'_, K> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeMap as _;
            let mut map = serializer.serialize_map(Some(1))?;
            map.serialize_entry(self.0, &())?;
            map.end()
        }
    }

    match key.serialize(KeyProbe) {
        Ok(KeyKind::Str(key)) => Ok((key, true)),
        Ok(KeyKind::UnitVariant(variant)) => Ok((variant.to_owned(), true)),
        Ok(KeyKind::Other) | Err(_) => match serde_json::to_value(SingleKey(key))? {
            serde_json::Value::Object(map) => map
                .into_iter()
                .next()
                .map(|(key, _)| (key, false))
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
struct KeyProbe;

macro_rules! other_key {
    ($($method:ident($type:ty)),* $(,)?) => {
        $(fn $method(self, _: $type) -> Result<KeyKind, SerializeError> {
            Ok(KeyKind::Other)
        })*
    };
}

type NotAKey = serde::ser::Impossible<KeyKind, SerializeError>;

impl serde::Serializer for KeyProbe {
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

#[derive(Clone, Debug, PartialEq)]
pub(super) enum ProjectionValue {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(String),
    Utf16CodeUnit(u16),
    Array(Vec<Self>),
    Object(Vec<(String, Self)>),
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
                        (key, value)
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
                .map(|(key, value)| (key, projection_value_from_json(value)))
                .collect(),
        ),
    }
}

pub(super) fn encode_javascript_json(value: &ProjectionValue) -> Result<Bytes, ApiError> {
    let mut bytes = Vec::new();
    write_projection_json(&mut bytes, value)
        .map_err(|error| ApiError::serialization(error.to_string()))?;
    Ok(Bytes::from(bytes))
}

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
            if value.is_i64() {
                bytes
                    .extend_from_slice(value.as_i64().expect("checked i64").to_string().as_bytes());
            } else if value.is_u64() {
                bytes
                    .extend_from_slice(value.as_u64().expect("checked u64").to_string().as_bytes());
            } else {
                crate::calculation::write_js_json_number(
                    bytes,
                    value.as_f64().expect("JSON number"),
                )?;
            }
        }
        ProjectionValue::String(value) => serde_json::to_writer(bytes, value)?,
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
        ProjectionValue::Object(entries) => {
            bytes.push(b'{');

            for (index, (key, value)) in entries.iter().enumerate() {
                if index != 0 {
                    bytes.push(b',');
                }
                serde_json::to_writer(&mut *bytes, key)?;
                bytes.push(b':');
                write_projection_json(bytes, value)?;
            }
            bytes.push(b'}');
        }
    }
    Ok(())
}

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
                        projected.push((field.clone(), value));
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
                        projected.push((field.clone(), value));
                    }
                }
            }
        }
    }
    ProjectionValue::Object(projected)
}

pub(super) fn javascript_property(value: &ProjectionValue, key: &str) -> Option<ProjectionValue> {
    match value {
        ProjectionValue::Object(object) => object
            .iter()
            .find_map(|(candidate, value)| (candidate == key).then(|| value.clone())),
        ProjectionValue::Array(array) if key == "length" => Some(ProjectionValue::Number(
            serde_json::Number::from(array.len()),
        )),
        ProjectionValue::Array(array) => javascript_array_index(key)
            .and_then(|index| usize::try_from(index).ok())
            .and_then(|index| array.get(index))
            .cloned(),
        ProjectionValue::String(string) if key == "length" => Some(ProjectionValue::Number(
            serde_json::Number::from(string.encode_utf16().count()),
        )),
        ProjectionValue::String(string) => javascript_array_index(key)
            .and_then(|index| usize::try_from(index).ok())
            .and_then(|index| string.encode_utf16().nth(index))
            .map(|unit| {
                char::from_u32(u32::from(unit))
                    .map_or(ProjectionValue::Utf16CodeUnit(unit), |character| {
                        ProjectionValue::String(character.to_string())
                    })
            }),
        ProjectionValue::Utf16CodeUnit(_) if key == "length" => {
            Some(ProjectionValue::Number(serde_json::Number::from(1)))
        }
        ProjectionValue::Utf16CodeUnit(_) if javascript_array_index(key) == Some(0) => {
            Some(value.clone())
        }
        _ => None,
    }
}

pub(super) fn javascript_array_index(key: &str) -> Option<u32> {
    if key.is_empty() || (key.len() > 1 && key.starts_with('0')) {
        return None;
    }
    let index = key.parse::<u32>().ok()?;
    (index != u32::MAX && index.to_string() == key).then_some(index)
}

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

    fn document(&self) -> Result<Option<serde_json::Value>, ApiError> {
        match crate::api::v2::document(&self.snapshot) {
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
