//! Behavioral golden fixtures (`GOLDEN_CONTRACT.md` §9, §11, §12) and their
//! gates.
//!
//! Only `tests/fixtures/upstream/<Cargo source-commit>/` is read, and only
//! when the Cargo pin and `PINNED_SOURCE_REVISION` agree. Before the pilot
//! commits fixtures, a `PENDING_PILOT` marker naming exactly that pin is the
//! only way the pinned test passes without cases; a missing marker, a marker
//! for another pin, or a marker beside fixtures fails. Loading enforces the
//! layout G0's writer produces: exact file sets, the inventory hashes, the
//! size budget, exact brand-inventory lines, the closed normalization rule
//! list, nonzero cases for every family, no `/api/ingame` record, and a
//! provenance label. A `synthetic` fixture is vosu-generated mechanics input
//! and is refused under the pinned directory.
use super::envelope::Envelope;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

pub const PLATFORMS: [&str; 3] = ["linux-lazer", "windows-lazer", "windows-stable"];
pub const FAMILIES: [&str; 6] = [
    "state-http",
    "ws",
    "files",
    "path-security",
    "calculation",
    "origin-peer",
];
pub const NORMALIZATION_VERSION: u64 = 1;
/// `GOLDEN_CONTRACT.md` §7, first column. Version 1 approves no
/// `client-timestamp-pointer` instance, and `raw-capability-redaction` only
/// ever applies to private raw evidence.
pub const RULE_IDS: [&str; 7] = [
    "header-date",
    "header-last-modified",
    "header-etag",
    "root-prefix",
    "envelope-identity-ordinal",
    "raw-capability-redaction",
    "client-timestamp-pointer",
];
/// `GOLDEN_CONTRACT.md` §6 compared headers plus `date`, whose presence is
/// compared; every `access-control-*` header is compared too.
pub const COMPARED_HEADERS: [&str; 17] = [
    "accept-ranges",
    "cache-control",
    "connection",
    "content-length",
    "content-range",
    "content-security-policy",
    "content-type",
    "date",
    "etag",
    "last-modified",
    "location",
    "sec-websocket-accept",
    "sec-websocket-extensions",
    "sec-websocket-protocol",
    "sec-websocket-version",
    "upgrade",
    "x-content-type-options",
];
pub const JSON_BUDGET: u64 = 8 << 20;
pub const ASSET_BUDGET: u64 = 4 << 20;
pub const MARKER: &str = "PENDING_PILOT";
pub const DATE: &str = "<golden:date>";
pub const LAST_MODIFIED: &str = "<golden:last-modified>";

pub fn compared(name: &str) -> bool {
    COMPARED_HEADERS.contains(&name) || name.starts_with("access-control-")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provenance {
    Captured,
    Synthetic,
}

#[derive(Clone, Debug)]
pub struct Fixture {
    pub provenance: Provenance,
    pub platforms: Vec<Platform>,
    /// `divergence-rules.json`, when present.
    pub rules: Option<Value>,
}

#[derive(Clone, Debug)]
pub struct Platform {
    pub name: String,
    pub synthetic_roots: BTreeMap<String, String>,
    pub assets: Vec<Asset>,
    pub cases: Vec<Case>,
}
impl Platform {
    pub fn asset(&self, id: &str) -> Option<&Asset> {
        self.assets.iter().find(|asset| asset.id == id)
    }
    pub fn counts(&self) -> BTreeMap<&'static str, usize> {
        FAMILIES
            .iter()
            .map(|family| {
                (
                    *family,
                    self.cases.iter().filter(|c| c.family == *family).count(),
                )
            })
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct Asset {
    pub id: String,
    pub slot: String,
    pub relative: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Case {
    pub id: String,
    pub family: String,
    pub conditional: Option<String>,
    pub normalized: Vec<String>,
    /// The normalized request text, validator placeholders included.
    pub request: String,
    pub expected: Expected,
    pub replay: Envelope,
    /// The replay envelope's exact JSON, to group cases into one replay.
    pub replay_json: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expected {
    pub status: u16,
    /// Lowercase names, sorted by name, values as normalized.
    pub headers: Vec<(String, String)>,
    pub body: Body,
    /// `Some` exactly for the `ws` family.
    pub frames: Option<Vec<ExpectedFrame>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    Text(String),
    Asset {
        id: String,
        offset: usize,
        length: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpectedFrame {
    pub from_server: bool,
    pub fin: bool,
    pub opcode: u8,
    pub masked: bool,
    /// The normalized payload length.
    pub length: usize,
    pub payload: Vec<u8>,
}

/// What the pinned directory holds.
pub enum Pinned {
    /// No fixtures yet, acknowledged by a marker naming this pin.
    Pending {
        reason: String,
    },
    Present(Fixture),
}

pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The Cargo `source-commit` and the live-parity `PINNED_SOURCE_REVISION`,
/// which must be one 40-hex pin.
pub fn pin(repo: &Path) -> Result<String, String> {
    let cargo = quoted_after(&read_text(&repo.join("Cargo.toml"))?, "source-commit = ")
        .ok_or("Cargo.toml has no source-commit pin")?;
    let live = quoted_after(
        &read_text(&repo.join("tools/live-parity/live_parity.py"))?,
        "PINNED_SOURCE_REVISION = ",
    )
    .ok_or("live_parity.py has no PINNED_SOURCE_REVISION")?;
    agree(&cargo, &live)
}

pub fn agree(cargo: &str, live: &str) -> Result<String, String> {
    if !hex(cargo, 40) {
        return Err(format!("Cargo source-commit {cargo:?} is not a 40-hex pin"));
    }
    if cargo != live {
        return Err(format!(
            "Cargo source-commit {cargo} differs from live-parity PINNED_SOURCE_REVISION {live}"
        ));
    }
    Ok(cargo.to_owned())
}

fn quoted_after(text: &str, prefix: &str) -> Option<String> {
    let mut found = text
        .lines()
        .filter_map(|line| line.strip_prefix(prefix))
        .map(|rest| rest.trim().strip_prefix('"')?.strip_suffix('"'));
    let first = found.next()??;
    found.next().is_none().then(|| first.to_owned())
}

/// Selects `upstream/<pin>/` and applies the marker rules.
pub fn discover(upstream: &Path, pin: &str) -> Result<Pinned, String> {
    let mut entries = std::fs::read_dir(upstream)
        .map_err(|e| format!("{}: {e}", upstream.display()))?
        .map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    entries.sort();
    if let Some(stale) = entries.iter().find(|name| hex(name, 40) && *name != pin) {
        return Err(format!(
            "fixture directory for pin {stale} is not the Cargo pin {pin}; a pin move deletes \
             the previous behavioral directory (A4b.10)"
        ));
    }
    let directory = upstream.join(pin);
    let marker = upstream.join(MARKER);
    let present = directory.exists();
    match (present, marker.exists()) {
        (true, true) => Err(format!(
            "{MARKER} is present beside the golden fixtures for pin {pin}; the fixture commit \
             deletes it"
        )),
        (true, false) => load(&directory, pin, Provenance::Captured).map(Pinned::Present),
        (false, true) => {
            let text = read_text(&marker)?;
            let mut lines = text.lines();
            let (named, reason) = (lines.next().unwrap_or(""), lines.next().unwrap_or(""));
            if named != pin {
                return Err(format!(
                    "no golden fixtures for pin {pin}, and {MARKER} names {named:?} instead"
                ));
            }
            if reason.trim().is_empty() || lines.next().is_some() {
                return Err(format!(
                    "{MARKER} must hold exactly the pin and a one-line reason"
                ));
            }
            Ok(Pinned::Pending {
                reason: reason.trim().to_owned(),
            })
        }
        (false, false) => Err(format!(
            "no golden fixtures for pin {pin}: expected tests/fixtures/upstream/{pin}/ or a \
             {MARKER} marker naming that pin"
        )),
    }
}

/// Loads and gates one fixture tree in the writer's layout.
pub fn load(root: &Path, pin: &str, provenance: Provenance) -> Result<Fixture, String> {
    let files = walk(root)?;
    let manifest = json(root, "manifest.json")?;
    let manifest = object(
        &manifest,
        "manifest.json",
        &[
            "format",
            "normalization_version",
            "provenance",
            "source_revision",
            "platforms",
        ],
    )?;
    expect_str(
        manifest,
        "format",
        "vosu-golden-fixture-v1",
        "manifest.json",
    )?;
    expect_header(manifest, pin, "manifest.json")?;
    let label = label(manifest, "manifest.json")?;
    if label != provenance {
        return Err(match label {
            Provenance::Synthetic => format!(
                "manifest.json: a synthetic fixture is vosu-generated mechanics input and is \
                 never pilot or full evidence; refused for pin {pin}"
            ),
            Provenance::Captured => {
                "manifest.json: expected a synthetic mechanics fixture".to_owned()
            }
        });
    }
    let names = string_list(&manifest["platforms"], "manifest.json platforms")?;
    if names.is_empty()
        || names.windows(2).any(|pair| pair[0] >= pair[1])
        || names.iter().any(|name| !PLATFORMS.contains(&name.as_str()))
    {
        return Err(format!(
            "manifest.json platforms {names:?} are not sorted supported platforms"
        ));
    }
    for file in &files {
        let top = file.split('/').next().unwrap_or_default();
        let known = matches!(
            file.as_str(),
            "manifest.json" | "inventory.json" | "divergence-rules.json"
        ) || (file.contains('/') && names.iter().any(|name| name == top));
        if !known {
            return Err(format!("unknown fixture file {file}"));
        }
    }
    // Layout and budget come from what is on disk, before any hash is trusted.
    for name in &names {
        layout(name, &files)?;
        budget(root, name, &files)?;
    }
    let inventory = json(root, "inventory.json")?;
    let inventory = object(
        &inventory,
        "inventory.json",
        &[
            "format",
            "source_revision",
            "normalization_version",
            "files",
        ],
    )?;
    expect_str(
        inventory,
        "format",
        "vosu-golden-inventory-v1",
        "inventory.json",
    )?;
    expect_header(inventory, pin, "inventory.json")?;
    check_inventory(root, &files, &inventory["files"], "inventory.json", "")?;
    let platforms = names
        .iter()
        .map(|name| platform(root, name, pin, label, &files))
        .collect::<Result<_, _>>()?;
    let rules = files
        .contains("divergence-rules.json")
        .then(|| json(root, "divergence-rules.json"))
        .transpose()?;
    Ok(Fixture {
        provenance,
        platforms,
        rules,
    })
}

/// Every platform file is one the writer produces.
fn layout(name: &str, all: &BTreeSet<String>) -> Result<(), String> {
    let prefix = format!("{name}/");
    for file in all.iter().filter_map(|file| file.strip_prefix(&prefix)) {
        let known = matches!(
            file,
            "manifest.json" | "inventory.json" | "assets.json" | "brand-inventory.txt"
        ) || file
            .strip_prefix("cases/")
            .is_some_and(|rest| rest.ends_with(".json") && !rest.contains('/'))
            || file
                .strip_prefix("assets/")
                .is_some_and(|rest| !rest.contains('/'));
        if !known {
            return Err(format!("unknown fixture file {name}/{file}"));
        }
    }
    Ok(())
}

const PLATFORM_KEYS: [&str; 14] = [
    "format",
    "normalization_version",
    "provenance",
    "source_revision",
    "platform",
    "client",
    "source",
    "vosu",
    "scenario_set",
    "authorization",
    "review_receipt",
    "synthetic_roots",
    "case_counts",
    "cases",
];

fn platform(
    root: &Path,
    name: &str,
    pin: &str,
    provenance: Provenance,
    all: &BTreeSet<String>,
) -> Result<Platform, String> {
    let dir = root.join(name);
    let prefix = format!("{name}/");
    let files: BTreeSet<String> = all
        .iter()
        .filter_map(|file| file.strip_prefix(&prefix).map(str::to_owned))
        .collect();
    let at = |file: &str| format!("{name}/{file}");
    let manifest = json(&dir, "manifest.json")?;
    let where_ = at("manifest.json");
    let manifest = object(&manifest, &where_, &PLATFORM_KEYS)?;
    expect_str(manifest, "format", "vosu-golden-platform-v1", &where_)?;
    expect_header(manifest, pin, &where_)?;
    if label(manifest, &where_)? != provenance {
        return Err(format!(
            "{where_}: provenance differs from the revision manifest"
        ));
    }
    if manifest["platform"] != name {
        return Err(format!("{where_}: platform is not {name}"));
    }
    let (os, variant) = name.split_once('-').expect("platform names have an OS");
    if manifest["client"]["variant"] != variant {
        return Err(format!("{where_}: client variant is not {variant}"));
    }
    let source = &manifest["source"];
    if !matches!(source["kind"].as_str(), Some("source" | "native"))
        || !source["sha256"].as_str().is_some_and(|h| hex(h, 64))
        || !manifest["vosu"]["sha256"]
            .as_str()
            .is_some_and(|h| hex(h, 64))
    {
        return Err(format!(
            "{where_}: source and vosu attestations are incomplete"
        ));
    }
    if provenance == Provenance::Captured {
        if source["sha256"] == manifest["vosu"]["sha256"] {
            return Err(format!(
                "{where_}: the expected side is attested as the vosu binary; upstream is the \
                 only expected source"
            ));
        }
        if !manifest["review_receipt"].is_string() {
            return Err(format!(
                "{where_}: a committed capture needs main's review receipt (A4b.8)"
            ));
        }
    }
    let windows = os == "windows";
    let synthetic_roots = roots(&manifest["synthetic_roots"], windows, &where_)?;

    let inventory = json(&dir, "inventory.json")?;
    let where_inventory = at("inventory.json");
    let inventory = object(
        &inventory,
        &where_inventory,
        &[
            "format",
            "source_revision",
            "platform",
            "normalization_version",
            "files",
            "privacy_scan",
            "budget",
        ],
    )?;
    expect_str(
        inventory,
        "format",
        "vosu-golden-inventory-v1",
        &where_inventory,
    )?;
    expect_header(inventory, pin, &where_inventory)?;
    if inventory["platform"] != name || inventory["privacy_scan"]["result"] != "clean" {
        return Err(format!(
            "{where_inventory}: wrong platform or no clean privacy scan"
        ));
    }
    let budget = &inventory["budget"];
    if budget["json_limit"] != JSON_BUDGET || budget["asset_limit"] != ASSET_BUDGET {
        return Err(format!(
            "{where_inventory}: budget limits are not the A4b.13 limits"
        ));
    }
    check_inventory(&dir, &files, &inventory["files"], &where_inventory, &prefix)?;

    let assets = assets(&dir, name, &files, &synthetic_roots)?;
    brand(&dir, name, pin, provenance, &files)?;
    let cases = cases(&dir, name, manifest, &files, &assets)?;
    Ok(Platform {
        name: name.to_owned(),
        synthetic_roots,
        assets,
        cases,
    })
}

/// A4b.13: at most 8 MiB of JSON/CBOR-envelope files and 4 MiB of assets
/// per platform, measured on disk.
fn budget(root: &Path, name: &str, all: &BTreeSet<String>) -> Result<(), String> {
    let (mut json_bytes, mut asset_bytes) = (0, 0);
    for file in all
        .iter()
        .filter_map(|f| f.strip_prefix(&format!("{name}/")).map(str::to_owned))
    {
        let size = std::fs::metadata(root.join(name).join(&file))
            .map_err(|e| format!("{name}/{file}: {e}"))?
            .len();
        *if file.starts_with("assets/") {
            &mut asset_bytes
        } else {
            &mut json_bytes
        } += size;
    }
    if json_bytes > JSON_BUDGET || asset_bytes > ASSET_BUDGET {
        return Err(format!(
            "{name}: over budget ({json_bytes} JSON bytes of {JSON_BUDGET}, {asset_bytes} asset \
             bytes of {ASSET_BUDGET}); reduce the scenario or get owner approval"
        ));
    }
    Ok(())
}

fn roots(value: &Value, windows: bool, at: &str) -> Result<BTreeMap<String, String>, String> {
    let map = value
        .as_object()
        .ok_or_else(|| format!("{at}: synthetic_roots is not an object"))?;
    map.iter()
        .map(|(slot, root)| {
            let expected = if windows {
                format!("C:\\vosu-golden\\{slot}\\")
            } else {
                format!("/vosu-golden/{slot}/")
            };
            if root.as_str() != Some(expected.as_str()) {
                return Err(format!("{at}: slot {slot} is not mapped to {expected}"));
            }
            Ok((slot.clone(), expected))
        })
        .collect()
}

fn assets(
    dir: &Path,
    name: &str,
    files: &BTreeSet<String>,
    roots: &BTreeMap<String, String>,
) -> Result<Vec<Asset>, String> {
    let at = format!("{name}/assets.json");
    let value = json(dir, "assets.json")?;
    let value = object(&value, &at, &["format", "assets"])?;
    expect_str(value, "format", "vosu-golden-assets-v1", &at)?;
    let mut out = Vec::new();
    for item in value["assets"]
        .as_array()
        .ok_or(format!("{at}: no assets"))?
    {
        let item = object(
            item,
            &at,
            &[
                "id",
                "slot",
                "relative",
                "media_type",
                "generator",
                "license",
                "sha256",
                "bytes",
            ],
        )?;
        let field = |key: &str| {
            item[key]
                .as_str()
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| format!("{at}: asset {key} is not a nonempty string"))
        };
        let id = field("id")?;
        let bytes = std::fs::read(dir.join("assets").join(&id))
            .map_err(|e| format!("{name}/assets/{id}: {e}"))?;
        if sha256(&bytes) != field("sha256")? || item["bytes"] != bytes.len() {
            return Err(format!("{at}: asset {id} differs from its file"));
        }
        let slot = field("slot")?;
        if !roots.contains_key(&slot) {
            return Err(format!("{at}: asset {id} uses undeclared slot {slot}"));
        }
        field("generator")?;
        field("license")?;
        out.push(Asset {
            id,
            slot,
            relative: field("relative")?,
            media_type: field("media_type")?,
            bytes,
        });
    }
    let listed: BTreeSet<String> = out.iter().map(|a| format!("assets/{}", a.id)).collect();
    let present: BTreeSet<String> = files
        .iter()
        .filter(|f| f.starts_with("assets/"))
        .cloned()
        .collect();
    if listed != present {
        return Err(format!("{at}: listed assets differ from assets/"));
    }
    Ok(out)
}

/// `brand-inventory.txt` lines equal the old-brand counts of the platform's
/// files, and a committed fixture's lines are exact allowlist entries.
pub fn brand(
    dir: &Path,
    name: &str,
    pin: &str,
    provenance: Provenance,
    files: &BTreeSet<String>,
) -> Result<(), String> {
    let at = format!("{name}/brand-inventory.txt");
    let text = read_text(&dir.join("brand-inventory.txt"))?;
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .collect();
    // `scripts/check-brand.py`'s pattern, built at runtime so this checker is
    // not itself an old-brand occurrence.
    let needle = ["to", "su"].concat();
    let mut expected = BTreeMap::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(dir.join(file)) else {
            continue;
        };
        let count = text.to_ascii_lowercase().matches(&needle).count();
        if count > 0 && file != "brand-inventory.txt" {
            expected.insert(
                format!("tests/fixtures/upstream/{pin}/{name}/{file}"),
                count,
            );
        }
    }
    let mut seen = BTreeMap::new();
    for line in &lines {
        let mut parts = line.splitn(4, ' ');
        let (path, count, category) = (parts.next(), parts.next(), parts.next());
        let (Some(path), Some(count), Some(category), Some(_reason)) = (
            path,
            count.and_then(|c| c.parse::<usize>().ok()),
            category,
            parts.next(),
        ) else {
            return Err(format!("{at}: malformed line {line:?}"));
        };
        let asset = path.contains(&format!("/{name}/assets/"));
        if category != if asset { "fixture" } else { "parity" } {
            return Err(format!("{at}: {path} has category {category}"));
        }
        seen.insert(path.to_owned(), count);
    }
    if seen != expected {
        return Err(format!(
            "{at}: lines {seen:?} differ from the files' old-brand counts {expected:?}"
        ));
    }
    if provenance == Provenance::Captured && !lines.is_empty() {
        let allowlist = read_text(&repo_root().join("scripts/brand-allowlist.txt"))?;
        let entries: BTreeSet<&str> = allowlist.lines().map(str::trim).collect();
        if let Some(missing) = lines.iter().find(|line| !entries.contains(line.trim())) {
            return Err(format!(
                "{at}: {missing:?} is not an exact scripts/brand-allowlist.txt entry"
            ));
        }
    }
    Ok(())
}

fn cases(
    dir: &Path,
    name: &str,
    manifest: &Map<String, Value>,
    files: &BTreeSet<String>,
    assets: &[Asset],
) -> Result<Vec<Case>, String> {
    let listing = manifest["cases"]
        .as_array()
        .ok_or(format!("{name}/manifest.json: no case listing"))?;
    let mut cases = Vec::with_capacity(listing.len());
    for entry in listing {
        let entry = object(
            entry,
            &format!("{name}/manifest.json cases"),
            &["id", "family", "barrier", "attempts", "file"],
        )?;
        let id = entry["id"].as_str().unwrap_or_default();
        if entry["file"] != format!("cases/{id}.json") {
            return Err(format!(
                "{name}: case {id} is not listed at cases/{id}.json"
            ));
        }
        let case = super::case_file::case(dir, name, id, assets)?;
        if entry["family"] != case.family.as_str() {
            return Err(format!("{name}: case {id} is listed in another family"));
        }
        cases.push(case);
    }
    let listed: BTreeSet<String> = cases
        .iter()
        .map(|c| format!("cases/{}.json", c.id))
        .collect();
    let present: BTreeSet<String> = files
        .iter()
        .filter(|f| f.starts_with("cases/"))
        .cloned()
        .collect();
    if listed != present || cases.windows(2).any(|pair| pair[0].id >= pair[1].id) {
        return Err(format!(
            "{name}: listed cases differ from cases/ or are not sorted and unique"
        ));
    }
    let counts = manifest["case_counts"]
        .as_object()
        .ok_or(format!("{name}/manifest.json: no case_counts"))?;
    if counts.len() != FAMILIES.len() {
        return Err(format!(
            "{name}/manifest.json: case_counts are not the six families"
        ));
    }
    for family in FAMILIES {
        let actual = cases.iter().filter(|c| c.family == family).count();
        if counts.get(family) != Some(&Value::from(actual)) {
            return Err(format!(
                "{name}/manifest.json: {family} count is not {actual}"
            ));
        }
        if actual == 0 {
            return Err(format!(
                "{name}: zero {family} cases; a zero-case family is not evidence"
            ));
        }
    }
    Ok(cases)
}

fn check_inventory(
    dir: &Path,
    files: &BTreeSet<String>,
    listed: &Value,
    at: &str,
    prefix: &str,
) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for entry in listed.as_array().ok_or(format!("{at}: files"))? {
        let entry = object(entry, at, &["path", "bytes", "sha256"])?;
        let path = entry["path"].as_str().unwrap_or_default();
        if !files.contains(path) || path == "inventory.json" || !seen.insert(path.to_owned()) {
            return Err(format!(
                "{at}: lists {prefix}{path}, which is not a fixture file"
            ));
        }
        let bytes = std::fs::read(dir.join(path)).map_err(|e| format!("{prefix}{path}: {e}"))?;
        if entry["bytes"] != bytes.len() || entry["sha256"] != sha256(&bytes) {
            return Err(format!(
                "{at}: {prefix}{path} differs from its inventory hash"
            ));
        }
    }
    if let Some(file) = files
        .iter()
        .find(|file| *file != "inventory.json" && !seen.contains(*file))
    {
        return Err(format!("{at}: {prefix}{file} is not inventoried"));
    }
    Ok(())
}

fn label(map: &Map<String, Value>, at: &str) -> Result<Provenance, String> {
    match map["provenance"].as_str() {
        Some("captured") => Ok(Provenance::Captured),
        Some("synthetic") => Ok(Provenance::Synthetic),
        _ => Err(format!(
            "{at}: provenance is neither captured nor synthetic"
        )),
    }
}

fn expect_header(map: &Map<String, Value>, pin: &str, at: &str) -> Result<(), String> {
    if map["source_revision"] != pin {
        return Err(format!("{at}: source_revision is not the Cargo pin {pin}"));
    }
    if map["normalization_version"] != NORMALIZATION_VERSION {
        return Err(format!(
            "{at}: normalization_version is not {NORMALIZATION_VERSION}"
        ));
    }
    Ok(())
}

pub fn expect_str(
    map: &Map<String, Value>,
    key: &str,
    value: &str,
    at: &str,
) -> Result<(), String> {
    if map[key] != value {
        return Err(format!("{at}: {key} is not {value}"));
    }
    Ok(())
}

pub fn object<'a>(
    value: &'a Value,
    at: &str,
    keys: &[&str],
) -> Result<&'a Map<String, Value>, String> {
    let map = value.as_object().ok_or(format!("{at}: not an object"))?;
    let actual: BTreeSet<&str> = map.keys().map(String::as_str).collect();
    let wanted: BTreeSet<&str> = keys.iter().copied().collect();
    if actual != wanted {
        return Err(format!("{at}: members {actual:?}, expected {wanted:?}"));
    }
    Ok(map)
}

pub fn string_list(value: &Value, at: &str) -> Result<Vec<String>, String> {
    value
        .as_array()
        .ok_or(format!("{at}: not an array"))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or(format!("{at}: not a string array"))
        })
        .collect()
}

pub fn as_usize(value: &Value) -> Result<usize, String> {
    value
        .as_u64()
        .and_then(|v| usize::try_from(v).ok())
        .ok_or_else(|| format!("{value} is not a size"))
}

pub fn json(dir: &Path, file: &str) -> Result<Value, String> {
    let bytes = std::fs::read(dir.join(file)).map_err(|e| format!("{file}: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{file}: {e}"))
}

fn read_text(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Every regular file below `root`, as `/`-separated relative paths.
pub fn walk(root: &Path) -> Result<BTreeSet<String>, String> {
    let mut out = BTreeSet::new();
    let mut pending = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = pending.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            let path = format!("{prefix}{name}");
            if kind.is_dir() {
                pending.push((entry.path(), format!("{path}/")));
            } else if kind.is_file() {
                out.insert(path);
            } else {
                return Err(format!("fixture entry {path} is not a regular file"));
            }
        }
    }
    Ok(out)
}

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn hex(text: &str, length: usize) -> bool {
    text.len() == length && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}
