//! Replays fixture cases through R0's real host and listener and compares
//! each record with the upstream expectation (`GOLDEN_CONTRACT.md` §6, §7).
//!
//! Observation sends the recorded request bytes to the production listener
//! of a replay of the case's envelope. Its only substitution is a declared
//! conditional validator, taken from vosu's own answer to the same target
//! earlier in the case. Payload headers and framing are retained; validator
//! bindings are verified locally rather than compared with upstream policy.
//! Headers, bodies and frames use the payload equality in §7. A validated
//! divergence rule only replaces the expectation for its own record part.
use super::{
    assets::{AssetFile, ReplayAssets, ResourceAsset, RootAsset},
    divergence::{self, Part, Rule},
    envelope::{Envelope, reference_position, tokens, tree_position},
    fixture::{Asset, Body, Case, LAST_MODIFIED, Platform, compared},
    payload_clock::{Delta, Scope},
    replay::Replay,
    routes::{self, RawResponse},
    websocket::{self, Frame, Step},
};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use vosu_plugin_api::CborValue;

/// The declared modification time of every replay asset file:
/// 2026-01-01T00:00:00Z. `header-last-modified` holds vosu to it.
pub fn replay_mtime() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_767_225_600)
}

/// The replay assets for one envelope: every fixture asset in its declared
/// root (with the declared mtime) when a session is selected, and each
/// recorded `vosu.chart` resource reference backed by the asset the osu!
/// plugin would resolve for it (`recorded_asset`).
pub fn replay_assets(assets: &[Asset], envelope: &Envelope) -> Result<ReplayAssets, String> {
    let Some(session) = &envelope.session else {
        return Ok(ReplayAssets::default());
    };
    let mut roots: Vec<RootAsset> = Vec::new();
    for asset in assets {
        let file = AssetFile {
            path: asset.relative.clone(),
            bytes: asset.bytes.clone(),
            modified: Some(replay_mtime()),
        };
        match roots.iter_mut().find(|root| root.slot == asset.slot) {
            Some(root) => root.files.push(file),
            None => roots.push(RootAsset {
                slot: asset.slot.clone(),
                files: vec![file],
            }),
        }
    }
    let mut resources = Vec::new();
    for entry in &envelope.entries {
        let Some(bytes) = &entry.value else { continue };
        if entry.ns != "vosu.chart"
            || !reference_position(&entry.ns, &entry.pointer)
            || tree_position(&entry.ns, &entry.pointer)
        {
            continue;
        }
        let CborValue::Map(fields) =
            vosu_plugin_api::decode::<CborValue>(bytes).map_err(|e| e.to_string())?
        else {
            continue;
        };
        let media = fields
            .iter()
            .find(|(key, _)| key.as_text() == Some("type"))
            .and_then(|(_, value)| value.as_text())
            .ok_or(format!("{}: reference has no media type", entry.pointer))?;
        let kind = tokens(&entry.pointer).expect("reference pointer")[1].clone();
        let asset = recorded_asset(assets, envelope, session.variant == "lazer", &kind)
            .map_err(|e| format!("{}: {e}", entry.pointer))?;
        if asset.media_type != media {
            return Err(format!(
                "{}: recorded media type {media}, approved asset {} is {}",
                entry.pointer, asset.id, asset.media_type
            ));
        }
        resources.push(ResourceAsset {
            kind,
            name: asset
                .relative
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_owned(),
            slot: asset.slot.clone(),
            path: asset.relative.clone(),
        });
    }
    Ok(ReplayAssets { roots, resources })
}

/// The approved asset a recorded `kind` reference names, by the identity
/// the osu! plugin resolves it from: the recorded `vosu.osu`
/// `/internal/paths` `current` leaf, joined under `beatmapFolder` on stable,
/// in the variant's songs slot (`songs`, lazer `files`). This keeps two
/// approved assets of one media type apart. The recorded string only
/// selects among approved assets; it never names a file to read, so an
/// absolute or private recorded path matches nothing.
fn recorded_asset<'a>(
    assets: &'a [Asset],
    envelope: &Envelope,
    lazer: bool,
    kind: &str,
) -> Result<&'a Asset, String> {
    let leaf_key = match kind {
        "file" => "beatmapFile",
        "audio" => "audio",
        "background" => "background",
        _ => return Err(format!("{kind} is not a replayable resource kind")),
    };
    let paths = envelope
        .entries
        .iter()
        .find(|e| e.ns == "vosu.osu" && e.pointer == "/internal/paths")
        .and_then(|e| e.value.as_deref())
        .ok_or("the envelope records no /internal/paths to identify its asset")?;
    let paths: CborValue = vosu_plugin_api::decode(paths).map_err(|e| e.to_string())?;
    let text = |map: &CborValue, key: &str| -> Option<String> {
        map.as_map()?
            .iter()
            .find(|(k, _)| k.as_text() == Some(key))?
            .1
            .as_text()
            .filter(|text| !text.is_empty())
            .map(|text| text.replace('\\', "/"))
    };
    let current = paths
        .as_map()
        .and_then(|fields| fields.iter().find(|(k, _)| k.as_text() == Some("current")))
        .map(|(_, current)| current.clone())
        .ok_or("the recorded /internal/paths has no current selection")?;
    let leaf = text(&current, leaf_key).ok_or(format!("no recorded current {leaf_key}"))?;
    let relative = match text(&current, "beatmapFolder").filter(|_| !lazer) {
        Some(folder) => format!("{folder}/{leaf}"),
        None => leaf,
    };
    let slot = if lazer { "files" } else { "songs" };
    let matching: Vec<&Asset> = assets
        .iter()
        .filter(|a| a.slot == slot && a.relative == relative)
        .collect();
    match matching[..] {
        [asset] => Ok(asset),
        _ => Err(format!(
            "no approved {slot} asset is at the recorded current {leaf_key} location; \
             recorded paths are never file authority"
        )),
    }
}

/// One normalized vosu record.
#[derive(Clone, Debug)]
pub struct Observed {
    pub status: u16,
    /// Compared payload headers plus `content-length` for local framing only.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    raw_body_len: usize,
    /// Every frame in sequence order for an upgraded `ws` case.
    pub frames: Vec<Frame>,
    /// `header-etag` ordinals the request substitution bound.
    pub bound: Vec<(u64, String)>,
    /// Failed local invariants, as (part, detail).
    pub invariants: Vec<(String, String)>,
    /// vosu closed the connection without a response byte.
    pub closed: bool,
}

/// Replays every case of `platform`, one replay per distinct envelope.
pub fn observe(platform: &Platform) -> Result<Vec<Result<Observed, String>>, String> {
    observe_with_rules(platform, &[])
}

pub(super) fn observe_with_rules(
    platform: &Platform,
    rules: &[Rule],
) -> Result<Vec<Result<Observed, String>>, String> {
    let mut out: Vec<Option<Result<Observed, String>>> = vec![None; platform.cases.len()];
    let mut groups: Vec<(&[u8], Vec<usize>)> = Vec::new();
    for (index, case) in platform.cases.iter().enumerate() {
        match groups
            .iter_mut()
            .find(|(json, _)| *json == case.replay_json.as_slice())
        {
            Some((_, members)) => members.push(index),
            None => groups.push((&case.replay_json, vec![index])),
        }
    }
    for (_, members) in groups {
        let envelope = &platform.cases[members[0]].replay;
        let assets = replay_assets(&platform.assets, envelope)?;
        let replay = Replay::start(std::slice::from_ref(envelope), &assets).map_err(|e| {
            format!(
                "{}: replay of {}: {e}",
                platform.name, platform.cases[members[0]].id
            )
        })?;
        let roots: Vec<(String, String)> = replay
            .roots()
            .iter()
            .map(|(slot, path)| {
                let temp = format!("{}{}", path.display(), std::path::MAIN_SEPARATOR);
                (temp, platform.synthetic_roots[slot].clone())
            })
            .collect();
        for index in members {
            let record = &platform.cases[index];
            out[index] = Some(case(
                &replay,
                record,
                platform,
                &roots,
                sequence_rule(rules, platform, record),
            ));
        }
    }
    Ok(out
        .into_iter()
        .map(|o| o.expect("every case observed"))
        .collect())
}

fn case(
    replay: &Replay,
    case: &Case,
    platform: &Platform,
    roots: &[(String, String)],
    sequence: Option<&Rule>,
) -> Result<Observed, String> {
    let mut invariants = Vec::new();
    let request = if case.id == "calculation.absolute-inside-root-osu" {
        std::borrow::Cow::Owned(calculation_request(replay, case, platform)?)
    } else {
        std::borrow::Cow::Borrowed(case.request.as_str())
    };
    let (request, bound) = substitute(replay.address, &request, &mut invariants)?;
    if case.expected.closed {
        // Upstream closed without a response: vosu must close too, and a
        // response it sends instead is observed in full for the report.
        return Ok(
            match routes::exchange_or_close(replay.address, request.as_bytes())? {
                Some(response) => normalize(response, Vec::new(), roots, bound, invariants),
                None => Observed {
                    status: 0,
                    headers: Vec::new(),
                    body: Vec::new(),
                    raw_body_len: 0,
                    frames: Vec::new(),
                    bound,
                    invariants,
                    closed: true,
                },
            },
        );
    }
    let (response, frames) = if let Some(expected) = expected_frames(case, sequence) {
        let steps: Vec<Step> = expected
            .iter()
            .map(|frame| match frame.from_server {
                true => Step::Receive,
                false => Step::Send {
                    fin: frame.fin,
                    opcode: frame.opcode,
                    payload: frame.payload.clone(),
                },
            })
            .collect();
        let exchange = websocket::exchange(replay.address, request.as_bytes(), &steps)?;
        (exchange.handshake, exchange.frames)
    } else {
        (
            routes::exchange(replay.address, request.as_bytes())?,
            Vec::new(),
        )
    };
    Ok(normalize(response, frames, roots, bound, invariants))
}

fn sequence_rule<'a>(rules: &'a [Rule], platform: &Platform, case: &Case) -> Option<&'a Rule> {
    rules
        .iter()
        .find(|rule| rule.part == Part::WsSequence && rule.covers(&platform.name, &case.id))
}

fn expected_frames<'a>(
    case: &'a Case,
    sequence: Option<&'a Rule>,
) -> Option<&'a [super::fixture::ExpectedFrame]> {
    sequence
        .map(|rule| divergence::sequence(&rule.change).expect("validated sequence"))
        .or_else(|| case.expected.frames.as_deref().map(payload_frames))
}

/// Bind just the positive absolute calculator probe to an approved asset in
/// the materialized root. The synthetic prefix itself grants no file access.
fn calculation_request(
    replay: &Replay,
    case: &Case,
    platform: &Platform,
) -> Result<String, String> {
    let (query, tail) = case
        .request
        .strip_prefix("GET /api/calculate/pp?")
        .and_then(|rest| rest.split_once(" HTTP/1.1\r\n"))
        .filter(|(query, _)| query.starts_with("path=") && !query.contains('&'))
        .ok_or("absolute calculation probe has an unexpected request target")?;
    let decoded = url::form_urlencoded::parse(query.as_bytes())
        .next()
        .expect("checked path query")
        .1
        .replace('\\', "/");
    if !platform
        .synthetic_roots
        .values()
        .any(|root| decoded.starts_with(&root.replace('\\', "/")))
    {
        return Err("absolute calculation probe has no declared synthetic root".into());
    }
    let slot = if platform.name.ends_with("-lazer") {
        "files"
    } else {
        "songs"
    };
    let mut matching = platform.assets.iter().filter(|asset| {
        asset.slot == slot
            && asset.relative.ends_with(".osu")
            && decoded
                .strip_suffix(&asset.relative)
                .is_some_and(|prefix| prefix.ends_with('/'))
    });
    let asset = matching
        .next()
        .filter(|_| matching.next().is_none())
        .ok_or("absolute calculation probe does not identify one approved .osu asset")?;
    let (_, root) = replay
        .roots()
        .iter()
        .find(|(name, _)| name == slot)
        .ok_or("absolute calculation probe has no materialized root")?;
    let path = root.join(&asset.relative);
    let path = path
        .to_str()
        .ok_or("absolute calculation replay path is not UTF-8")?;
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("path", path)
        .finish();
    Ok(format!("GET /api/calculate/pp?{query} HTTP/1.1\r\n{tail}"))
}

/// Replaces declared validator placeholders with vosu's own validators for
/// the same target, read by a validator-free `GET` earlier in the case.
pub fn substitute(
    address: SocketAddr,
    request: &str,
    invariants: &mut Vec<(String, String)>,
) -> Result<(String, Vec<(u64, String)>), String> {
    if !request.contains("<golden:") {
        return Ok((request.to_owned(), Vec::new()));
    }
    let (head, body) = request
        .split_once("\r\n\r\n")
        .ok_or("request has no head")?;
    let mut lines: Vec<String> = head.split("\r\n").map(str::to_owned).collect();
    let line = &lines[0];
    let first = line
        .split_once(' ')
        .map_or(line.clone(), |(_, rest)| format!("GET {rest}"));
    let probe: Vec<&str> = std::iter::once(first.as_str())
        .chain(lines[1..].iter().map(String::as_str).filter(|line| {
            let name = line
                .split(':')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            !matches!(
                name.as_str(),
                "if-none-match"
                    | "if-range"
                    | "if-modified-since"
                    | "if-unmodified-since"
                    | "range"
            )
        }))
        .collect();
    let earlier = routes::exchange(
        address,
        format!("{}\r\n\r\n", probe.join("\r\n")).as_bytes(),
    )
    .map_err(|e| format!("validator probe: {e}"))?;
    let mut bound = Vec::new();
    for line in &mut lines[1..] {
        while let Some(start) = line.find("\"<golden:etag:") {
            let tail = &line[start + 14..];
            let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
            let ordinal: u64 = tail[..digits].parse().map_err(|_| "etag placeholder")?;
            let tag = earlier.header("etag").ok_or("vosu sent no ETag to echo")?;
            let opaque = tag.strip_prefix("W/").unwrap_or(tag).to_owned();
            line.replace_range(start..start + 14 + digits + 2, &opaque);
            bound.push((ordinal, opaque));
        }
        if line.contains(LAST_MODIFIED) {
            let modified = earlier
                .header("last-modified")
                .ok_or("vosu sent no Last-Modified to echo")?;
            last_modified(modified, invariants);
            *line = line.replace(LAST_MODIFIED, modified);
        }
    }
    Ok((format!("{}\r\n\r\n{body}", lines.join("\r\n")), bound))
}

fn last_modified(value: &str, invariants: &mut Vec<(String, String)>) {
    let declared = httpdate::fmt_http_date(replay_mtime());
    if value != declared {
        invariants.push((
            "headers.last-modified".into(),
            format!("header-last-modified invariant: {value:?} is not the declared replay file mtime {declared:?}"),
        ));
    }
}

pub fn normalize(
    response: RawResponse,
    mut frames: Vec<Frame>,
    roots: &[(String, String)],
    bound: Vec<(u64, String)>,
    mut invariants: Vec<(String, String)>,
) -> Observed {
    // `root-prefix` for the replay's real roots: the one-level JSON-escaped
    // spelling first, then the raw one.
    let rewrite = |text: &str| -> String {
        let escape = |s: &str| {
            let quoted = serde_json::to_string(s).expect("strings serialize");
            quoted[1..quoted.len() - 1].to_owned()
        };
        let mut out = text.to_owned();
        for (temp, synthetic) in roots {
            out = out
                .replace(&escape(temp), &escape(synthetic))
                .replace(temp, synthetic);
        }
        out
    };
    let mut body = response.body;
    let raw_body_len = body.len();
    let mut rewritten = false;
    if let Ok(text) = std::str::from_utf8(&body) {
        let normalized = rewrite(text);
        rewritten = normalized != text;
        body = normalized.into_bytes();
    }
    for frame in &mut frames {
        if frame.from_server
            && frame.opcode == 1
            && let Ok(text) = std::str::from_utf8(&frame.payload)
        {
            frame.payload = rewrite(text).into_bytes();
        }
    }
    let mut headers = Vec::new();
    for (name, value) in response.headers {
        let name = name.to_ascii_lowercase();
        if !(compared(&name) || name == "content-length") {
            continue;
        }
        let mut value = rewrite(&value);
        if name == "content-length" && rewritten {
            if !valid_content_length(&value, Some(raw_body_len)) {
                invariants.push((
                    "headers.content-length".into(),
                    format!("vosu {value:?} does not describe its raw {raw_body_len}-byte body"),
                ));
            }
            value = body.len().to_string();
        }
        headers.push((name, value));
    }
    headers.sort_by(|a, b| a.0.cmp(&b.0));
    Observed {
        status: response.status,
        headers,
        body,
        raw_body_len,
        frames,
        bound,
        invariants,
        closed: false,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mismatch {
    pub case: String,
    pub part: String,
    pub detail: Detail,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Detail {
    Other(String),
    Json {
        frame: Option<usize>,
        deltas: Vec<Delta>,
    },
}

impl From<String> for Detail {
    fn from(value: String) -> Self {
        Self::Other(value)
    }
}

#[derive(Debug, Default)]
pub struct Report {
    pub mismatches: Vec<Mismatch>,
    /// Indices of the rules that subtracted something.
    pub used: BTreeSet<usize>,
}

/// `header-etag` ordinals bound one-to-one to vosu tags across a platform.
#[derive(Default)]
struct Etags {
    by_ordinal: BTreeMap<u64, String>,
    by_tag: BTreeMap<String, u64>,
}
impl Etags {
    fn bind(&mut self, ordinal: u64, tag: &str) -> Result<(), String> {
        let known = (self.by_ordinal.get(&ordinal), self.by_tag.get(tag));
        match known {
            (None, None) => {
                self.by_ordinal.insert(ordinal, tag.to_owned());
                self.by_tag.insert(tag.to_owned(), ordinal);
                Ok(())
            }
            (Some(t), Some(o)) if t == tag && *o == ordinal => Ok(()),
            _ => Err(format!(
                "header-etag invariant: upstream tag {ordinal} and vosu tag {tag} are not one-to-one"
            )),
        }
    }
}

pub(super) fn has_json_content_type(headers: &[(String, String)]) -> bool {
    headers.iter().any(|(name, value)| {
        if name != "content-type" {
            return false;
        }
        let media = value.split(';').next().unwrap_or_default().trim();
        media.eq_ignore_ascii_case("application/json")
            || media
                .rsplit_once('/')
                .and_then(|(_, subtype)| subtype.get(subtype.len().saturating_sub(5)..))
                .is_some_and(|suffix| suffix.eq_ignore_ascii_case("+json"))
    })
}

fn media_type_parts(value: &str) -> impl Iterator<Item = &str> {
    let (mut quoted, mut escaped) = (false, false);
    value
        .split(move |c| {
            if escaped {
                escaped = false;
            } else if c == '\\' && quoted {
                escaped = true;
            } else if c == '"' {
                quoted = !quoted;
            } else if c == ';' && !quoted {
                return true;
            }
            false
        })
        .enumerate()
        .filter_map(|(index, part)| {
            if index == 0 {
                Some(part.trim_end())
            } else if part
                .split_once('=')
                .is_some_and(|(key, _)| key.trim().eq_ignore_ascii_case("charset"))
            {
                None
            } else {
                Some(part)
            }
        })
}

pub(super) fn same_header(name: &str, left: &str, right: &str) -> bool {
    match name {
        "content-type" => media_type_parts(left).eq(media_type_parts(right)),
        _ => left == right,
    }
}

fn valid_content_length(value: &str, size: Option<usize>) -> bool {
    !value.is_empty()
        && value.bytes().all(|b| b.is_ascii_digit())
        && size.is_none_or(|size| value.parse::<usize>() == Ok(size))
}

pub(super) fn same_body(got: &[u8], want: &[u8], json: bool) -> bool {
    same_body_on(got, want, json, "")
}

pub(super) fn same_body_on(got: &[u8], want: &[u8], json: bool, route: &str) -> bool {
    body_matches(got, want, json, Scope::strict(route))
}

fn body_matches(got: &[u8], want: &[u8], json: bool, scope: Scope<'_>) -> bool {
    if !json || (got.is_empty() && want.is_empty()) {
        return got == want;
    }
    scope.same_json(got, want) == Some(true)
}

pub(super) fn same_text_frame(got: &[u8], want: &[u8]) -> bool {
    same_text_frame_on(got, want, "")
}

pub(super) fn same_text_frame_on(got: &[u8], want: &[u8], route: &str) -> bool {
    Scope::strict(route)
        .same_json(got, want)
        .unwrap_or(got == want)
}

fn body_len(body: &Body) -> usize {
    match body {
        Body::Text(text) => text.len(),
        Body::Asset { length, .. } => *length,
    }
}

fn expected_size(case: &Case, get: Option<&Case>) -> Option<usize> {
    if case.expected.status == 304 {
        None
    } else if case.request.starts_with("HEAD ") {
        get.filter(|get| get.expected.status == case.expected.status)
            .map(|get| body_len(&get.expected.body))
    } else {
        Some(body_len(&case.expected.body))
    }
}

fn observed_size(case: &Case, observed: &Observed, get: Option<&Observed>) -> Option<usize> {
    if observed.status == 304 {
        None
    } else if case.request.starts_with("HEAD ") {
        get.filter(|get| get.status == observed.status)
            .map(|get| get.raw_body_len)
    } else {
        Some(observed.body.len())
    }
}

/// Compares every case of `platform` with its observation.
pub fn compare(
    platform: &Platform,
    observations: &[Result<Observed, String>],
    rules: &[Rule],
) -> Report {
    let mut report = Report::default();
    let mut etags = Etags::default();
    for (case, observed) in platform.cases.iter().zip(observations) {
        let mut push = |part: &str, detail: Detail| {
            report.mismatches.push(Mismatch {
                case: case.id.clone(),
                part: part.to_owned(),
                detail,
            });
        };
        let observed = match observed {
            Ok(observed) => observed,
            Err(error) => {
                push("transport", error.clone().into());
                continue;
            }
        };
        for (part, detail) in &observed.invariants {
            push(part, detail.clone().into());
        }
        for (ordinal, tag) in &observed.bound {
            if let Err(error) = etags.bind(*ordinal, tag) {
                push("request", error.into());
            }
        }
        if case.expected.closed || observed.closed {
            if !observed.closed {
                push(
                    "outcome",
                    format!(
                        "vosu answered {}, expected the connection to close without a response",
                        observed.status
                    )
                    .into(),
                );
            } else if !case.expected.closed {
                push(
                    "outcome",
                    Detail::Other("vosu closed the connection without a response".into()),
                );
            }
            continue;
        }
        let rule = |part: &Part| {
            rules
                .iter()
                .find(|r| r.part == *part && r.covers(&platform.name, &case.id))
        };
        let mut status = case.expected.status;
        let mut headers = case.expected.headers.clone();
        if let Some(r) = rule(&Part::WsHandshake) {
            (status, headers) = divergence::handshake(&r.change).expect("validated handshake");
            report.used.insert(r.index);
        }
        if let Some(r) = rule(&Part::Status) {
            status = divergence::status(&r.change).expect("validated status");
            report.used.insert(r.index);
        }
        if observed.status != status {
            push(
                "status",
                format!("vosu {}, expected {status}", observed.status).into(),
            );
        }
        let matching_get = case.request.strip_prefix("HEAD ").and_then(|target| {
            platform.cases.iter().zip(observations).find(|(get, _)| {
                get.barrier == case.barrier && get.request.strip_prefix("GET ") == Some(target)
            })
        });
        let expected_size = expected_size(case, matching_get.map(|(get, _)| get));
        let observed_size = observed_size(
            case,
            observed,
            matching_get.and_then(|(_, get)| get.as_ref().ok()),
        );
        // Validate the original evidence and the observation, not the bytes a
        // divergence rule substituted. A body rule never changes wire framing.
        for (side, headers, size) in [
            ("upstream", &case.expected.headers, expected_size),
            ("vosu", &observed.headers, observed_size),
        ] {
            for (_, value) in headers.iter().filter(|(name, _)| name == "content-length") {
                if !valid_content_length(value, size) {
                    push(
                        "headers.content-length",
                        format!(
                            "{side} {value:?} is not consistent with its own response length {size:?}"
                        )
                        .into(),
                    );
                }
            }
        }
        let names: BTreeSet<&str> = headers
            .iter()
            .chain(&observed.headers)
            .map(|(name, _)| name.as_str())
            .chain(rules.iter().filter_map(|rule| match &rule.part {
                Part::Header(name) if rule.covers(&platform.name, &case.id) => Some(name.as_str()),
                _ => None,
            }))
            .filter(|name| compared(name))
            .collect();
        for name in names {
            let values = |list: &[(String, String)]| -> Vec<String> {
                list.iter()
                    .filter(|(n, _)| n == name)
                    .map(|(_, v)| v.clone())
                    .collect()
            };
            let mut want = values(&headers);
            if let Some(r) = rule(&Part::Header(name.to_owned())) {
                want = divergence::header_value(&r.change).into_iter().collect();
                report.used.insert(r.index);
            }
            let got = values(&observed.headers);
            let same = want.len() == got.len()
                && want
                    .iter()
                    .zip(&got)
                    .all(|(want, got)| same_header(name, want, got));
            if !same {
                push(
                    &format!("headers.{name}"),
                    format!("vosu {got:?}, expected {want:?}").into(),
                );
            }
        }
        let body_rule = rule(&Part::Body);
        let want = match (&case.expected.body, body_rule) {
            (Body::Text(text), Some(r)) => {
                report.used.insert(r.index);
                divergence::apply_text(&r.change, text).map(|text| Cow::Owned(text.into_bytes()))
            }
            (Body::Text(text), None) => Ok(Cow::Borrowed(text.as_bytes())),
            (Body::Asset { .. }, Some(r)) => {
                report.used.insert(r.index);
                divergence::apply_text(&r.change, "").map(|text| Cow::Owned(text.into_bytes()))
            }
            (Body::Asset { id, offset, length }, _) => Ok(Cow::Borrowed(
                &platform.asset(id).expect("validated asset").bytes[*offset..offset + length],
            )),
        };
        let json = rule(&Part::Header("content-type".into())).map_or_else(
            || has_json_content_type(&headers),
            |r| {
                divergence::header_value(&r.change).is_some_and(|value| {
                    let media = [("content-type".to_owned(), value)];
                    has_json_content_type(&media)
                })
            },
        ) && (!matches!(case.expected.body, Body::Asset { .. }) || body_rule.is_some());
        let scope = Scope::for_case(case, platform);
        match want {
            Ok(want) if body_matches(&observed.body, &want, json, scope) => {}
            Ok(want) => push(
                "body",
                payload_difference(&observed.body, &want, json, scope, None),
            ),
            Err(error) => push("body", error.into()),
        }
        let sequence = sequence_rule(rules, platform, case);
        if let Some(r) = sequence {
            report.used.insert(r.index);
        }
        if let Some(expected) = expected_frames(case, sequence) {
            frames(
                expected,
                &observed.frames,
                scope,
                rule(&Part::WsFrame),
                &mut report.used,
                &mut push,
            );
        }
    }
    report
}

/// The terminal client-initiated normal close is capture cleanup, not a
/// payload observation (GOLDEN_CONTRACT §6). Retain other control frames,
/// including early or server-initiated closes, at their exact positions.
pub(super) fn payload_frames(
    frames: &[super::fixture::ExpectedFrame],
) -> &[super::fixture::ExpectedFrame] {
    match frames {
        [payload @ .., sent, received]
            if sent.fin
                && received.fin
                && sent.opcode == 8
                && received.opcode == 8
                && !sent.from_server
                && received.from_server
                && sent.masked
                && !received.masked
                && sent.payload == 1000_u16.to_be_bytes()
                && received.payload == sent.payload =>
        {
            payload
        }
        _ => frames,
    }
}

fn frames(
    expected: &[super::fixture::ExpectedFrame],
    observed: &[Frame],
    scope: Scope<'_>,
    rule: Option<&Rule>,
    used: &mut BTreeSet<usize>,
    push: &mut impl FnMut(&str, Detail),
) {
    if let Some(rule) = rule {
        used.insert(rule.index);
    }
    if expected.len() != observed.len() {
        push(
            "ws.frame",
            format!(
                "vosu sent {} frames, expected {}",
                observed.len(),
                expected.len()
            )
            .into(),
        );
    }
    for (index, (want, got)) in expected.iter().zip(observed).enumerate() {
        let mut payload = Cow::Borrowed(want.payload.as_slice());
        if let Some(rule) = rule
            && rule.applies_to_frame(index)
            && want.from_server
            && want.opcode == 1
        {
            match divergence::apply_text(&rule.change, &String::from_utf8_lossy(&payload)) {
                Ok(text) => payload = Cow::Owned(text.into_bytes()),
                Err(error) => push("ws.frame", format!("frame {index}: {error}").into()),
            }
        }
        let want_flags = (want.from_server, want.fin, want.opcode, want.masked);
        let got_flags = (got.from_server, got.fin, got.opcode, got.masked);
        let text = want.fin && want.opcode == 1;
        let scope = if want.from_server {
            scope
        } else {
            Scope::strict("")
        };
        if want_flags != got_flags {
            push(
                "ws.frame",
                format!(
                    "frame {index}: (server, fin, opcode, masked) {got_flags:?}, expected {want_flags:?}"
                )
                .into(),
            );
        } else if !(if text {
            scope
                .same_json(&got.payload, &payload)
                .unwrap_or(got.payload.as_slice() == payload.as_ref())
        } else {
            payload == got.payload
        }) {
            push(
                "ws.frame",
                payload_difference(&got.payload, &payload, text, scope, Some(index)),
            );
        }
    }
}

fn payload_difference(
    got: &[u8],
    want: &[u8],
    json: bool,
    scope: Scope<'_>,
    frame: Option<usize>,
) -> Detail {
    if json
        && let Some(deltas) = scope.json_differences(got, want)
        && !deltas.is_empty()
    {
        return Detail::Json { frame, deltas };
    }
    let bytes = difference(got, want);
    match frame {
        Some(index) => format!("frame {index}: {bytes}").into(),
        None => bytes.into(),
    }
}

fn difference(got: &[u8], want: &[u8]) -> String {
    let at = got.iter().zip(want).take_while(|(a, b)| a == b).count();
    let excerpt = |bytes: &[u8]| {
        let start = at.saturating_sub(24);
        String::from_utf8_lossy(&bytes[start.min(bytes.len())..(at + 40).min(bytes.len())])
            .into_owned()
    };
    format!(
        "vosu {} bytes, expected {}; first difference at byte {at}: vosu {:?}, expected {:?}",
        got.len(),
        want.len(),
        excerpt(got),
        excerpt(want)
    )
}
