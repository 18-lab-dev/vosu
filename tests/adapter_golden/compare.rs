//! Replays fixture cases through R0's real host and listener and compares
//! each record with the upstream expectation (`GOLDEN_CONTRACT.md` §6, §7).
//!
//! Observation sends the recorded request bytes to the production listener
//! of a replay of the case's envelope. Its only substitution is a declared
//! conditional validator, taken from vosu's own answer to the same target
//! earlier in the case. The vosu record then gets exactly the §7 rules: the
//! compared header set, `header-date`, `header-last-modified` (which must
//! equal the declared replay file mtime, `AssetFile.modified`),
//! `header-etag` (an ordinal bijection with upstream, so distinct tags stay
//! distinct) and `root-prefix` for the replay's real temporary roots.
//! Comparison is exact per part (status, each header, body, each frame);
//! only a validated divergence rule replaces the expected value of its own
//! part.
use super::{
    assets::{AssetFile, ReplayAssets, ResourceAsset, RootAsset},
    case_file::etag_ordinal,
    divergence::{self, Part, Rule},
    envelope::{Envelope, reference_position, tokens, tree_position},
    fixture::{Asset, Body, Case, DATE, LAST_MODIFIED, Platform, compared},
    replay::Replay,
    routes::{self, RawResponse},
    websocket::{self, Frame, Step},
};
use std::{
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
    /// Compared headers only, lowercase, sorted by name; `etag` is raw.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Every frame in sequence order for an upgraded `ws` case.
    pub frames: Vec<Frame>,
    /// `header-etag` ordinals the request substitution bound.
    pub bound: Vec<(u64, String)>,
    /// Failed local invariants, as (part, detail).
    pub invariants: Vec<(String, String)>,
}

/// Replays every case of `platform`, one replay per distinct envelope.
pub fn observe(platform: &Platform) -> Result<Vec<Result<Observed, String>>, String> {
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
            out[index] = Some(case(&replay, &platform.cases[index], &roots));
        }
    }
    Ok(out
        .into_iter()
        .map(|o| o.expect("every case observed"))
        .collect())
}

fn case(replay: &Replay, case: &Case, roots: &[(String, String)]) -> Result<Observed, String> {
    let mut invariants = Vec::new();
    let (request, bound) = substitute(replay.address, &case.request, &mut invariants)?;
    let (response, frames) = if case.family == "ws" {
        let steps: Vec<Step> = case
            .expected
            .frames
            .iter()
            .flatten()
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
    Ok(normalize(
        response,
        frames,
        case.family == "files",
        roots,
        bound,
        invariants,
    ))
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

fn imf_fixdate(value: &str) -> bool {
    httpdate::parse_http_date(value).is_ok_and(|time| httpdate::fmt_http_date(time) == value)
}

pub fn normalize(
    response: RawResponse,
    mut frames: Vec<Frame>,
    files: bool,
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
        if !compared(&name) {
            continue;
        }
        let mut value = rewrite(&value);
        if name == "date" {
            if !imf_fixdate(&value) {
                invariants.push((
                    "headers.date".into(),
                    format!("Date {value:?} is not an IMF-fixdate"),
                ));
            }
            value = DATE.into();
        } else if files && name == "last-modified" {
            last_modified(&value, &mut invariants);
            value = LAST_MODIFIED.into();
        } else if name == "content-length" && rewritten {
            value = body.len().to_string();
        }
        headers.push((name, value));
    }
    headers.sort_by(|a, b| a.0.cmp(&b.0));
    Observed {
        status: response.status,
        headers,
        body,
        frames,
        bound,
        invariants,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mismatch {
    pub case: String,
    pub part: String,
    pub detail: String,
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

/// Compares every case of `platform` with its observation.
pub fn compare(
    platform: &Platform,
    observed: &[Result<Observed, String>],
    rules: &[Rule],
) -> Report {
    let mut report = Report::default();
    let mut etags = Etags::default();
    for (case, observed) in platform.cases.iter().zip(observed) {
        let mut push = |part: &str, detail: String| {
            report.mismatches.push(Mismatch {
                case: case.id.clone(),
                part: part.to_owned(),
                detail,
            });
        };
        let observed = match observed {
            Ok(observed) => observed,
            Err(error) => {
                push("transport", error.clone());
                continue;
            }
        };
        for (part, detail) in &observed.invariants {
            push(part, detail.clone());
        }
        for (ordinal, tag) in &observed.bound {
            if let Err(error) = etags.bind(*ordinal, tag) {
                push("request", error);
            }
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
                format!("vosu {}, expected {status}", observed.status),
            );
        }
        let names: BTreeSet<&str> = headers
            .iter()
            .chain(&observed.headers)
            .map(|(name, _)| name.as_str())
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
            let same = if name == "etag" && case.family == "files" {
                want.len() == got.len()
                    && want.iter().zip(&got).all(|(want, got)| {
                        let tag = got.strip_prefix("W/").unwrap_or(got);
                        etag_ordinal(want).is_some_and(|(weak, ordinal)| {
                            weak == got.starts_with("W/") && etags.bind(ordinal, tag).is_ok()
                        })
                    })
            } else {
                want == got
            };
            if !same {
                push(
                    &format!("headers.{name}"),
                    format!("vosu {got:?}, expected {want:?}"),
                );
            }
        }
        let body_rule = rule(&Part::Body);
        let want = match (&case.expected.body, body_rule) {
            (Body::Text(text), Some(r)) => {
                report.used.insert(r.index);
                divergence::apply_text(&r.change, text).map(String::into_bytes)
            }
            (Body::Text(text), None) => Ok(text.as_bytes().to_vec()),
            (Body::Asset { id, offset, length }, _) => Ok(platform
                .asset(id)
                .expect("validated asset")
                .bytes[*offset..offset + length]
                .to_vec()),
        };
        match want {
            Ok(want) if want == observed.body => {}
            Ok(want) => push("body", difference(&observed.body, &want)),
            Err(error) => push("body", error),
        }
        if let Some(expected) = &case.expected.frames {
            frames(
                expected,
                &observed.frames,
                rule(&Part::WsFrame),
                &mut report.used,
                &mut push,
            );
        }
    }
    report
}

fn frames(
    expected: &[super::fixture::ExpectedFrame],
    observed: &[Frame],
    rule: Option<&Rule>,
    used: &mut BTreeSet<usize>,
    push: &mut impl FnMut(&str, String),
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
            ),
        );
    }
    for (index, (want, got)) in expected.iter().zip(observed).enumerate() {
        let mut payload = want.payload.clone();
        if let Some(rule) = rule
            && want.from_server
            && want.opcode == 1
        {
            match divergence::apply_text(&rule.change, &String::from_utf8_lossy(&payload)) {
                Ok(text) => payload = text.into_bytes(),
                Err(error) => push("ws.frame", format!("frame {index}: {error}")),
            }
        }
        let want_flags = (want.from_server, want.fin, want.opcode, want.masked);
        let got_flags = (got.from_server, got.fin, got.opcode, got.masked);
        if want_flags != got_flags {
            push(
                "ws.frame",
                format!(
                    "frame {index}: (server, fin, opcode, masked) {got_flags:?}, expected {want_flags:?}"
                ),
            );
        } else if got.payload != payload {
            push(
                "ws.frame",
                format!("frame {index}: {}", difference(&got.payload, &payload)),
            );
        }
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

/// Case counts by family for one platform.
pub type Counts = (String, BTreeMap<&'static str, usize>);

/// Replays and compares every platform; returns the counts by platform and
/// family, or every mismatch and unused rule.
pub fn run(platforms: &[Platform], rules: &[Rule]) -> Result<Vec<Counts>, String> {
    let mut used = BTreeSet::new();
    let mut failures = Vec::new();
    let mut counts = Vec::new();
    for platform in platforms {
        let observed = observe(platform)?;
        let report = compare(platform, &observed, rules);
        used.extend(report.used);
        failures.extend(
            report
                .mismatches
                .into_iter()
                .map(|m| format!("{} {} {}: {}", platform.name, m.case, m.part, m.detail)),
        );
        counts.push((platform.name.clone(), platform.counts()));
    }
    for rule in rules {
        if !used.contains(&rule.index) {
            failures.push(format!(
                "divergence rule {} ({}) is unused",
                rule.index, rule.ledger_id
            ));
        }
    }
    if failures.is_empty() {
        Ok(counts)
    } else {
        Err(failures.join("\n"))
    }
}
