//! Canonical D7 mirror records and shared selector demand.
use super::SchemaError;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneRecord {
    pub pointer: String,
    pub lane: String,
    pub activity: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DemandBits {
    pub regular_always: bool,
    pub precise_always: bool,
    pub regular_gameplay: bool,
    pub precise_gameplay: bool,
}
impl DemandBits {
    pub fn union(self, other: Self) -> Self {
        Self {
            regular_always: self.regular_always || other.regular_always,
            precise_always: self.precise_always || other.precise_always,
            regular_gameplay: self.regular_gameplay || other.regular_gameplay,
            precise_gameplay: self.precise_gameplay || other.precise_gameplay,
        }
    }
    pub fn effective(self, in_play: bool) -> (bool, bool) {
        (
            self.regular_always || (self.regular_gameplay && in_play),
            self.precise_always || (self.precise_gameplay && in_play),
        )
    }
}

pub(crate) fn tokens(pointer: &str) -> Result<Vec<String>, SchemaError> {
    if pointer.is_empty() {
        return Ok(Vec::new());
    }
    let Some(tail) = pointer.strip_prefix('/') else {
        return Err(SchemaError::InvalidPointer(pointer.into()));
    };
    tail.split('/')
        .map(|part| {
            let mut decoded = String::new();
            let mut chars = part.chars();
            while let Some(c) = chars.next() {
                if c == '~' {
                    decoded.push(match chars.next() {
                        Some('0') => '~',
                        Some('1') => '/',
                        _ => return Err(SchemaError::InvalidPointer(pointer.into())),
                    });
                } else {
                    decoded.push(c);
                }
            }
            Ok(decoded)
        })
        .collect()
}

pub(crate) fn pointer(path: &[String]) -> String {
    let mut result = String::new();
    for token in path {
        result.push('/');
        result.push_str(&token.replace('~', "~0").replace('/', "~1"));
    }
    result
}

pub(crate) fn valid_annotation(lane: &str, activity: &str) -> bool {
    matches!(lane, "regular" | "precise" | "both") && matches!(activity, "always" | "gameplay")
}

/// Reject malformed or noncanonical mirror tables; distinct contributions at
/// one pointer are intentional, not conflicting records.
pub fn validate_records(records: &[LaneRecord]) -> Result<(), SchemaError> {
    for row in records {
        tokens(&row.pointer)?;
        if !valid_annotation(&row.lane, &row.activity) {
            return Err(SchemaError::InvalidLane(row.pointer.clone()));
        }
    }
    if records.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(SchemaError::NoncanonicalRecords);
    }
    Ok(())
}

/// Union contributions from a validated compiler/mirror table. Wildcards are
/// metadata-only array/map segments, matching exactly one selector token.
pub fn demand(records: &[LaneRecord], selector: &[&str]) -> DemandBits {
    records
        .iter()
        .filter(|row| {
            let path = row.pointer.strip_prefix('/').map(|tail| tail.split('/'));
            path.is_none_or(|path| {
                path.zip(selector)
                    .all(|(p, s)| p == "*" || *s == "*" || decoded_eq(p, s))
            })
        })
        .fold(DemandBits::default(), |acc, row| {
            let regular = row.lane == "regular" || row.lane == "both";
            let precise = row.lane == "precise" || row.lane == "both";
            acc.union(DemandBits {
                regular_always: regular && row.activity == "always",
                precise_always: precise && row.activity == "always",
                regular_gameplay: regular && row.activity == "gameplay",
                precise_gameplay: precise && row.activity == "gameplay",
            })
        })
}

fn decoded_eq(encoded: &str, token: &str) -> bool {
    let mut chars = encoded.chars();
    let decoded = std::iter::from_fn(|| {
        chars.next().map(|c| {
            if c == '~' {
                match chars.next() {
                    Some('0') => '~',
                    Some('1') => '/',
                    _ => '\0',
                }
            } else {
                c
            }
        })
    });
    decoded.eq(token.chars())
}
