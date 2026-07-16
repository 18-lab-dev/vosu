use std::collections::HashMap;

pub(super) fn env_binding_order(content: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for original_line in content.lines() {
        let mut line = original_line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("export ") {
            line = rest.trim_start();
        }
        let Some((key, _)) = split_env_binding(line) else {
            continue;
        };
        let key = key.trim();
        if !key.is_empty()
            && key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
            && !keys.iter().any(|existing| existing == key)
        {
            keys.push(key.to_owned());
        }
    }
    keys
}
pub(super) fn parse_real_number(raw: &str) -> Option<f64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    let prefixed_integer = [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ]
    .into_iter()
    .find_map(|(prefix, radix)| {
        raw.strip_prefix(prefix)
            .and_then(|digits| u64::from_str_radix(digits, radix).ok())
    });

    prefixed_integer
        .map(|value| value as f64)
        .or_else(|| raw.parse::<f64>().ok())
        .filter(|value| value.is_finite())
}

pub(super) fn format_number(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }

    value.to_string()
}

/// Parses the dotenv forms used by the source config. Later duplicate bindings
/// replace earlier ones, matching `dotenv.parse`.
pub(super) fn parse_env(content: &str) -> HashMap<String, String> {
    let mut parsed = HashMap::new();

    for original_line in content.lines() {
        let mut line = original_line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("export ") {
            line = rest.trim_start();
        }

        let Some((key, value)) = split_env_binding(line) else {
            continue;
        };
        let key = key.trim();
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
        {
            continue;
        }

        parsed.insert(key.to_owned(), parse_env_value(value));
    }

    parsed
}

fn split_env_binding(line: &str) -> Option<(&str, &str)> {
    if let Some(binding) = line.split_once('=') {
        return Some(binding);
    }

    let (key, value) = line.split_once(':')?;
    value
        .starts_with(char::is_whitespace)
        .then_some((key, value.trim_start()))
}

fn parse_env_value(raw: &str) -> String {
    let value = raw.trim();
    let Some(quote) = value
        .as_bytes()
        .first()
        .copied()
        .filter(|quote| matches!(quote, b'\'' | b'"' | b'`'))
    else {
        return value
            .split_once('#')
            .map_or(value, |(before_comment, _)| before_comment)
            .trim_end()
            .to_owned();
    };

    let mut escaped = false;
    let closing_quote = value.as_bytes()[1..]
        .iter()
        .position(|byte| {
            if escaped {
                escaped = false;
                return false;
            }
            if *byte == b'\\' {
                escaped = true;
                return false;
            }
            *byte == quote
        })
        .map(|position| position + 1);

    let Some(closing_quote) = closing_quote else {
        return value.to_owned();
    };
    let inner = &value[1..closing_quote];
    if quote == b'"' {
        inner.replace("\\n", "\n").replace("\\r", "\r")
    } else {
        inner.to_owned()
    }
}
