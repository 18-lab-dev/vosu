pub(super) fn metadata_value<'a>(source: &'a str, key: &str) -> Option<&'a str> {
    section_value(source, "Metadata", key)
}
pub(super) fn general_value<'a>(source: &'a str, key: &str) -> Option<&'a str> {
    let value = section_value(source, "General", key)?;
    Some(js_trim(
        value.split_once("//").map_or(value, |(before, _)| before),
    ))
}

fn source_section_bit(section: &str) -> u8 {
    match section {
        "General" => 1 << 0,
        "Metadata" => 1 << 1,
        "Events" => 1 << 2,
        "TimingPoints" => 1 << 3,
        "HitObjects" => 1 << 4,
        _ => 0,
    }
}

pub(super) struct SourceLineScanner<'a> {
    lines: std::str::Lines<'a>,
    current_section: Option<&'a str>,
    current_enabled: bool,
    consumed_sections: u8,
}

impl<'a> SourceLineScanner<'a> {
    pub(super) fn new(source: &'a str) -> Self {
        Self {
            lines: source.lines(),
            current_section: None,
            current_enabled: false,
            consumed_sections: 0,
        }
    }
}

impl<'a> Iterator for SourceLineScanner<'a> {
    type Item = (&'a str, &'a str);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let raw_line = self.lines.next()?;
            if raw_line.is_empty() || raw_line.starts_with("//") {
                continue;
            }
            let mut line = raw_line;
            if self.current_section != Some("Metadata")
                && let Some(comment) = line.find("//").filter(|index| *index > 0)
            {
                line = &line[..comment];
            }
            line = line.trim_end_matches(is_ecmascript_whitespace);
            if let Some(name) = line
                .strip_prefix('[')
                .and_then(|line| line.strip_suffix(']'))
            {
                if self.current_enabled {
                    self.consumed_sections |= self.current_section.map_or(0, source_section_bit);
                }
                let bit = source_section_bit(name);
                self.current_section = Some(name);
                self.current_enabled = bit != 0 && self.consumed_sections & bit == 0;
                continue;
            }
            return Some((
                self.current_enabled
                    .then_some(self.current_section)
                    .flatten()
                    .unwrap_or_default(),
                line,
            ));
        }
    }
}

pub(super) fn section_value<'a>(source: &'a str, section: &str, key: &str) -> Option<&'a str> {
    let mut result = None;
    for (current_section, line) in SourceLineScanner::new(source) {
        if current_section != section {
            continue;
        }
        if let Some((name, value)) = line.split_once(':')
            && js_trim(name) == key
        {
            result = Some(js_trim(value));
        }
    }
    result
}

pub(super) fn section_integer_value(source: &str, section: &str, key: &str) -> Option<i64> {
    let mut result = None;
    for (current_section, line) in SourceLineScanner::new(source) {
        if current_section != section {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if js_trim(name) != key {
            continue;
        }
        if let Some(value) = parse_integer_prefix(js_trim(value), MAX_PARSE_VALUE) {
            result = Some(value);
        }
    }
    result
}

pub(super) fn background_from_events(source: &str) -> String {
    let mut background = String::new();
    for (section, line) in SourceLineScanner::new(source) {
        if section != "Events" || line.is_empty() {
            continue;
        }
        let mut fields = line.split(',');
        let event_type = fields.next();
        let _start_time = fields.next().map(js_trim);
        let path = fields.next().map(js_trim);
        if event_type.is_some_and(event_is_background)
            && let Some(path) = path
        {
            background = path.replace('"', "");
        }
    }
    background
}

pub(super) fn event_is_background(value: &str) -> bool {
    if matches!(value.as_bytes().first(), Some(b' ' | b'_')) {
        return false;
    }
    let value = js_trim(value);
    value == "Background" || parse_integer_prefix(value, MAX_PARSE_VALUE) == Some(0)
}
pub(super) const MAX_PARSE_VALUE: f64 = 2_147_483_647.0;
pub(super) const MAX_COORDINATE_VALUE: f64 = 131_072.0;

pub(super) fn js_round_i32(value: f64) -> i32 {
    (value + 0.5).floor() as i32
}

fn is_ecmascript_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}
pub(super) fn js_trim(value: &str) -> &str {
    value.trim_matches(is_ecmascript_whitespace)
}

pub(super) fn parse_float_prefix(value: &str, limit: f64) -> Option<f64> {
    let value = value.trim_start_matches(is_ecmascript_whitespace);
    let bytes = value.as_bytes();
    let mut index = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let integer_start = index;
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        index += 1;
    }
    let mut digits = index - integer_start;
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        let fraction_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        digits += index - fraction_start;
    }
    if digits == 0 {
        return None;
    }
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        let exponent = index;
        index += 1;
        if matches!(bytes.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        let exponent_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == exponent_start {
            index = exponent;
        }
    }
    let parsed = value[..index].parse::<f64>().ok()?;
    (parsed.is_finite() && parsed.abs() <= limit).then_some(parsed)
}
pub(super) fn parse_float_allow_nan(value: &str, limit: f64) -> Result<Option<f64>, ()> {
    if let Some(value) = parse_float_prefix(value, limit) {
        return Ok(Some(value));
    }
    let value = value
        .trim_start_matches(is_ecmascript_whitespace)
        .trim_start_matches(['+', '-']);
    let numeric_prefix = value.as_bytes().first().is_some_and(u8::is_ascii_digit)
        || (value.starts_with('.') && value.as_bytes().get(1).is_some_and(u8::is_ascii_digit))
        || value.starts_with("Infinity");
    if numeric_prefix { Err(()) } else { Ok(None) }
}

pub(super) fn parse_integer_prefix(value: &str, limit: f64) -> Option<i64> {
    let value = value.trim_start_matches(is_ecmascript_whitespace);
    let bytes = value.as_bytes();
    let negative = bytes.first() == Some(&b'-');
    let mut index = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let radix =
        if bytes.get(index) == Some(&b'0') && matches!(bytes.get(index + 1), Some(b'x' | b'X')) {
            index += 2;
            16
        } else {
            10
        };
    let digits_start = index;
    while bytes.get(index).is_some_and(|byte| match radix {
        16 => byte.is_ascii_hexdigit(),
        _ => byte.is_ascii_digit(),
    }) {
        index += 1;
    }
    if index == digits_start {
        return None;
    }
    let magnitude = i64::from_str_radix(&value[digits_start..index], radix).ok()?;
    let parsed = if negative { -magnitude } else { magnitude };
    ((parsed as f64).abs() <= limit).then_some(parsed)
}
