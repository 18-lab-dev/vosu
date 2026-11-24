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
//! Regenerate with `python tests/adapter_golden/generate_synthetic.py`,
//! which runs this ignored exporter, then G0's writer, and replaces the
//! committed fixture tree.
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
use vosu::server::adapter_capture::schema_from_query;
use vosu_plugin_api::CborValue;

pub const EXPORT_ENV: &str = "VOSU_GOLDEN_SYNTHETIC_EXPORT";
const HOST: &str = "Host: 127.0.0.1:24050";
const WS_KEY: &str = "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==";
/// Where the synthetic chart sits inside the replay `songs` root, and the
/// recorded relative chart location that names it.
const FOLDER: &str = "synthetic";
const FILE: &str = "chart.osu";

/// One synthetic case: an ID, the replay seed and schema, the request and,
/// for `ws`, the frame sequence.
struct Def {
    id: &'static str,
    seed: Seed,
    schema: &'static str,
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

fn defs() -> Vec<Def> {
    let http = |id, seed, schema, request| Def {
        id,
        seed,
        schema,
        conditional: None,
        request,
        steps: None,
    };
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

/// The synthetic chart: the corpus chart, owned by this repository.
fn chart_asset() -> Asset {
    let chart = source::capture(1);
    Asset {
        id: "synthetic-chart".into(),
        slot: "songs".into(),
        relative: format!("{FOLDER}/{FILE}"),
        media_type: "text/plain; charset=utf-8".into(),
        bytes: chart.assets.roots[0].files[0].bytes.clone(),
    }
}

/// A corpus envelope whose recorded current chart location is the relative
/// `synthetic/chart.osu`, resealed as a forged dump would be.
fn seed(seed: Seed, schema: &str) -> Envelope {
    let source = source::capture(usize::from(seed == Seed::Client));
    let index = ["v1", "sc", "v2", "v2-precise"]
        .iter()
        .position(|name| *name == schema)
        .expect("known schema");
    let mut value: Value = serde_json::from_slice(&source.envelopes[index]).unwrap();
    for entry in value["entries"].as_array_mut().unwrap() {
        if entry["ns"] != "vosu.osu" || entry["pointer"] != "/internal/paths" {
            continue;
        }
        let Some(encoded) = entry["value_cbor_b64"].as_str() else {
            continue;
        };
        let bytes = STANDARD.decode(encoded).unwrap();
        let mut paths: CborValue = vosu_plugin_api::decode(&bytes).unwrap();
        let CborValue::Map(fields) = &mut paths else {
            panic!("paths map")
        };
        for (key, current) in fields.iter_mut() {
            if key.as_text() != Some("current") {
                continue;
            }
            let CborValue::Map(current) = current else {
                panic!("current map")
            };
            for (name, slot) in current.iter_mut() {
                match name.as_text() {
                    Some("beatmapFolder") => *slot = CborValue::Text(FOLDER.into()),
                    Some("beatmapFile") => *slot = CborValue::Text(FILE.into()),
                    _ => {}
                }
            }
        }
        entry["value_cbor_b64"] = json!(STANDARD.encode(vosu_plugin_api::encode(&paths).unwrap()));
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

#[test]
#[ignore = "writes the synthetic mechanics capture; run through tests/adapter_golden/generate_synthetic.py"]
fn export_synthetic_capture() {
    let out = std::env::var_os(EXPORT_ENV).unwrap_or_else(|| panic!("set {EXPORT_ENV}"));
    let chart = chart_asset();
    let mut cases = Vec::new();
    let defs = defs();
    // One replay per seed and schema, as the comparator groups cases by
    // envelope, so `header-etag` sees one stable tag per file.
    let mut groups: Vec<(Seed, &str)> = Vec::new();
    for def in &defs {
        if !groups.contains(&(def.seed, def.schema)) {
            groups.push((def.seed, def.schema));
        }
    }
    for (seed_kind, schema_name) in groups {
        let schema = schema_from_query(schema_name).expect("schema");
        let seeded = seed(seed_kind, schema_name);
        let assets = replay_assets(std::slice::from_ref(&chart), &seeded).unwrap();
        let replay = Replay::start(std::slice::from_ref(&seeded), &assets).unwrap();
        // The dump a live harness endpoint would serve for this replay.
        let dump: Value = serde_json::from_slice(&replay.dump_bytes(schema).unwrap()).unwrap();
        for def in defs
            .iter()
            .filter(|d| (d.seed, d.schema) == (seed_kind, schema_name))
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
    let export = json!({
        "cases": cases,
        "assets": [{
            "id": chart.id,
            "slot": chart.slot,
            "relative": chart.relative,
            "media_type": chart.media_type,
            "generator": "adapter_golden synthetic exporter: the state_api_compat stable-play-osu-fl corpus chart",
            "license": "MIT OR Apache-2.0 (vosu-owned test data)",
            "sha256": super::fixture::sha256(&chart.bytes),
            "b64": STANDARD.encode(&chart.bytes),
        }],
    });
    std::fs::write(out, serde_json::to_vec_pretty(&export).unwrap()).unwrap();
}
