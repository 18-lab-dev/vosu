//! JavaScript property lookup, object ordering and JSON spelling.
use super::*;

/// An object property name: borrowed for DTO fields and enum variants, which
/// are `'static`, and owned only for map keys and parsed JSON.
pub type ObjectKey = Cow<'static, str>;

#[derive(Clone, Debug, PartialEq)]
pub enum ProjectionValue {
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
pub fn projection_value_from_json(value: serde_json::Value) -> ProjectionValue {
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
/// Writes the projection, through `filters`, of the object whose JavaScript
/// properties `property` reads, into `scratch`, without materializing or
/// cloning the projected tree.
pub(super) fn encode_root<'v>(
    scratch: &mut Vec<u8>,
    filters: &[Filter],
    property: &dyn Fn(&str) -> Option<Cow<'v, ProjectionValue>>,
) -> Result<Bytes, serde_json::Error> {
    scratch.clear();
    write_projection(scratch, filters, property)
        .map_err(<serde_json::Error as serde::ser::Error>::custom)?;
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

pub(super) fn filter_field(filter: &Filter) -> &str {
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
pub fn write_projection_json(bytes: &mut Vec<u8>, value: &ProjectionValue) -> io::Result<()> {
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
                    vosu_osu_model::write_js_json_number(bytes, value)?;
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
/// The value JavaScript reads for `value[key]`, borrowed where it exists in
/// `value` and synthesized for `length` and string code units.
pub fn javascript_property_ref<'v>(
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
pub fn javascript_array_index(key: &str) -> Option<u32> {
    if key.is_empty()
        || (key.len() > 1 && key.starts_with('0'))
        || !key.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let index = key.parse::<u32>().ok()?;
    (index != u32::MAX).then_some(index)
}

pub fn encode_javascript_json(value: &ProjectionValue) -> Result<Bytes, serde_json::Error> {
    let mut bytes = Vec::new();
    write_projection_json(&mut bytes, value)
        .map_err(<serde_json::Error as serde::ser::Error>::custom)?;
    Ok(Bytes::from(bytes))
}

/// Writes the projection of `value` through `filters`: the bytes
/// `encode_javascript_json(&project_filters(value, filters))` produces.
pub fn encode_projection(
    value: &ProjectionValue,
    filters: &[Filter],
) -> Result<Bytes, serde_json::Error> {
    encode_root(&mut Vec::new(), filters, &|key| {
        javascript_property_ref(value, key)
    })
}
