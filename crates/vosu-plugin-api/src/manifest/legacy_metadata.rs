//! Read-only legacy `metadata.txt` interpretation, deliberately not a v2 manifest.
//! The host owns byte bounds, UTF-8 decoding, file confinement and settings.
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct LegacyMetadata {
    pub name: String,
    pub author: String,
    pub version: String,
    pub resolution: Vec<Value>,
    pub authorlinks: Vec<String>,
}

impl LegacyMetadata {
    pub fn parse(text: &str) -> Self {
        let mut fields = HashMap::new();
        for line in text.lines() {
            let Some((key, rest)) = line.split_once(':') else {
                continue;
            };
            let value = rest
                .split("##")
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned();
            fields.insert(key.to_lowercase(), value);
        }
        let resolution = fields
            .remove("resolution")
            .map(|value| {
                if dimensions(&value) {
                    value
                        .split(['x', 'X'])
                        .map(|part| {
                            part.trim()
                                .parse::<i64>()
                                .map(Value::from)
                                .unwrap_or(Value::Null)
                        })
                        .collect()
                } else {
                    vec![Value::from("Any"), Value::from("Any")]
                }
            })
            .unwrap_or_else(|| vec![Value::from("Any"), Value::from("Any")]);
        let authorlinks = fields
            .remove("authorlinks")
            .map(|value| value.split(',').map(str::to_owned).collect())
            .unwrap_or_default();
        Self {
            name: fields.remove("name").unwrap_or_default(),
            author: fields.remove("author").unwrap_or_default(),
            version: fields
                .remove("version")
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "1.0".into()),
            resolution,
            authorlinks,
        }
    }
}

fn dimensions(value: &str) -> bool {
    let Some((left, right)) = value.split_once(['x', 'X']) else {
        return false;
    };
    !left.trim().is_empty()
        && !right.trim().is_empty()
        && left.trim().chars().all(|ch| ch.is_ascii_digit())
        && right
            .trim()
            .chars()
            .all(|ch| ch.is_ascii_digit() || ch == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_legacy_metadata_preserves_historical_semantics() {
        let metadata = LegacyMetadata::parse(
            "NAME: first\r\nName: final ## comment\r\nVersion: \r\nResolution: 1920x--1\r\nAuthorlinks: a, b\r\n",
        );
        assert_eq!(metadata.name, "final");
        assert_eq!(metadata.version, "1.0");
        assert_eq!(metadata.resolution, vec![Value::from(1920), Value::Null]);
        assert_eq!(metadata.authorlinks, vec!["a", " b"]);
        assert_eq!(
            LegacyMetadata::parse("Resolution: invalid").resolution,
            vec![Value::from("Any"), Value::from("Any")]
        );
    }
}
