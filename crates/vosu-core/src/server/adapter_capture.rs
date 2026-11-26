//! Test-harness-only coherent bus dump (`vosu-adapter-capture-v1`): the exact
//! selection a [`super::bus_provider::BusStateProvider`] decoded for one
//! snapshot, plus the production HTTP result of that same snapshot. Golden
//! capture records it as replay input only, never as an expected value.
//!
//! Selected `StateEntry` values travel as the canonical bytes of the plugin
//! ABI CBOR codec, never through JSON numbers, so integer/float, signed zero,
//! null/absence and map order survive exactly. Host-issued session ids and
//! owner instance numbers are private run identity: they become
//! first-appearance ordinals (roster order, then the selected session), so
//! equal identities share an ordinal and distinct identities never do.
pub mod endpoint;

use crate::server::{routes::CachedJson, websocket::Schema};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{collections::HashMap, hash::Hash};
use vosu_plugin_api::ErrorInfo;
use vosu_surface::state::{CapturedSelection, IdentifiedSession};

pub const FORMAT: &str = "vosu-adapter-capture-v1";

/// The exact `schema` query values and the provider schema each selects.
pub const SCHEMAS: [(&str, Schema); 4] = [
    ("v1", Schema::V1),
    ("sc", Schema::StreamCompanion),
    ("v2", Schema::V2),
    ("v2-precise", Schema::V2Precise),
];

/// Exact match only: no case folding, trimming or prefix matching.
pub fn schema_from_query(value: &str) -> Option<Schema> {
    SCHEMAS
        .iter()
        .find_map(|(name, schema)| (*name == value).then_some(*schema))
}

pub fn schema_name(schema: Schema) -> &'static str {
    SCHEMAS
        .iter()
        .find_map(|(name, known)| (*known == schema).then_some(*name))
        .expect("every schema has a query name")
}

/// Whether the snapshot's derived inputs (the streamCompanion chart) were
/// complete for its selection when it was taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DerivedStatus {
    Ready,
    Pending,
    /// The adapter has no derived inputs.
    None,
}

/// One provider snapshot: the capture its adapter decoded and the HTTP
/// result rendered from that decoded view. Never a second capture.
#[derive(Clone, Debug)]
pub struct AdapterCaptureSample {
    pub schema: Schema,
    pub provider_generation: u64,
    pub capture: CapturedSelection,
    pub derived_revision: u64,
    pub derived: DerivedStatus,
    pub http: CachedJson,
}

impl AdapterCaptureSample {
    /// The canonical envelope JSON. `digest` is `sha256:` plus the lowercase
    /// hex SHA-256 of the compact JSON object holding exactly `session`,
    /// `focus_revision`, `roster`, `bus_revision`, `namespace_revisions` and
    /// `entries` in that order, as they appear in the envelope.
    pub fn envelope(&self) -> Result<Vec<u8>, String> {
        let selection = Selection::new(&self.capture)?;
        let digest = digest(&serde_json::to_vec(&selection).map_err(|e| e.to_string())?);
        let kind = match self.http {
            CachedJson::Ready(_) => "ready",
            CachedJson::NotReady => "not_ready",
            CachedJson::Failed { .. } => "failed",
        };
        let (status, body) = self.http.clone().status_body();
        let body_utf8 = std::str::from_utf8(&body)
            .map_err(|_| "adapter response body is not UTF-8".to_owned())?;
        serde_json::to_vec(&Envelope {
            format: FORMAT,
            schema: schema_name(self.schema),
            provider_generation: self.provider_generation,
            selection,
            derived: Derived {
                revision: self.derived_revision,
                status: match self.derived {
                    DerivedStatus::Ready => "ready",
                    DerivedStatus::Pending => "pending",
                    DerivedStatus::None => "none",
                },
            },
            http: Http {
                kind,
                status: status.as_u16(),
                body_utf8,
            },
            digest,
        })
        .map_err(|e| e.to_string())
    }
}

#[derive(Serialize)]
struct Envelope<'a> {
    format: &'static str,
    schema: &'static str,
    provider_generation: u64,
    #[serde(flatten)]
    selection: Selection<'a>,
    derived: Derived,
    http: Http<'a>,
    digest: String,
}
#[derive(Serialize)]
struct Selection<'a> {
    session: Option<Session<'a>>,
    focus_revision: u64,
    roster: Roster<'a>,
    bus_revision: u64,
    namespace_revisions: &'a [(String, u64)],
    entries: Vec<Entry<'a>>,
}
#[derive(Serialize)]
struct Session<'a> {
    ordinal: usize,
    generation: u64,
    owner: &'a str,
    owner_instance: usize,
    game: &'a str,
    variant: &'a str,
    foreground: Option<bool>,
}
#[derive(Serialize)]
struct Roster<'a> {
    revision: u64,
    sessions: Vec<Session<'a>>,
}
#[derive(Serialize)]
struct Entry<'a> {
    ns: &'a str,
    pointer: &'a str,
    value_cbor_b64: Option<String>,
    error: Option<&'a ErrorInfo>,
}
#[derive(Serialize)]
struct Derived {
    revision: u64,
    status: &'static str,
}
#[derive(Serialize)]
struct Http<'a> {
    kind: &'static str,
    status: u16,
    body_utf8: &'a str,
}

#[derive(Default)]
struct Ordinals<'a> {
    sessions: HashMap<&'a str, usize>,
    instances: HashMap<u64, usize>,
}
impl<'a> Ordinals<'a> {
    fn session(&mut self, session: &'a IdentifiedSession) -> Session<'a> {
        Session {
            ordinal: ordinal(&mut self.sessions, session.session.as_str()),
            generation: session.generation,
            owner: &session.owner,
            owner_instance: ordinal(&mut self.instances, session.owner_instance),
            game: &session.game,
            variant: &session.variant,
            foreground: session.foreground,
        }
    }
}
fn ordinal<K: Eq + Hash>(ordinals: &mut HashMap<K, usize>, key: K) -> usize {
    let next = ordinals.len();
    *ordinals.entry(key).or_insert(next)
}

impl<'a> Selection<'a> {
    fn new(capture: &'a CapturedSelection) -> Result<Self, String> {
        let mut ordinals = Ordinals::default();
        let sessions = capture
            .roster
            .sessions
            .iter()
            .map(|session| ordinals.session(session))
            .collect();
        let session = capture
            .session
            .as_ref()
            .map(|session| ordinals.session(session));
        let entries = capture
            .values
            .iter()
            .map(|entry| {
                Ok(Entry {
                    ns: &entry.ns,
                    pointer: &entry.pointer,
                    value_cbor_b64: entry
                        .value
                        .as_ref()
                        .map(|value| vosu_plugin_api::encode(value).map(|b| STANDARD.encode(b)))
                        .transpose()
                        .map_err(|e| e.to_string())?,
                    error: entry.error.as_ref(),
                })
            })
            .collect::<Result<_, String>>()?;
        Ok(Self {
            session,
            focus_revision: capture.focus_revision,
            roster: Roster {
                revision: capture.roster.revision,
                sessions,
            },
            bus_revision: capture.revision,
            namespace_revisions: &capture.namespace_revisions,
            entries,
        })
    }
}

fn digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::from("sha256:");
    for byte in Sha256::digest(bytes).iter() {
        out.push(HEX[usize::from(byte >> 4)] as char);
        out.push(HEX[usize::from(byte & 15)] as char);
    }
    out
}

#[cfg(test)]
mod tests;
