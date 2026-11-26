//! Exporter for the synthetic mechanics capture behind
//! `tests/fixtures/golden-synthetic/`.
//!
//! Every record here is **vosu-generated**: the "upstream" sandwich, the
//! vosu route and the dumps all come from R0 replays of R0's corpus source.
//! The capture exists only so G0's writer (`golden(...,
//! provenance="synthetic")`) can produce a fixture that gives the
//! comparator nonzero coverage before live captures exist. It is never
//! labelled upstream and never counts as pilot or full evidence.
//!
//! Route bytes depend on the OS the replay runs on (upstream `game.focused`
//! is compiled for Windows only), so each synthetic platform is exported,
//! and compared, only on the host OS its name starts with: `windows-stable`
//! on Windows and the lazer-shaped `linux-lazer` (every foreground fact
//! `null`, as a Linux host records) on Linux. Both platforms hold the same
//! case IDs, including two approved charts of one media type that only
//! their recorded locations tell apart.
//!
//! Regenerate with `python tests/adapter_golden/generate_synthetic.py`,
//! which runs this ignored exporter for the local OS, takes the other OS's
//! export, runs G0's writer and replaces the committed fixture tree.
use super::{
    Value,
    compare::replay_assets,
    envelope::{Envelope, selection_digest},
    fixture::Asset,
    json,
    replay::Replay,
    routes::{self, RawResponse},
    source,
    websocket::{self, Frame, Step},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use vosu_core::server::adapter_capture::schema_from_query;
use vosu_plugin_api::CborValue;

pub const EXPORT_ENV: &str = "VOSU_GOLDEN_SYNTHETIC_EXPORT";
const HOST: &str = "Host: 127.0.0.1:24050";
const WS_KEY: &str = "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==";
const TEXT: &str = "text/plain; charset=utf-8";
const LICENSE: &str = "MIT OR Apache-2.0 (vosu-owned test data)";

/// The OS prefix of the synthetic platform names this build replays.
pub fn host_os() -> &'static str {
    if cfg!(windows) { "windows" } else { "linux" }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Platform {
    WindowsStable,
    LinuxLazer,
}
impl Platform {
    const ALL: [Self; 2] = [Self::WindowsStable, Self::LinuxLazer];
    fn name(self) -> &'static str {
        match self {
            Self::WindowsStable => "windows-stable",
            Self::LinuxLazer => "linux-lazer",
        }
    }
    fn lazer(self) -> bool {
        self == Self::LinuxLazer
    }
    /// The variant's chart slot: lazer resolves charts in `files`.
    fn slot(self) -> &'static str {
        if self.lazer() { "files" } else { "songs" }
    }
}

/// Which approved chart a case's recording selects.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Chart {
    First,
    Second,
}
impl Chart {
    /// The recorded `current` `beatmapFolder` and `beatmapFile`. Lazer
    /// records no folder: the osu! plugin resolves its leaf in `files`.
    fn recorded(self, platform: Platform) -> (Option<&'static str>, &'static str) {
        match (platform.lazer(), self) {
            (true, Chart::First) => (None, "chart.osu"),
            (true, Chart::Second) => (None, "second.osu"),
            (false, Chart::First) => (Some("synthetic"), "chart.osu"),
            (false, Chart::Second) => (Some("second"), "chart.osu"),
        }
    }
    /// The approved asset's location in the platform's chart slot, which is
    /// exactly what the recording names.
    fn relative(self, platform: Platform) -> String {
        match self.recorded(platform) {
            (Some(folder), file) => format!("{folder}/{file}"),
            (None, file) => file.to_owned(),
        }
    }
}

/// One synthetic case: an ID, the replay seed, schema and chart, the
/// request and, for `ws`, the frame sequence.
struct Def {
    id: &'static str,
    seed: Seed,
    schema: &'static str,
    chart: Chart,
    conditional: Option<&'static str>,
    request: String,
    steps: Option<Vec<Step>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Seed {
    NoClient,
    Client,
}

fn request(method: &str, target: &str, extra: &[&str]) -> String {
    let mut lines = vec![format!("{method} {target} HTTP/1.1"), HOST.to_owned()];
    lines.extend(extra.iter().map(|line| (*line).to_owned()));
    lines.push("Connection: close".into());
    format!("{}\r\n\r\n", lines.join("\r\n"))
}

fn upgrade(target: &str) -> String {
    format!(
        "GET {target} HTTP/1.1\r\n{HOST}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
         Sec-WebSocket-Version: 13\r\n{WS_KEY}\r\n\r\n"
    )
}

fn http(id: &'static str, seed: Seed, schema: &'static str, request: String) -> Def {
    Def {
        id,
        seed,
        schema,
        chart: Chart::First,
        conditional: None,
        request,
        steps: None,
    }
}

/// The case set every synthetic platform records.
fn defs() -> Vec<Def> {
    use Seed::{Client, NoClient};
    vec![
        http(
            "state-http.get-json-v2-no-client",
            NoClient,
            "v2",
            request("GET", "/json/v2", &[]),
        ),
        http(
            "state-http.post-json-v2",
            NoClient,
            "v2",
            request("POST", "/json/v2", &["Content-Length: 0"]),
        ),
        http(
            "state-http.trailing-slash-json-v2",
            NoClient,
            "v2",
            request("GET", "/json/v2/", &[]),
        ),
        http(
            "origin-peer.foreign-origin-json-v2",
            NoClient,
            "v2",
            request("GET", "/json/v2", &["Origin: http://example.invalid"]),
        ),
        http(
            "path-security.encoded-parent-files-beatmap",
            NoClient,
            "v2",
            request("GET", "/files/beatmap/..%2F..%2Fsentinel.osu", &[]),
        ),
        Def {
            steps: Some(Vec::new()),
            ..http(
                "ws.get-without-upgrade-websocket-v2",
                NoClient,
                "v2",
                request("GET", "/websocket/v2", &[]),
            )
        },
        http(
            "state-http.get-json-v2",
            Client,
            "v2",
            request("GET", "/json/v2", &[]),
        ),
        http(
            "state-http.get-json-sc",
            Client,
            "sc",
            request("GET", "/json/sc", &[]),
        ),
        // The second approved chart has the first one's media type; only
        // its recorded location selects it, and the chart-derived
        // streamCompanion values tell the two apart.
        Def {
            chart: Chart::Second,
            ..http(
                "state-http.get-json-sc-second-chart",
                Client,
                "sc",
                request("GET", "/json/sc", &[]),
            )
        },
        http(
            "calculation.no-query",
            Client,
            "v2",
            request("GET", "/api/calculate/pp", &[]),
        ),
        http(
            "files.get-beatmap-file",
            Client,
            "v2",
            request("GET", "/files/beatmap/file", &[]),
        ),
        http(
            "files.head-beatmap-file",
            Client,
            "v2",
            request("HEAD", "/files/beatmap/file", &[]),
        ),
        Def {
            conditional: Some("etag"),
            ..http(
                "files.if-none-match-beatmap-file",
                Client,
                "v2",
                String::new(),
            )
        },
        Def {
            conditional: Some("last-modified"),
            ..http(
                "files.if-modified-since-beatmap-file",
                Client,
                "v2",
                String::new(),
            )
        },
        Def {
            steps: Some(vec![Step::Receive]),
            ..http(
                "ws.upgrade-websocket-v2",
                Client,
                "v2",
                upgrade("/websocket/v2"),
            )
        },
    ]
}

/// The approved assets: the corpus chart and a second chart of the same
/// media type with another beat length, both owned by this repository.
fn assets(platform: Platform, chart: &[u8]) -> Vec<(Asset, &'static str)> {
    let text = std::str::from_utf8(chart).expect("corpus chart text");
    let second = text.replacen("\n0,320,", "\n0,250,", 1);
    assert_ne!(
        second, text,
        "the corpus chart has its expected timing point"
    );
    let asset = |id: &str, chart: Chart, bytes: Vec<u8>| Asset {
        id: id.into(),
        slot: platform.slot().into(),
        relative: chart.relative(platform),
        media_type: TEXT.into(),
        bytes,
    };
    vec![
        (
            asset("synthetic-chart", Chart::First, chart.to_vec()),
            "adapter_golden synthetic exporter: the state_api_compat stable-play-osu-fl corpus chart",
        ),
        (
            asset("synthetic-second-chart", Chart::Second, second.into_bytes()),
            "adapter_golden synthetic exporter: the corpus chart with a 250 ms beat length",
        ),
    ]
}

/// Records `folder`/`file` as the current chart. A lazer recording carries
/// the `lazer` roots instead of the `stable` ones (left unknown here, so no
/// path is echoed) and no stable skin folder.
fn reshape_paths(value: &mut CborValue, lazer: bool, folder: Option<&str>, file: &str) {
    let CborValue::Map(fields) = value else {
        panic!("paths map")
    };
    for (key, field) in fields.iter_mut() {
        match key.as_text() {
            Some("stable" | "skinFolder") if lazer => *field = CborValue::Null,
            Some("lazer") if lazer => {
                *field = CborValue::Map(vec![
                    (CborValue::Text("data".into()), CborValue::Null),
                    (CborValue::Text("files".into()), CborValue::Null),
                ]);
            }
            Some("current") => {
                let CborValue::Map(current) = field else {
                    panic!("current map")
                };
                for (name, slot) in current.iter_mut() {
                    match name.as_text() {
                        Some("beatmapFolder") => {
                            *slot = folder.map_or(CborValue::Null, |f| CborValue::Text(f.into()));
                        }
                        Some("beatmapFile") => *slot = CborValue::Text(file.into()),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

/// A corpus envelope recording `chart`'s location, resealed as a forged dump
/// would be. A `linux-lazer` seed is a lazer session whose host had no
/// OS-foreground source, so every foreground fact is `null`.
fn seed(platform: Platform, seed: Seed, schema: &str, chart: Chart) -> Envelope {
    let source = source::capture(usize::from(seed == Seed::Client));
    let index = ["v1", "sc", "v2", "v2-precise"]
        .iter()
        .position(|name| *name == schema)
        .expect("known schema");
    let mut value: Value = serde_json::from_slice(&source.envelopes[index]).unwrap();
    let (folder, file) = chart.recorded(platform);
    for entry in value["entries"].as_array_mut().unwrap() {
        if entry["ns"] != "vosu.osu" || entry["pointer"] != "/internal/paths" {
            continue;
        }
        let Some(encoded) = entry["value_cbor_b64"].as_str() else {
            continue;
        };
        let mut paths: CborValue =
            vosu_plugin_api::decode(&STANDARD.decode(encoded).unwrap()).unwrap();
        reshape_paths(&mut paths, platform.lazer(), folder, file);
        entry["value_cbor_b64"] = json!(STANDARD.encode(vosu_plugin_api::encode(&paths).unwrap()));
    }
    if platform.lazer() {
        let lazer = |session: &mut Value| {
            session["variant"] = json!("lazer");
            session["foreground"] = Value::Null;
        };
        value["roster"]["sessions"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .for_each(lazer);
        if !value["session"].is_null() {
            lazer(&mut value["session"]);
        }
    }
    value["digest"] = json!(selection_digest(&value).unwrap());
    Envelope::parse(&serde_json::to_vec(&value).unwrap()).unwrap()
}

fn text_or_b64(bytes: &[u8]) -> Value {
    match std::str::from_utf8(bytes) {
        Ok(text) => json!({"text": text}),
        Err(_) => json!({"b64": STANDARD.encode(bytes)}),
    }
}

fn frame(frame: &Frame) -> Value {
    json!({
        "from": if frame.from_server { "server" } else { "client" },
        "fin": frame.fin,
        "opcode": frame.opcode,
        "masked": frame.masked,
        "mask_key": frame.masked.then_some("37fa213d"),
        "length": frame.payload.len(),
        "payload": text_or_b64(&frame.payload),
    })
}

fn record(request: &str, response: &RawResponse, frames: Option<&[Frame]>) -> Value {
    let mut value = json!({
        "attempt": 1,
        "request": {"text": request},
        "status_line": response.status_line,
        "headers": response.headers.iter().map(|(n, v)| json!([n, v])).collect::<Vec<_>>(),
        "body": text_or_b64(&response.body),
    });
    if let Some(frames) = frames {
        value["frames"] = frames.iter().map(frame).collect();
    }
    value
}

fn exchange(replay: &Replay, def: &Def) -> Value {
    let request = match def.conditional {
        None => def.request.clone(),
        Some(kind) => {
            let earlier = routes::get(replay.address, "/files/beatmap/file");
            let header = if kind == "etag" {
                format!("If-None-Match: {}", earlier.header("etag").expect("etag"))
            } else {
                format!(
                    "If-Modified-Since: {}",
                    earlier.header("last-modified").expect("last-modified")
                )
            };
            request("GET", "/files/beatmap/file", &[&header])
        }
    };
    match &def.steps {
        Some(steps) => {
            let done = websocket::exchange(replay.address, request.as_bytes(), steps).unwrap();
            record(&request, &done.handshake, Some(&done.frames))
        }
        None => record(
            &request,
            &routes::exchange(replay.address, request.as_bytes()).unwrap(),
            None,
        ),
    }
}

fn export(platform: Platform, chart: &[u8]) -> Value {
    let assets = assets(platform, chart);
    let approved: Vec<Asset> = assets.iter().map(|(asset, _)| asset.clone()).collect();
    let defs = defs();
    let mut cases = Vec::new();
    // One replay per seed, schema and chart, as the comparator groups cases
    // by envelope, so `header-etag` sees one stable tag per file.
    let mut groups: Vec<(Seed, &str, Chart)> = Vec::new();
    for def in &defs {
        if !groups.contains(&(def.seed, def.schema, def.chart)) {
            groups.push((def.seed, def.schema, def.chart));
        }
    }
    for (seed_kind, schema_name, chart_kind) in groups {
        let schema = schema_from_query(schema_name).expect("schema");
        let seeded = seed(platform, seed_kind, schema_name, chart_kind);
        let replay_assets = replay_assets(&approved, &seeded).unwrap();
        let replay = Replay::start(std::slice::from_ref(&seeded), &replay_assets).unwrap();
        // The dump a live harness endpoint would serve for this replay.
        let dump: Value = serde_json::from_slice(&replay.dump_bytes(schema).unwrap()).unwrap();
        for def in defs
            .iter()
            .filter(|d| (d.seed, d.schema, d.chart) == (seed_kind, schema_name, chart_kind))
        {
            let records: Vec<Value> = (0..3).map(|_| exchange(&replay, def)).collect();
            let family = def.id.split('.').next().unwrap();
            let barrier = if def.seed == Seed::NoClient {
                "no-client"
            } else {
                "paused"
            };
            cases.push(json!({
                "id": def.id,
                "family": family,
                "barrier": barrier,
                "dump_schema": def.schema,
                "conditional": def.conditional,
                "attempts": [{
                    "attempt": 1,
                    "outcome": "complete",
                    "source": [records[0], records[1]],
                    "vosu": {
                        "dump_before": {"attempt": 1, "envelope": dump},
                        "route": records[2],
                        "dump_after": {"attempt": 1, "envelope": dump},
                    },
                }],
            }));
        }
    }
    cases.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    json!({
        "platform": platform.name(),
        "cases": cases,
        "assets": assets.iter().map(|(asset, generator)| json!({
            "id": asset.id,
            "slot": asset.slot,
            "relative": asset.relative,
            "media_type": asset.media_type,
            "generator": generator,
            "license": LICENSE,
            "sha256": super::fixture::sha256(&asset.bytes),
            "b64": STANDARD.encode(&asset.bytes),
        })).collect::<Vec<_>>(),
    })
}

/// Marks the export printed to standard output.
pub const EXPORT_BEGIN: &str = "=== vosu-golden-synthetic-export begin ===";
pub const EXPORT_END: &str = "=== vosu-golden-synthetic-export end ===";

/// Exports this host's synthetic platforms to `VOSU_GOLDEN_SYNTHETIC_EXPORT`.
/// When that is unset, as in a Windows VM `vm-test` run whose staging
/// directory is removed afterwards, it prints the export as one JSON line
/// between `EXPORT_BEGIN` and `EXPORT_END` (run with `--nocapture`).
#[test]
#[ignore = "writes the synthetic mechanics capture; run through tests/adapter_golden/generate_synthetic.py"]
fn export_synthetic_capture() {
    let chart = source::capture(1).assets.roots[0].files[0].bytes.clone();
    let platforms: Vec<Value> = Platform::ALL
        .into_iter()
        .filter(|platform| platform.name().starts_with(&format!("{}-", host_os())))
        .map(|platform| export(platform, &chart))
        .collect();
    assert!(
        !platforms.is_empty(),
        "no synthetic platform for {}",
        host_os()
    );
    let export = json!({"host": host_os(), "platforms": platforms});
    match std::env::var_os(EXPORT_ENV) {
        Some(out) => std::fs::write(out, serde_json::to_vec_pretty(&export).unwrap()).unwrap(),
        None => println!("{EXPORT_BEGIN}\n{export}\n{EXPORT_END}"),
    }
}
