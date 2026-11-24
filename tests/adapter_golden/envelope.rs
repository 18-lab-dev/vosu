//! Strict reader for `vosu-adapter-capture-v1` envelopes
//! (`tests/fixtures/upstream/GOLDEN_CONTRACT.md` §5).
//!
//! An envelope is replay input, never an expected value. Selected entry
//! values stay as the recorded canonical CBOR bytes and are never converted
//! through JSON. The digest is checked again here, so a hand-edited selection
//! cannot be replayed unnoticed.
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use vosu::server::{
    adapter_capture::{FORMAT, schema_from_query, schema_name},
    websocket::Schema,
};
use vosu_plugin_api::CborValue;

/// The members the envelope digest covers, in envelope order.
const DIGEST_KEYS: [&str; 6] = [
    "session",
    "focus_revision",
    "roster",
    "bus_revision",
    "namespace_revisions",
    "entries",
];

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecordedSession {
    pub ordinal: usize,
    pub generation: u64,
    pub owner: String,
    pub owner_instance: usize,
    pub game: String,
    pub variant: String,
    pub foreground: Option<bool>,
}
impl RecordedSession {
    /// Every fact a replay must reproduce exactly. Generations are host
    /// counters and are compared by their order within the roster instead.
    pub fn facts(&self) -> (usize, &str, usize, &str, &str, Option<bool>) {
        (
            self.ordinal,
            &self.owner,
            self.owner_instance,
            &self.game,
            &self.variant,
            self.foreground,
        )
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecordedError {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedEntry {
    pub ns: String,
    pub pointer: String,
    /// Canonical CBOR bytes, exactly as recorded.
    pub value: Option<Vec<u8>>,
    pub error: Option<RecordedError>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedHttp {
    pub kind: String,
    pub status: u16,
    pub body: String,
}

#[derive(Clone, Debug)]
pub struct Envelope {
    pub schema: Schema,
    pub session: Option<RecordedSession>,
    pub roster: Vec<RecordedSession>,
    pub bus_revision: u64,
    pub namespace_revisions: Vec<(String, u64)>,
    pub entries: Vec<RecordedEntry>,
    /// `ready`, `pending` or `none`.
    pub derived: String,
    pub http: RecordedHttp,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    format: String,
    schema: String,
    #[expect(dead_code, reason = "Validated as present; provider-local counter")]
    provider_generation: u64,
    session: Option<RecordedSession>,
    #[expect(dead_code, reason = "Validated as present; host-local counter")]
    focus_revision: u64,
    roster: WireRoster,
    bus_revision: u64,
    namespace_revisions: Vec<(String, u64)>,
    entries: Vec<WireEntry>,
    derived: WireDerived,
    http: WireHttp,
    #[expect(dead_code, reason = "Checked against the recomputed digest")]
    digest: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRoster {
    #[expect(dead_code, reason = "Validated as present; host-local counter")]
    revision: u64,
    sessions: Vec<RecordedSession>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireEntry {
    ns: String,
    pointer: String,
    value_cbor_b64: Option<String>,
    error: Option<RecordedError>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireDerived {
    #[expect(dead_code, reason = "Validated as present; adapter-local counter")]
    revision: u64,
    status: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireHttp {
    kind: String,
    status: u16,
    body_utf8: String,
}

impl Envelope {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let value: Value =
            serde_json::from_slice(bytes).map_err(|e| format!("envelope JSON: {e}"))?;
        let recorded = value
            .get("digest")
            .and_then(Value::as_str)
            .ok_or("envelope has no digest")?;
        if selection_digest(&value)? != recorded {
            return Err("envelope digest does not match its selection".into());
        }
        let wire: Wire = serde_json::from_value(value).map_err(|e| format!("envelope: {e}"))?;
        if wire.format != FORMAT {
            return Err(format!("envelope format {:?} is not {FORMAT}", wire.format));
        }
        let schema = schema_from_query(&wire.schema)
            .ok_or_else(|| format!("unknown envelope schema {:?}", wire.schema))?;
        if !matches!(wire.derived.status.as_str(), "ready" | "pending" | "none") {
            return Err(format!("unknown derived status {:?}", wire.derived.status));
        }
        if !matches!(wire.http.kind.as_str(), "ready" | "not_ready" | "failed") {
            return Err(format!("unknown HTTP result kind {:?}", wire.http.kind));
        }
        let entries = wire
            .entries
            .into_iter()
            .map(|entry| {
                let value = entry
                    .value_cbor_b64
                    .map(|encoded| {
                        let bytes = STANDARD
                            .decode(encoded)
                            .map_err(|e| format!("{}{}: base64: {e}", entry.ns, entry.pointer))?;
                        vosu_plugin_api::decode::<CborValue>(&bytes)
                            .map_err(|e| format!("{}{}: CBOR: {e}", entry.ns, entry.pointer))?;
                        Ok::<_, String>(bytes)
                    })
                    .transpose()?;
                Ok(RecordedEntry {
                    ns: entry.ns,
                    pointer: entry.pointer,
                    value,
                    error: entry.error,
                })
            })
            .collect::<Result<_, String>>()?;
        Ok(Self {
            schema,
            session: wire.session,
            roster: wire.roster.sessions,
            bus_revision: wire.bus_revision,
            namespace_revisions: wire.namespace_revisions,
            entries,
            derived: wire.derived.status,
            http: RecordedHttp {
                kind: wire.http.kind,
                status: wire.http.status,
                body: wire.http.body_utf8,
            },
        })
    }

    pub fn name(&self) -> &'static str {
        schema_name(self.schema)
    }

    /// Checks that `replayed`, the dump of the replayed provider, selected
    /// what this envelope recorded. Revisions are host counters and are
    /// compared only by their relations. Reference values carry host-minted
    /// ids, so only their media type and URL family must match.
    pub fn check_replayed(&self, replayed: &Envelope) -> Result<(), String> {
        let schema = self.name();
        if self.session.as_ref().map(RecordedSession::facts)
            != replayed.session.as_ref().map(RecordedSession::facts)
        {
            return Err(format!(
                "{schema}: selected session {:?}, recorded {:?}",
                replayed.session, self.session
            ));
        }
        if !self
            .roster
            .iter()
            .map(RecordedSession::facts)
            .eq(replayed.roster.iter().map(RecordedSession::facts))
        {
            return Err(format!(
                "{schema}: roster {:?}, recorded {:?}",
                replayed.roster, self.roster
            ));
        }
        if !increasing(&replayed.roster) {
            return Err(format!(
                "{schema}: replayed generations are out of roster order"
            ));
        }
        if (self.bus_revision == 0) != (replayed.bus_revision == 0) {
            return Err(format!(
                "{schema}: bus revision {}, recorded {}",
                replayed.bus_revision, self.bus_revision
            ));
        }
        let namespaces = |revisions: &[(String, u64)]| -> Vec<(String, bool)> {
            revisions
                .iter()
                .map(|(ns, revision)| (ns.clone(), *revision == 0))
                .collect()
        };
        if namespaces(&self.namespace_revisions) != namespaces(&replayed.namespace_revisions) {
            return Err(format!(
                "{schema}: namespace revisions {:?}, recorded {:?}",
                replayed.namespace_revisions, self.namespace_revisions
            ));
        }
        if self.entries.len() != replayed.entries.len() {
            return Err(format!(
                "{schema}: {} selected entries, recorded {}",
                replayed.entries.len(),
                self.entries.len()
            ));
        }
        for (recorded, entry) in self.entries.iter().zip(&replayed.entries) {
            let at = format!("{schema} {}{}", recorded.ns, recorded.pointer);
            if (&recorded.ns, &recorded.pointer) != (&entry.ns, &entry.pointer) {
                return Err(format!("{at}: replayed {}{}", entry.ns, entry.pointer));
            }
            if recorded.error != entry.error {
                return Err(format!(
                    "{at}: error {:?}, recorded {:?}",
                    entry.error, recorded.error
                ));
            }
            let same = match (&recorded.value, &entry.value) {
                (None, None) => true,
                (Some(left), Some(right)) if left == right => true,
                (Some(left), Some(right))
                    if reference_position(&recorded.ns, &recorded.pointer) =>
                {
                    same_reference(left, right)
                }
                _ => false,
            };
            if !same {
                return Err(format!("{at}: replayed value differs from the recording"));
            }
        }
        Ok(())
    }
}

/// `sha256:` plus the lowercase hex digest of the compact selection object,
/// exactly as the capture endpoint computes it.
pub fn selection_digest(envelope: &Value) -> Result<String, String> {
    let mut selection = Map::new();
    for key in DIGEST_KEYS {
        let member = envelope
            .get(key)
            .ok_or_else(|| format!("envelope has no {key}"))?;
        selection.insert(key.into(), member.clone());
    }
    let bytes = serde_json::to_vec(&Value::Object(selection)).map_err(|e| e.to_string())?;
    let mut digest = String::from("sha256:");
    for byte in Sha256::digest(bytes) {
        digest.push_str(&format!("{byte:02x}"));
    }
    Ok(digest)
}

/// Generations strictly increase in roster order.
pub fn increasing(roster: &[RecordedSession]) -> bool {
    roster
        .windows(2)
        .all(|pair| pair[0].generation < pair[1].generation)
}

/// Decoded JSON Pointer tokens; `None` for a malformed pointer.
pub fn tokens(pointer: &str) -> Option<Vec<String>> {
    if pointer.is_empty() {
        return Some(Vec::new());
    }
    pointer
        .strip_prefix('/')?
        .split('/')
        .map(|token| {
            let mut out = String::with_capacity(token.len());
            let mut chars = token.chars();
            while let Some(c) = chars.next() {
                if c != '~' {
                    out.push(c);
                    continue;
                }
                match chars.next() {
                    Some('0') => out.push('~'),
                    Some('1') => out.push('/'),
                    _ => return None,
                }
            }
            Some(out)
        })
        .collect()
}

/// A protected leaf that only a published resource or tree reference fills:
/// `vosu.chart` `/resources/<kind>` and `vosu.skin` `/tree`.
pub fn reference_position(ns: &str, pointer: &str) -> bool {
    let Some(tokens) = tokens(pointer) else {
        return false;
    };
    (ns == "vosu.chart" && tokens.len() == 2 && tokens[0] == "resources")
        || (ns == "vosu.skin" && tokens == ["tree"])
}

const REFERENCE_PREFIXES: [&str; 2] = ["/api/v1/resources/", "/api/v1/trees/"];

/// Whether `value` holds a host reference projection (`url` naming a host
/// resource or tree), which only a published reference can reproduce.
pub fn contains_reference(value: &CborValue) -> bool {
    match value {
        CborValue::Map(fields) => fields.iter().any(|(key, value)| {
            (key.as_text() == Some("url")
                && value
                    .as_text()
                    .is_some_and(|url| REFERENCE_PREFIXES.iter().any(|p| url.starts_with(p))))
                || contains_reference(value)
        }),
        CborValue::Array(items) => items.iter().any(contains_reference),
        CborValue::Tag(_, inner) => contains_reference(inner),
        _ => false,
    }
}

/// Two reference projections that differ at most in their host-minted id.
fn same_reference(left: &[u8], right: &[u8]) -> bool {
    let fields = |bytes: &[u8]| -> Option<Vec<(String, String)>> {
        let CborValue::Map(fields) = vosu_plugin_api::decode::<CborValue>(bytes).ok()? else {
            return None;
        };
        fields
            .into_iter()
            .map(|(key, value)| {
                let key = key.into_text().ok()?;
                let value = value.into_text().ok()?;
                let value = if key == "url" {
                    REFERENCE_PREFIXES
                        .iter()
                        .find(|prefix| value.starts_with(*prefix))?
                        .to_string()
                } else {
                    value
                };
                Some((key, value))
            })
            .collect()
    };
    fields(left).is_some_and(|left| fields(right) == Some(left))
}
