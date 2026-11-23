//! Source-compatible parser for the osu! client's own startup argv.
//!
//! Lives in `vosu-osu-reader` (not the root crate's `app` module) because
//! `instance::discovery` is the only consumer today; the root crate parses
//! `update`/`onedrive` with this type and builds `vosu_core::app::StartupFlags`
//! from them, so core itself never depends on this osu!-specific parser.

use std::ffi::OsString;

use indexmap::IndexMap;

/// A value produced by the source application's permissive argument grammar.
#[derive(Clone, Debug, PartialEq)]
pub enum StartupValue {
    Number(f64),
    Boolean(bool),
    String(OsString),
}

impl StartupValue {
    pub fn as_boolean(&self) -> Option<bool> {
        match self {
            Self::Boolean(value) => Some(*value),
            Self::Number(_) | Self::String(_) => None,
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(value) => Some(*value),
            Self::Boolean(_) | Self::String(_) => None,
        }
    }

    pub fn as_os_str(&self) -> Option<&std::ffi::OsStr> {
        match self {
            Self::String(value) => Some(value.as_os_str()),
            Self::Number(_) | Self::Boolean(_) => None,
        }
    }

    pub fn is_truthy(&self) -> bool {
        match self {
            Self::Number(value) => *value != 0.0 && !value.is_nan(),
            Self::Boolean(value) => *value,
            Self::String(value) => !value.is_empty(),
        }
    }
}

/// Parsed startup arguments. Unknown entries are deliberately retained so this
/// remains the common source-compatible parser, while startup decisions consult
/// only `update` and `onedrive`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StartupArgs {
    raw: Vec<OsString>,
    values: IndexMap<String, StartupValue>,
}

impl StartupArgs {
    /// Parses an argv array, excluding argv\[0\]. Each remaining array element is
    /// matched independently, like the TypeScript array overload.
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Self {
        let mut args = args.into_iter();
        let _ = args.next();
        Self::from_array(args)
    }

    /// Parses argument elements without treating the first element as argv\[0\].
    pub fn from_array(args: impl IntoIterator<Item = OsString>) -> Self {
        let raw = args.into_iter().collect::<Vec<_>>();
        let mut values = IndexMap::new();
        for argument in &raw {
            // Process argv may contain arbitrary Unix bytes. Preserve it in `raw` and
            // never manufacture flag names or values through replacement characters.
            let Some(argument) = argument.to_str() else {
                continue;
            };
            if let Some((name, value)) = first_match(argument) {
                values.insert(name, coerce(value));
            }
        }
        Self { raw, values }
    }

    /// Parses the string overload globally, accepting `-`/`--` and `=`/one
    /// literal space exactly like the source regular expression.
    pub fn parse_command_line(command_line: &str) -> Self {
        let mut values = IndexMap::new();
        let mut cursor = 0;
        while let Some((start, end, name, value)) = next_match(command_line, cursor) {
            values.insert(name, coerce(value));
            cursor = end.max(start + 1);
        }
        Self {
            raw: vec![OsString::from(command_line)],
            values,
        }
    }

    pub fn raw(&self) -> &[OsString] {
        &self.raw
    }

    pub fn values(&self) -> &IndexMap<String, StartupValue> {
        &self.values
    }

    pub fn get(&self, name: &str) -> Option<&StartupValue> {
        self.values.get(name)
    }
}

fn first_match(input: &str) -> Option<(String, &str)> {
    next_match(input, 0).map(|(_, _, name, value)| (name, value))
}

fn next_match(input: &str, from: usize) -> Option<(usize, usize, String, &str)> {
    let bytes = input.as_bytes();
    let mut start = from;
    while start < bytes.len() {
        if bytes[start] != b'-' {
            start += 1;
            continue;
        }

        let mut name_start = start + 1;
        if bytes.get(name_start) == Some(&b'-') {
            name_start += 1;
        }
        if name_start >= bytes.len()
            || bytes[name_start] == b'='
            || bytes[name_start].is_ascii_whitespace()
        {
            start += 1;
            continue;
        }

        let mut name_end = name_start;
        while name_end < bytes.len()
            && bytes[name_end] != b'='
            && !bytes[name_end].is_ascii_whitespace()
        {
            name_end += 1;
        }
        let name = input[name_start..name_end].to_owned();
        let mut end = name_end;
        let mut value = "";
        if matches!(bytes.get(name_end), Some(b'=') | Some(b' ')) {
            let value_start = name_end + 1;
            if value_start < bytes.len()
                && bytes[value_start] != b'-'
                && !bytes[value_start].is_ascii_whitespace()
            {
                end = value_start;
                while end < bytes.len() && !bytes[end].is_ascii_whitespace() {
                    end += 1;
                }
                value = &input[value_start..end];
            } else {
                end = value_start.min(bytes.len());
            }
        }
        return Some((start, end, name, value));
    }
    None
}

fn coerce(value: &str) -> StartupValue {
    if let Some(number) = parse_float_prefix(value) {
        StartupValue::Number(number)
    } else if value == "true" {
        StartupValue::Boolean(true)
    } else if value == "false" {
        StartupValue::Boolean(false)
    } else {
        StartupValue::String(OsString::from(value))
    }
}

/// JavaScript `parseFloat`'s accepted numeric prefix for the argument inputs.
fn parse_float_prefix(value: &str) -> Option<f64> {
    let bytes = value.as_bytes();
    let mut cursor = 0;
    if matches!(bytes.first(), Some(b'+') | Some(b'-')) {
        cursor += 1;
    }
    if value.get(cursor..)?.starts_with("Infinity") {
        return Some(if bytes.first() == Some(&b'-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        });
    }

    let integer_start = cursor;
    while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
        cursor += 1;
    }
    let mut has_digit = cursor > integer_start;
    if bytes.get(cursor) == Some(&b'.') {
        cursor += 1;
        let fraction_start = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        has_digit |= cursor > fraction_start;
    }
    if !has_digit {
        return None;
    }

    let exponent = cursor;
    if matches!(bytes.get(cursor), Some(b'e') | Some(b'E')) {
        cursor += 1;
        if matches!(bytes.get(cursor), Some(b'+') | Some(b'-')) {
            cursor += 1;
        }
        let digits = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        if cursor == digits {
            cursor = exponent;
        }
    }
    value[..cursor].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> StartupArgs {
        StartupArgs::from_array(items.iter().map(OsString::from))
    }

    #[test]
    fn parses_array_and_command_line_source_grammar() {
        let parsed = args(&[
            "ignored prefix --rate=12.5ms",
            "-enabled true",
            "--missing",
            "--next -other=false",
            "--unknown=value",
        ]);
        assert_eq!(parsed.get("rate"), Some(&StartupValue::Number(12.5)));
        assert_eq!(parsed.get("enabled"), Some(&StartupValue::Boolean(true)));
        assert_eq!(
            parsed.get("missing"),
            Some(&StartupValue::String(OsString::new()))
        );
        assert_eq!(
            parsed.get("next"),
            Some(&StartupValue::String(OsString::new()))
        );
        assert_eq!(
            parsed.get("unknown"),
            Some(&StartupValue::String("value".into()))
        );
        assert_eq!(
            parsed.get("other"),
            None,
            "array overload takes one match per element"
        );

        let parsed = StartupArgs::parse_command_line(
            "vosu --rate=+.5e2tail -enabled false --missing --other=3e --truth=TRUE",
        );
        assert_eq!(parsed.get("rate"), Some(&StartupValue::Number(50.0)));
        assert_eq!(parsed.get("enabled"), Some(&StartupValue::Boolean(false)));
        assert_eq!(
            parsed.get("missing"),
            Some(&StartupValue::String(OsString::new()))
        );
        assert_eq!(parsed.get("other"), Some(&StartupValue::Number(3.0)));
        assert_eq!(
            parsed.get("truth"),
            Some(&StartupValue::String("TRUE".into()))
        );
    }

    #[test]
    fn process_argv_skips_executable_and_retains_unknown_map_entries() {
        let parsed = StartupArgs::parse([
            OsString::from("--not-a-flag=1"),
            OsString::from("--update=false"),
            OsString::from("--future=42suffix"),
        ]);
        assert_eq!(parsed.get("not-a-flag"), None);
        assert_eq!(parsed.get("update"), Some(&StartupValue::Boolean(false)));
        assert_eq!(parsed.get("future"), Some(&StartupValue::Number(42.0)));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_process_argv_is_retained_without_lossy_flag_substitution() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let invalid = OsString::from_vec(b"--devserver=\xff".to_vec());
        let parsed = StartupArgs::from_array([invalid.clone()]);
        assert_eq!(parsed.raw()[0].as_bytes(), invalid.as_bytes());
        assert_eq!(parsed.get("devserver"), None);
    }
}
