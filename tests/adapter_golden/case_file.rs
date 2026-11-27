//! One `cases/<id>.json` record (`vosu-golden-case-v1`): the normalized
//! request, the upstream expected record and the replay envelope. Parsing
//! is closed: every member is known, every placeholder sits where its §7
//! rule puts it and is listed in `normalized`, and `/api/ingame` or the
//! harness endpoint is refused in any spelling.
use super::{
    envelope::Envelope,
    fixture::{
        Asset, Body, Case, Expected, ExpectedFrame, FAMILIES, LAST_MODIFIED, RULE_IDS, as_usize,
        compared, expect_str, json, object, string_list,
    },
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

/// The route prefixes whose closing `/` ends a token in a request target
/// (`GOLDEN_CONTRACT.md` §7 `root-prefix`, normalization version 2).
const ROUTE_PREFIXES: [&str; 3] = ["/Songs/", "/files/beatmap/", "/files/skin/"];

const BARRIERS: [&str; 4] = ["no-client", "menu", "paused", "result"];

const CASE_KEYS: [&str; 10] = [
    "format",
    "id",
    "family",
    "barrier",
    "conditional",
    "attempts",
    "normalized",
    "request",
    "expected",
    "replay",
];

pub fn case(dir: &Path, platform: &str, id: &str, assets: &[Asset]) -> Result<Case, String> {
    let at = format!("{platform}/cases/{id}.json");
    let value = json(dir, &format!("cases/{id}.json"))?;
    let value = object(&value, &at, &CASE_KEYS)?;
    expect_str(value, "format", "vosu-golden-case-v1", &at)?;
    let family = value["family"].as_str().unwrap_or_default().to_owned();
    if value["id"] != id || !case_id(id, &family) {
        return Err(format!("{at}: id {id} is not a {family:?} case ID"));
    }
    // Route-matrix negative and diagnostic roles name themselves in an ID
    // segment; their records are never goldens (golden refuses them too).
    if id
        .split('.')
        .any(|segment| matches!(segment, "negative" | "diagnostic"))
    {
        return Err(format!(
            "{at}: negative and diagnostic route cases are never goldens"
        ));
    }
    let barrier = value["barrier"]
        .as_str()
        .filter(|b| BARRIERS.contains(b))
        .ok_or_else(|| format!("{at}: unknown barrier"))?;
    let conditional = match &value["conditional"] {
        Value::Null => None,
        Value::String(kind)
            if family == "files" && matches!(kind.as_str(), "etag" | "last-modified") =>
        {
            Some(kind.clone())
        }
        _ => {
            return Err(format!(
                "{at}: only files cases declare an etag/last-modified validator"
            ));
        }
    };
    let normalized = string_list(&value["normalized"], &at)?;
    if normalized.windows(2).any(|pair| pair[0] >= pair[1])
        || normalized
            .iter()
            .any(|rule| !RULE_IDS.contains(&rule.as_str()))
    {
        return Err(format!(
            "{at}: normalized {normalized:?} is not a sorted rule ID list"
        ));
    }
    for rule in ["client-timestamp-pointer", "raw-capability-redaction"] {
        if normalized.iter().any(|r| r == rule) {
            return Err(format!(
                "{at}: {rule} has no approved instance in a fixture"
            ));
        }
    }
    let request = object(&value["request"], &at, &["text"])?["text"]
        .as_str()
        .ok_or(format!("{at}: request text"))?
        .to_owned();
    excluded_target(&request).map_err(|e| format!("{at}: {e}"))?;
    request_placeholders(&request, conditional.as_deref(), &normalized)
        .map_err(|e| format!("{at}: {e}"))?;
    let expected =
        expected(&value["expected"], &family, assets).map_err(|e| format!("{at}: {e}"))?;
    let replay_json = serde_json::to_vec(&value["replay"]).map_err(|e| e.to_string())?;
    let replay = Envelope::parse(&replay_json).map_err(|e| format!("{at}: replay: {e}"))?;
    foreground_facts(platform, &replay).map_err(|e| format!("{at}: replay: {e}"))?;
    Ok(Case {
        id: id.to_owned(),
        family,
        barrier: barrier.to_owned(),
        conditional,
        normalized,
        request,
        expected,
        replay,
        replay_json,
    })
}

/// Core reports per-session foreground facts exactly on Windows
/// (`docs/v2/plugin-abi.md` §6.2): a Windows capture records `true`/`false`
/// for every session and a Linux capture records `null`. Anything else was
/// not recorded by that platform's production host, so it is refused
/// rather than replayed with a source the platform does not have.
pub fn foreground_facts(platform: &str, replay: &Envelope) -> Result<(), String> {
    let windows = platform.starts_with("windows-");
    match replay
        .roster
        .iter()
        .chain(&replay.session)
        .find(|session| session.foreground.is_some() != windows)
    {
        Some(session) => Err(format!(
            "session {} records foreground {:?}, which a {platform} host never reports",
            session.ordinal, session.foreground
        )),
        None => Ok(()),
    }
}

/// `/api/ingame` and the harness endpoint are never golden records, in any
/// spelling the probe could have sent.
pub fn excluded_target(request: &str) -> Result<(), String> {
    let line = request.split("\r\n").next().unwrap_or_default();
    let decoded = percent_decode(&percent_decode(line))
        .to_lowercase()
        .replace('\\', "/");
    let mut collapsed = String::with_capacity(decoded.len());
    for c in decoded.chars() {
        if !(c == '/' && collapsed.ends_with('/')) {
            collapsed.push(c);
        }
    }
    if collapsed.contains("/api/ingame") {
        return Err("an /api/ingame record is excluded from M4b goldens (A4b.12)".into());
    }
    if collapsed.contains("/__vosu_test") {
        return Err("the harness capture endpoint is not a golden route".into());
    }
    Ok(())
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let pair = bytes
            .get(index + 1..index + 3)
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        match (bytes[index], pair) {
            (b'%', Some(byte)) => {
                out.push(byte);
                index += 3;
            }
            (byte, _) => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A synthetic root sits in a request target only where `root-prefix`
/// (§7, normalization version 2) puts one: at a token boundary, which in a
/// request target includes the closing `/` of a route prefix
/// (`/Songs//vosu-golden/osu/…`, `/Songs/C%3A%5Cvosu-golden%5Cosu!%5C…`). A
/// root inside a path segment was never rewritten from an attested root, so
/// the case is refused, and a case carrying a root lists `root-prefix`.
/// Spellings are compared percent-decoded, case-folded and with `\` as `/`;
/// a Windows root also matches its device and admin-share forms.
pub fn request_roots(
    request: &str,
    roots: &BTreeMap<String, String>,
    normalized: &[String],
) -> Result<(), String> {
    let line = request.split("\r\n").next().unwrap_or_default();
    let target = line.split(' ').nth(1).unwrap_or_default();
    // Route prefixes are unencoded ASCII, so the decoded view keeps their length.
    let boundary = ROUTE_PREFIXES
        .iter()
        .find(|prefix| target.starts_with(**prefix))
        .map(|prefix| prefix.len());
    let view = percent_decode(target).to_lowercase().replace('\\', "/");
    let mut forms = Vec::new();
    for root in roots.values() {
        let bare = root.to_lowercase().replace('\\', "/");
        let bare = bare.trim_end_matches('/');
        if let Some(below) = bare.strip_prefix("c:/") {
            forms.extend(["//?/", "//./"].map(|device| format!("{device}{bare}")));
            forms.extend(["localhost", "127.0.0.1"].map(|host| format!("//{host}/c$/{below}")));
        }
        forms.push(bare.to_owned());
    }
    forms.sort_by_key(|form| std::cmp::Reverse(form.len()));
    let path_char = |c: char| c.is_ascii_alphanumeric() || "_.!~$/%-".contains(c);
    let slot_char = |c: char| c.is_ascii_alphanumeric() || "!._-".contains(c);
    let (mut index, mut found) = (0, false);
    while let Some(rest) = view.get(index..).filter(|rest| !rest.is_empty()) {
        // A form starting inside the route prefix (its closing `/`) is a
        // relative path below the route, not a root.
        let form = forms.iter().find(|form| {
            rest.starts_with(form.as_str()) && !rest[form.len()..].starts_with(slot_char)
        });
        let Some(form) = form.filter(|_| boundary.is_none_or(|end| index >= end)) else {
            index += rest.chars().next().map_or(1, char::len_utf8);
            continue;
        };
        if Some(index) != boundary && view[..index].chars().next_back().is_some_and(path_char) {
            return Err(format!(
                "request target carries synthetic root {form:?} inside a path segment, where \
                 root-prefix never puts one"
            ));
        }
        found = true;
        index += form.len();
    }
    if found && !normalized.iter().any(|rule| rule == "root-prefix") {
        return Err("request target carries a synthetic root but root-prefix is not listed".into());
    }
    Ok(())
}

/// Validator placeholders appear only in conditional request headers, and
/// exactly as the case's declared validator and normalized rules say.
fn request_placeholders(
    request: &str,
    conditional: Option<&str>,
    normalized: &[String],
) -> Result<(), String> {
    let mut found = BTreeSet::new();
    for line in request
        .split("\r\n\r\n")
        .next()
        .unwrap_or_default()
        .split("\r\n")
    {
        let (name, value) = line.split_once(':').unwrap_or((line, ""));
        let validator = matches!(
            name.trim().to_ascii_lowercase().as_str(),
            "if-none-match" | "if-range" | "if-modified-since" | "if-unmodified-since"
        );
        if value.contains("<golden:etag:") && validator {
            found.insert("etag");
        }
        if value.contains(LAST_MODIFIED) && validator {
            found.insert("last-modified");
        }
        let rest = value.replace(LAST_MODIFIED, "");
        let rest = strip_etag_placeholders(&rest);
        if rest.contains("<golden:") || (!validator && line.contains("<golden:")) {
            return Err(format!("unapproved placeholder in request line {line:?}"));
        }
    }
    for kind in &found {
        if !normalized
            .iter()
            .any(|rule| *rule == format!("header-{kind}"))
        {
            return Err(format!("request {kind} placeholder without header-{kind}"));
        }
    }
    if let Some(kind) = conditional
        && !found.contains(kind)
    {
        return Err(format!("declared {kind} validator is not in the request"));
    }
    Ok(())
}

fn strip_etag_placeholders(value: &str) -> String {
    let mut out = value.to_owned();
    while let Some(start) = out.find("\"<golden:etag:") {
        let tail = &out[start + 14..];
        let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 || !tail[digits..].starts_with(">\"") {
            break;
        }
        out.replace_range(start..start + 14 + digits + 2, "");
    }
    out
}

fn expected(value: &Value, family: &str, assets: &[Asset]) -> Result<Expected, String> {
    if value.get("outcome").is_some() {
        let value = object(value, "expected", &["outcome"])?;
        if value["outcome"] != "closed" {
            return Err("expected outcome is not closed".into());
        }
        return Ok(Expected {
            status: 0,
            headers: Vec::new(),
            body: Body::Text(String::new()),
            frames: None,
            closed: true,
        });
    }
    let ws = family == "ws";
    let keys: &[&str] = if ws {
        &["status", "headers", "body", "frames"]
    } else {
        &["status", "headers", "body"]
    };
    let value = object(value, "expected", keys)?;
    let status = value["status"]
        .as_u64()
        .and_then(|s| u16::try_from(s).ok())
        .filter(|s| (100..=599).contains(s))
        .ok_or("expected status is not an HTTP status")?;
    let mut headers = Vec::new();
    for pair in value["headers"].as_array().ok_or("expected headers")? {
        let pair = string_list(pair, "expected header")?;
        let [name, value] = <[String; 2]>::try_from(pair).map_err(|_| "header is not a pair")?;
        if !(compared(&name) || name == "content-length") {
            return Err(format!(
                "header {name} is not a payload header or local framing"
            ));
        }
        if value.contains("<golden:") {
            return Err(format!("header {name} carries an unapproved placeholder"));
        }
        headers.push((name, value));
    }
    if headers.windows(2).any(|pair| pair[0].0 > pair[1].0) {
        return Err("expected headers are not sorted by name".into());
    }
    let body = value["body"].as_object().ok_or("expected body")?;
    let body = if body.len() == 1
        && let Some(text) = body.get("text").and_then(Value::as_str)
    {
        Body::Text(text.to_owned())
    } else {
        let body = object(
            &value["body"],
            "expected body",
            &["asset", "offset", "length"],
        )?;
        let id = body["asset"].as_str().unwrap_or_default();
        let (offset, length) = (as_usize(&body["offset"])?, as_usize(&body["length"])?);
        let asset = assets
            .iter()
            .find(|a| a.id == id)
            .ok_or(format!("body names unknown asset {id:?}"))?;
        if offset
            .checked_add(length)
            .is_none_or(|end| end > asset.bytes.len())
        {
            return Err(format!("body slice is outside asset {id}"));
        }
        Body::Asset {
            id: id.to_owned(),
            offset,
            length,
        }
    };
    let frames = if ws {
        Some(
            value["frames"]
                .as_array()
                .ok_or("ws frames")?
                .iter()
                .map(frame)
                .collect::<Result<_, _>>()?,
        )
    } else {
        None
    };
    Ok(Expected {
        status,
        headers,
        body,
        frames,
        closed: false,
    })
}

pub(super) fn frame(value: &Value) -> Result<ExpectedFrame, String> {
    let value = object(
        value,
        "frame",
        &["from", "fin", "opcode", "masked", "length", "payload"],
    )?;
    let from_server = match value["from"].as_str() {
        Some("server") => true,
        Some("client") => false,
        _ => return Err("frame from is neither client nor server".into()),
    };
    let payload = value["payload"].as_object().ok_or("frame payload")?;
    let payload = match (payload.len(), payload.get("text"), payload.get("hex")) {
        (1, Some(Value::String(text)), None) => text.as_bytes().to_vec(),
        (1, None, Some(Value::String(hex))) => decode_hex(hex).ok_or("frame payload hex")?,
        _ => return Err("frame payload is not exactly text or hex".into()),
    };
    let flag = |key: &str| value[key].as_bool().ok_or(format!("frame {key}"));
    let frame = ExpectedFrame {
        from_server,
        fin: flag("fin")?,
        opcode: value["opcode"]
            .as_u64()
            .and_then(|o| u8::try_from(o).ok())
            .filter(|o| matches!(o, 0 | 1 | 2 | 8 | 9 | 10))
            .ok_or("frame opcode")?,
        masked: flag("masked")?,
        length: as_usize(&value["length"])?,
        payload,
    };
    if frame.length != frame.payload.len() {
        return Err("frame length differs from its payload".into());
    }
    Ok(frame)
}

fn case_id(id: &str, family: &str) -> bool {
    FAMILIES.contains(&family)
        && id.len() <= 120
        && id
            .strip_prefix(family)
            .and_then(|r| r.strip_prefix('.'))
            .is_some_and(|slug| {
                slug.split('.').all(|segment| {
                    segment.split('-').all(|word| {
                        !word.is_empty()
                            && word
                                .bytes()
                                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                    })
                })
            })
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    text.len()
        .is_multiple_of(2)
        .then(|| {
            (0..text.len())
                .step_by(2)
                .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
                .collect()
        })
        .flatten()
}
