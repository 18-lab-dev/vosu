use std::io::{self, Write};

use serde::Serialize;
use serde_json::ser::{Formatter, Serializer};

use super::ApiError;

/// Serializer formatter matching JavaScript JSON number spelling.
///
/// The numeric conversion itself remains owned by `calculation`; this is the
/// single adapter used by every API wire encoder.
pub(crate) struct JsFormatter;

impl Formatter for JsFormatter {
    fn write_f32<W>(&mut self, writer: &mut W, value: f32) -> io::Result<()>
    where
        W: ?Sized + Write,
    {
        crate::calculation::write_js_json_number(writer, f64::from(value))
    }

    fn write_f64<W>(&mut self, writer: &mut W, value: f64) -> io::Result<()>
    where
        W: ?Sized + Write,
    {
        crate::calculation::write_js_json_number(writer, value)
    }
}

pub(crate) fn encode<T: Serialize>(value: &T, capacity: usize) -> Result<Vec<u8>, ApiError> {
    let mut output = Vec::with_capacity(capacity);
    let mut serializer = Serializer::with_formatter(&mut output, JsFormatter);
    value
        .serialize(&mut serializer)
        .map_err(|error| ApiError::serialization(error.to_string()))?;
    Ok(output)
}
