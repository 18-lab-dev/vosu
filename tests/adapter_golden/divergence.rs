//! Divergence subtraction (`GOLDEN_CONTRACT.md` §10, A4b.11).
//!
//! `divergence-rules.json` rules name a ledger row of `DIVERGENCES.md`, exact
//! platforms and case IDs, one record part and the exact vosu value of that
//! part (or minimal JSON Pointer replacements inside a JSON body or frame).
//! A rule is refused when its row is unknown or `temporary`, when a listed
//! case is missing on a listed platform, when it changes nothing, when two
//! rules claim one part, or when it subtracts a whole status, body,
//! handshake, frame or every case of a route without a ledger scope naming
//! that route verbatim. Each other comparison still follows §6–7; a body
//! rule never touches a header, and an unused rule fails the run.
use super::{
    compare::{has_json_content_type, same_body, same_header, same_text_frame},
    envelope::tokens,
    fixture::{Body, Case, Expected, Fixture, compared, object, string_list},
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const FORMAT: &str = "vosu-golden-divergence-rules-v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerRow {
    pub class: String,
    pub scope: String,
}

/// The `| id | class | scope | reason | closed by |` rows.
pub fn ledger(text: &str) -> Result<BTreeMap<String, LedgerRow>, String> {
    let mut rows = BTreeMap::new();
    let mut table = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("| id | class | scope |") {
            table = true;
            continue;
        }
        if !table || line.starts_with("| ---") {
            continue;
        }
        if !line.starts_with('|') {
            break;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        let [id, class, scope, _reason, _closed] = cells[..] else {
            return Err(format!("ledger row {line:?} does not have five cells"));
        };
        let row = LedgerRow {
            class: class.to_owned(),
            scope: scope.to_owned(),
        };
        if rows.insert(id.to_owned(), row).is_some() {
            return Err(format!("ledger id {id} appears twice"));
        }
    }
    if rows.is_empty() {
        return Err("the ledger has no rows".into());
    }
    Ok(rows)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    Status,
    Header(String),
    Body,
    WsHandshake,
    WsFrame,
}
impl Part {
    pub fn name(&self) -> String {
        match self {
            Part::Status => "status".into(),
            Part::Header(name) => format!("headers.{name}"),
            Part::Body => "body".into(),
            Part::WsHandshake => "ws.handshake".into(),
            Part::WsFrame => "ws.frame".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Change {
    /// The exact vosu value of the whole part.
    Value(Value),
    /// Minimal replacements inside a JSON body or frame.
    Replacements(Vec<(String, Value)>),
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub index: usize,
    pub ledger_id: String,
    pub platforms: Vec<String>,
    pub cases: Vec<String>,
    pub part: Part,
    pub change: Change,
}
impl Rule {
    pub fn covers(&self, platform: &str, case: &str) -> bool {
        self.platforms.iter().any(|p| p == platform) && self.cases.iter().any(|c| c == case)
    }
    fn whole(&self) -> bool {
        match (&self.part, &self.change) {
            (Part::Status | Part::WsHandshake, _)
            | (Part::Body | Part::WsFrame, Change::Value(_)) => true,
            (_, Change::Replacements(list)) => list.iter().any(|(pointer, _)| pointer.is_empty()),
            (Part::Header(_), Change::Value(_)) => false,
        }
    }
    fn label(&self) -> String {
        format!("rule {} ({})", self.index, self.ledger_id)
    }
}

/// Parses and validates `divergence-rules.json` against the ledger and the
/// fixture's recorded cases.
pub fn rules(
    value: Option<&Value>,
    ledger: &BTreeMap<String, LedgerRow>,
    fixture: &Fixture,
) -> Result<Vec<Rule>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let file = object(value, "divergence-rules.json", &["format", "rules"])?;
    if file["format"] != FORMAT {
        return Err(format!("divergence-rules.json: format is not {FORMAT}"));
    }
    let list = file["rules"]
        .as_array()
        .ok_or("divergence-rules.json: rules")?;
    let mut out: Vec<Rule> = Vec::with_capacity(list.len());
    for (index, item) in list.iter().enumerate() {
        let rule = rule(index, item, ledger, fixture)?;
        for platform in &rule.platforms {
            for case in &rule.cases {
                if let Some(other) = out
                    .iter()
                    .find(|o| o.part == rule.part && o.covers(platform, case))
                {
                    return Err(format!(
                        "{} and {} both claim {platform} {case} {}",
                        other.label(),
                        rule.label(),
                        rule.part.name()
                    ));
                }
            }
        }
        out.push(rule);
    }
    Ok(out)
}

fn rule(
    index: usize,
    item: &Value,
    ledger: &BTreeMap<String, LedgerRow>,
    fixture: &Fixture,
) -> Result<Rule, String> {
    let at = format!("divergence rule {index}");
    let map = item.as_object().ok_or(format!("{at}: not an object"))?;
    let change_key = match (
        map.contains_key("vosu_value"),
        map.contains_key("vosu_replacements"),
    ) {
        (true, false) => "vosu_value",
        (false, true) => "vosu_replacements",
        _ => {
            return Err(format!(
                "{at}: needs exactly one of vosu_value or vosu_replacements"
            ));
        }
    };
    object(
        item,
        &at,
        &["ledger_id", "platforms", "cases", "part", change_key],
    )?;
    let ledger_id = map["ledger_id"].as_str().unwrap_or_default().to_owned();
    let row = ledger.get(&ledger_id).ok_or(format!(
        "{at}: ledger id {ledger_id:?} is not a DIVERGENCES.md row"
    ))?;
    match row.class.as_str() {
        "security" | "contract" | "licensing" => {}
        "temporary" => {
            return Err(format!(
                "{at}: ledger row {ledger_id} is temporary; temporary rows never subtract"
            ));
        }
        class => {
            return Err(format!(
                "{at}: ledger row {ledger_id} has unknown class {class:?}"
            ));
        }
    }
    let platforms = unique(string_list(&map["platforms"], &at)?, &at, "platforms")?;
    let cases = unique(string_list(&map["cases"], &at)?, &at, "cases")?;
    let mut targets = Vec::new();
    for platform in &platforms {
        let known = fixture
            .platforms
            .iter()
            .find(|p| p.name == *platform)
            .ok_or(format!("{at}: platform {platform} has no fixtures"))?;
        for id in &cases {
            let case = known.cases.iter().find(|c| c.id == *id).ok_or(format!(
                "{at}: {platform} has no case {id}; the rule is unused there"
            ))?;
            targets.push((known, case));
        }
    }
    let part = match map["part"].as_str().unwrap_or_default() {
        "status" => Part::Status,
        "body" => Part::Body,
        "ws.handshake" => Part::WsHandshake,
        "ws.frame" => Part::WsFrame,
        other => match other.strip_prefix("headers.") {
            Some(name) if compared(name) && name != "date" && name == name.to_lowercase() => {
                Part::Header(name.to_owned())
            }
            _ => return Err(format!("{at}: part {other:?} is not a §10 record part")),
        },
    };
    if matches!(part, Part::WsHandshake | Part::WsFrame)
        && targets.iter().any(|(_, case)| case.family != "ws")
    {
        return Err(format!("{at}: {} applies only to ws cases", part.name()));
    }
    let change = change(&part, &map[change_key], change_key == "vosu_value")
        .map_err(|e| format!("{at}: {e}"))?;
    let rule = Rule {
        index,
        ledger_id,
        platforms,
        cases,
        part,
        change,
    };
    let mut whole = rule.whole();
    for (platform, _) in &targets {
        let mut routes: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
        for case in &platform.cases {
            routes.entry(route(case)).or_default().insert(&case.id);
        }
        whole |= routes
            .values()
            .any(|ids| ids.iter().all(|id| rule.cases.iter().any(|c| c == id)));
    }
    for (_, case) in &targets {
        if whole && !scope_names(&row.scope, &route(case)) {
            return Err(format!(
                "{}: a whole-route subtraction on {} needs a ledger scope naming {} verbatim",
                rule.label(),
                case.id,
                route(case)
            ));
        }
        let changed = derive(&rule, &case.expected)
            .map_err(|e| format!("{} at {}: {e}", rule.label(), case.id))?;
        if !changed {
            return Err(format!(
                "{} changes nothing at {}; it is unused there",
                rule.label(),
                case.id
            ));
        }
    }
    Ok(rule)
}

fn unique(list: Vec<String>, at: &str, what: &str) -> Result<Vec<String>, String> {
    let set: BTreeSet<&String> = list.iter().collect();
    if list.is_empty() || set.len() != list.len() {
        return Err(format!(
            "{at}: {what} must be a nonempty list without repeats"
        ));
    }
    Ok(list)
}

fn change(part: &Part, value: &Value, whole: bool) -> Result<Change, String> {
    if !whole {
        if !matches!(part, Part::Body | Part::WsFrame) {
            return Err(format!("{} takes vosu_value only", part.name()));
        }
        let mut list: Vec<(String, Value)> = Vec::new();
        for item in value
            .as_array()
            .filter(|l| !l.is_empty())
            .ok_or("empty replacements")?
        {
            let item = object(item, "replacement", &["pointer", "value"])?;
            let pointer = item["pointer"].as_str().ok_or("replacement pointer")?;
            let path = tokens(pointer).ok_or(format!("malformed pointer {pointer:?}"))?;
            if list.iter().any(|(other, _)| {
                let other = tokens(other).expect("checked pointer");
                other.starts_with(&path) || path.starts_with(&other)
            }) {
                return Err(format!("pointer {pointer:?} overlaps another replacement"));
            }
            list.push((pointer.to_owned(), item["value"].clone()));
        }
        return Ok(Change::Replacements(list));
    }
    let valid = match part {
        Part::Status => value.as_u64().is_some_and(|s| (100..=599).contains(&s)),
        Part::Header(_) => value.is_string() || value.is_null(),
        Part::Body | Part::WsFrame => value.is_string(),
        Part::WsHandshake => object(value, "ws.handshake", &["status", "headers"]).is_ok_and(|m| {
            m["status"]
                .as_u64()
                .is_some_and(|s| (100..=599).contains(&s))
                && headers(value).is_some()
        }),
    };
    if !valid {
        return Err(format!("vosu_value does not fit part {}", part.name()));
    }
    Ok(Change::Value(value.clone()))
}

fn headers(handshake: &Value) -> Option<Vec<(String, String)>> {
    handshake["headers"]
        .as_array()?
        .iter()
        .map(|pair| match pair.as_array()?.as_slice() {
            [Value::String(n), Value::String(v)] if compared(n) => Some((n.clone(), v.clone())),
            _ => None,
        })
        .collect()
}

/// Whether the rule's vosu value differs from the upstream part, which a
/// rule must do to be used; errors when a pointer is not in the upstream
/// record.
fn derive(rule: &Rule, expected: &Expected) -> Result<bool, String> {
    Ok(match &rule.part {
        Part::Status => Some(expected.status) != status(&rule.change),
        Part::Header(name) => {
            let upstream: Vec<&str> = expected
                .headers
                .iter()
                .filter(|(n, _)| n == name)
                .map(|(_, v)| v.as_str())
                .collect();
            match header_value(&rule.change).as_deref() {
                Some(value) => {
                    !matches!(upstream.as_slice(), [old] if same_header(name, value, old))
                }
                None => !upstream.is_empty(),
            }
        }
        Part::Body => {
            let Body::Text(text) = &expected.body else {
                return Err("a binary body is never subtracted".into());
            };
            !same_body(
                apply_text(&rule.change, text)?.as_bytes(),
                text.as_bytes(),
                has_json_content_type(&expected.headers),
            )
        }
        Part::WsHandshake => {
            let (status, headers) = handshake(&rule.change).expect("validated handshake");
            status != expected.status
                || headers.len() != expected.headers.len()
                || headers
                    .iter()
                    .zip(&expected.headers)
                    .any(|((name, value), (old_name, old))| {
                        name != old_name || !same_header(name, value, old)
                    })
        }
        Part::WsFrame => {
            let mut changed = false;
            let mut any = false;
            for frame in expected.frames.iter().flatten() {
                if frame.from_server && frame.opcode == 1 {
                    any = true;
                    let text = std::str::from_utf8(&frame.payload)
                        .map_err(|_| "server text frame is not UTF-8")?;
                    let replacement = apply_text(&rule.change, text)?;
                    changed |= if frame.fin {
                        !same_text_frame(replacement.as_bytes(), &frame.payload)
                    } else {
                        replacement != text
                    };
                }
            }
            if !any {
                return Err("no server text frame to subtract from".into());
            }
            changed
        }
    })
}

pub fn status(change: &Change) -> Option<u16> {
    match change {
        Change::Value(value) => value.as_u64().and_then(|s| u16::try_from(s).ok()),
        Change::Replacements(_) => None,
    }
}

pub fn header_value(change: &Change) -> Option<String> {
    match change {
        Change::Value(Value::String(value)) => Some(value.clone()),
        _ => None,
    }
}

pub fn handshake(change: &Change) -> Option<(u16, Vec<(String, String)>)> {
    let Change::Value(value) = change else {
        return None;
    };
    Some((
        u16::try_from(value["status"].as_u64()?).ok()?,
        headers(value)?,
    ))
}

/// The vosu text of a body or frame: the whole value, or the upstream text
/// with each pointer's value span replaced and every other byte kept.
pub fn apply_text(change: &Change, upstream: &str) -> Result<String, String> {
    match change {
        Change::Value(value) => Ok(value.as_str().ok_or("vosu_value is not text")?.to_owned()),
        Change::Replacements(list) => {
            let mut spans = Vec::with_capacity(list.len());
            for (pointer, value) in list {
                let path = tokens(pointer).ok_or("malformed pointer")?;
                let (start, end) = locate(upstream.as_bytes(), &path)?
                    .ok_or(format!("pointer {pointer:?} is not in the upstream JSON"))?;
                spans.push((
                    start,
                    end,
                    serde_json::to_string(value).map_err(|e| e.to_string())?,
                ));
            }
            spans.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
            let mut out = upstream.to_owned();
            for (start, end, text) in spans {
                out.replace_range(start..end, &text);
            }
            Ok(out)
        }
    }
}

/// Whether `scope` names `route` as a whole path, not as a prefix.
pub fn scope_names(scope: &str, route: &str) -> bool {
    let path = |c: char| c.is_ascii_alphanumeric() || "/_.-{}*%".contains(c);
    scope.match_indices(route).any(|(at, _)| {
        let before = scope[..at].chars().next_back();
        let after = scope[at + route.len()..].chars().next();
        !before.is_some_and(path) && !after.is_some_and(path)
    })
}

/// A case's route: its request target without the query.
pub fn route(case: &Case) -> String {
    let line = case.request.split("\r\n").next().unwrap_or_default();
    let target = line.split(' ').nth(1).unwrap_or_default();
    target.split('?').next().unwrap_or_default().to_owned()
}

/// The byte span of the JSON value at `path`, or `None` when absent.
pub fn locate(text: &[u8], path: &[String]) -> Result<Option<(usize, usize)>, String> {
    let mut at = skip(text, 0);
    for token in path {
        match text.get(at) {
            Some(b'{') => {
                let mut cursor = skip(text, at + 1);
                let mut found = None;
                while text.get(cursor) != Some(&b'}') {
                    let key_end = end(text, cursor)?;
                    let key: String = serde_json::from_slice(&text[cursor..key_end])
                        .map_err(|_| "object key is not a string")?;
                    cursor = skip(text, key_end);
                    if text.get(cursor) != Some(&b':') {
                        return Err("missing ':' in object".into());
                    }
                    let value = skip(text, cursor + 1);
                    if key == *token {
                        found = Some(value);
                    }
                    cursor = skip(text, end(text, value)?);
                    if text.get(cursor) == Some(&b',') {
                        cursor = skip(text, cursor + 1);
                    }
                }
                let Some(value) = found else { return Ok(None) };
                at = value;
            }
            Some(b'[') => {
                let Ok(wanted) = token.parse::<usize>() else {
                    return Ok(None);
                };
                let mut cursor = skip(text, at + 1);
                let mut index = 0;
                let mut found = None;
                while text.get(cursor) != Some(&b']') {
                    if index == wanted {
                        found = Some(cursor);
                    }
                    cursor = skip(text, end(text, cursor)?);
                    if text.get(cursor) == Some(&b',') {
                        cursor = skip(text, cursor + 1);
                    }
                    index += 1;
                }
                let Some(value) = found else { return Ok(None) };
                at = value;
            }
            _ => return Ok(None),
        }
    }
    Ok(Some((at, end(text, at)?)))
}

fn skip(text: &[u8], mut at: usize) -> usize {
    while text.get(at).is_some_and(|b| b" \t\r\n".contains(b)) {
        at += 1;
    }
    at
}

/// The end of the JSON value starting at `at`.
fn end(text: &[u8], at: usize) -> Result<usize, String> {
    let bad = || "malformed JSON".to_owned();
    match text.get(at).ok_or_else(bad)? {
        b'"' => {
            let mut cursor = at + 1;
            loop {
                match text.get(cursor).ok_or_else(bad)? {
                    b'\\' => cursor += 2,
                    b'"' => return Ok(cursor + 1),
                    _ => cursor += 1,
                }
            }
        }
        open @ (b'{' | b'[') => {
            let close = if *open == b'{' { b'}' } else { b']' };
            let mut cursor = skip(text, at + 1);
            while text.get(cursor) != Some(&close) {
                cursor = skip(text, end(text, cursor)?);
                if matches!(text.get(cursor), Some(b':' | b',')) {
                    cursor = skip(text, cursor + 1);
                } else if text.get(cursor) != Some(&close) {
                    return Err(bad());
                }
            }
            Ok(cursor + 1)
        }
        _ => {
            let mut cursor = at;
            while text.get(cursor).is_some_and(|b| !b",]} \t\r\n".contains(b)) {
                cursor += 1;
            }
            (cursor > at).then_some(cursor).ok_or_else(bad)
        }
    }
}
