//! M5 X0: runtime parity of the real external osu! producer
//! (`docs/v2/implementation-plan.md` M5 exit, "Parity").
//!
//! Every committed source-oracle case (`tests/fixtures/oracle/`, the pinned
//! M4a corpus, read only) is replayed twice through the same production
//! adapters: once in-process exactly as M4a's own suite replays it, and once
//! by the external `osu_process_plugin` library with its real `vosu.osu`
//! manifest in a real `vosu plugin-host` child ([`replay`]). The test
//! compares what consumers actually receive:
//!
//! - the encoded `/json` (v1), `/json/v2`, `/json/v2/precise` and
//!   StreamCompanion bytes of every case;
//! - for one stable and one lazer pair, the encoded API v1 frames: the
//!   session-events document, the field subscription's initial frame, its
//!   update frames after a second committed case is published to the same
//!   session, the HTTP session and state reads, the oracle chart resource
//!   bytes, and the removal frames and close when the session detaches.
//!
//! **Volatile identifiers.** The two runtimes compose the same host in the
//! same order, so session ids (`session-N`), session generations, target
//! PIDs, instance ids and revisions are equal by construction and compared
//! as they are. The legacy encodings carry no volatile value and are
//! compared byte-for-byte without normalization. API v1 carries exactly one
//! kind: a resource or tree URL (`/api/v1/resources/<id>`,
//! `/api/v1/trees/<id>/`) whose id ends in a MAC under the host's own
//! per-run secret, so the same reference has a different id in every host.
//! [`ResourceIds`] maps each run's distinct URLs, in order of first
//! appearance, to `<resource-N>`. It is a bijection per run (it fails if one
//! URL would get two placeholders or two URLs one), so equal normalized
//! frames mean the same references in the same positions. Nothing else is
//! rewritten: data, private fields and errors compare as served. The chart
//! root's temporary directory, the only other per-run value, never reaches
//! a consumer ([`assert_no_chart_root`]).

#[path = "../state_api_compat/bus.rs"]
mod bus;
#[path = "../state_api_compat/oracle.rs"]
#[expect(
    dead_code,
    reason = "the shared source-oracle suite; parity uses the replay, inventory and conversion"
)]
mod oracle;
#[path = "parity/replay.rs"]
mod replay;

use std::collections::{BTreeMap, BTreeSet};

use replay::{Run, Runtime};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::support::api::{Api, Socket, bounded, frame, select};

const SCHEMAS: [&str; 4] = ["v1", "v2", "v2-precise", "sc"];

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The documented per-run bijection from resource and tree URLs to
/// `<resource-N>` placeholders.
#[derive(Default)]
struct ResourceIds {
    forward: BTreeMap<String, String>,
    backward: BTreeMap<String, String>,
}

impl ResourceIds {
    fn normalize(&mut self, value: &Value) -> Value {
        match value {
            Value::String(text)
                if text.starts_with("/api/v1/resources/") || text.starts_with("/api/v1/trees/") =>
            {
                let next = format!("<resource-{}>", self.forward.len() + 1);
                let placeholder = self.forward.entry(text.clone()).or_insert(next).clone();
                let original = self
                    .backward
                    .entry(placeholder.clone())
                    .or_insert_with(|| text.clone());
                assert_eq!(original, text, "two URLs mapped to {placeholder}");
                Value::String(placeholder)
            }
            Value::Array(items) => Value::Array(items.iter().map(|v| self.normalize(v)).collect()),
            Value::Object(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(key, v)| (key.clone(), self.normalize(v)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
}

/// The run's chart root, its one other per-run value, never reaches a
/// consumer.
fn assert_no_chart_root(run: &Run, served: &[u8]) {
    let root = run
        .replay
        .fixture
        .h
        .root
        .path()
        .to_string_lossy()
        .into_owned();
    assert!(
        !String::from_utf8_lossy(served).contains(&root),
        "the chart root reached a consumer"
    );
}

/// The corpus pin and coverage, read from the committed inventory.
fn inventory() -> oracle::Inventory {
    serde_json::from_slice(&std::fs::read(oracle::fixture_root().join("inventory.json")).unwrap())
        .unwrap()
}

/// Every committed case's four encodings are byte-equal across the two
/// runtimes, and every client case committed its own nonzero facts in both.
#[test]
fn parity_osu_oracle_encodings_match_across_runtimes() {
    let inventory = inventory();
    let revisions = &inventory.revisions;
    println!(
        "RECORD parity pin tosu={} lazerCalculator={} osu={} calculatorPackage={} calculatorTree={} inventory_sha256={}",
        revisions.upstream_source,
        revisions.lazer_calculator,
        revisions.osu,
        revisions.calculator_package,
        revisions.calculator_tree,
        sha256(&std::fs::read(oracle::fixture_root().join("inventory.json")).unwrap()),
    );
    let covered: BTreeSet<&str> = inventory
        .cases
        .iter()
        .flat_map(|case| case.tags.iter().map(String::as_str))
        .collect();
    for required in &inventory.required_coverage {
        assert!(
            covered.contains(required.as_str()),
            "required coverage {required} has no case"
        );
    }

    let mut distinct = BTreeMap::new();
    for case in &inventory.cases {
        let input = replay::input(&case.id);
        let in_process = Run::in_process(&input);
        let expected: Vec<Vec<u8>> = SCHEMAS
            .iter()
            .map(|schema| oracle::encode(schema, &in_process.replay))
            .collect();
        let in_process_facts = input
            .source
            .is_some()
            .then(|| bus::published(&in_process.replay, &input));
        drop(in_process);

        let process = Run::process(&input, None);
        for (schema, expected) in SCHEMAS.iter().zip(&expected) {
            let actual = oracle::encode(schema, &process.replay);
            assert_no_chart_root(&process, &actual);
            assert_eq!(
                String::from_utf8_lossy(&actual),
                String::from_utf8_lossy(expected),
                "{} {schema}: the process runtime's bytes differ",
                case.id
            );
            println!(
                "RECORD parity case={} schema={schema} bytes={} sha256={}",
                case.id,
                actual.len(),
                sha256(&actual)
            );
            if *schema == "v2" {
                distinct.insert(case.id.clone(), actual);
            }
        }
        if let Some(facts) = in_process_facts {
            // The child committed this case's own facts, not a default image.
            assert_eq!(
                bus::published(&process.replay, &input),
                facts,
                "{}",
                case.id
            );
        }
        println!(
            "RECORD parity coverage case={} tags={}",
            case.id,
            case.tags.join(",")
        );
    }
    // Nonzero inputs: every client case serves its own document.
    let no_client = distinct["no-client"].clone();
    for (id, bytes) in &distinct {
        if id != "no-client" {
            assert_ne!(bytes, &no_client, "{id} served the no-client document");
        }
    }
}

/// What one consumer saw of one runtime's session lifecycle.
#[derive(Debug, serde::Serialize)]
struct Frames {
    sessions_initial: Value,
    initial: Value,
    session: Value,
    state: Value,
    chart: Option<Vec<u8>>,
    updates: Vec<Value>,
    removal: Value,
    close: Option<u16>,
    sessions_removal: Value,
}

/// Drives one runtime through initial, update and removal and records every
/// frame and read a consumer gets.
fn frames(run: &Run, update: &oracle::Input) -> Frames {
    let api = Api::new(&run.replay.fixture.h.host, replay::BOUND);
    let (_, session) = bus::selected(&run.replay);
    let (mut events, mut socket, sessions_initial, initial) = api.runtime.block_on(async {
        let mut events = api.ws("/sessions/subscribe").await;
        let sessions_initial = select(&mut events, &[""]).await;
        let mut socket = api.ws(&format!("/sessions/{session}/subscribe")).await;
        let initial = select(&mut socket, &[""]).await;
        (events, socket, sessions_initial, initial)
    });
    let session_view = api.json(&format!("/sessions/{session}"));
    let state = api.json(&format!("/sessions/{session}/state"));
    let chart = state
        .pointer("/vosu.chart/resources/file/url")
        .and_then(Value::as_str)
        .map(|url| {
            let url = api.url(url);
            api.runtime.block_on(bounded(async {
                let response = api.client.get(url).send().await.unwrap();
                assert_eq!(response.status().as_u16(), 200);
                response.bytes().await.unwrap().to_vec()
            }))
        });

    run.update(update);
    let updates = api.runtime.block_on(drain(&mut socket));
    assert!(!updates.is_empty(), "the update produced no frame");

    run.replay.fixture.h.host.process_exited(run.selected_pid());
    let (removal, close, sessions_removal) = api.runtime.block_on(async {
        let removal = frame(&mut socket).await;
        let close = closed(&mut socket).await;
        let sessions_removal = frame(&mut events).await;
        (removal, close, sessions_removal)
    });
    Frames {
        sessions_initial,
        initial,
        session: session_view,
        state,
        chart,
        updates,
        removal,
        close,
        sessions_removal,
    }
}

/// Every frame up to a ping fence after the first one: the update's frames
/// in order, however many commits it took.
async fn drain(socket: &mut Socket) -> Vec<Value> {
    use futures_util::{SinkExt as _, StreamExt as _};
    use tokio_tungstenite::tungstenite::Message;
    let mut frames = vec![frame(socket).await];
    bounded(socket.send(Message::Ping(b"x0-fence".to_vec().into())))
        .await
        .unwrap();
    loop {
        match bounded(socket.next()).await.unwrap().unwrap() {
            Message::Text(text) => frames.push(serde_json::from_str(&text).unwrap()),
            Message::Pong(_) => return frames,
            other => panic!("unexpected {other:?}"),
        }
    }
}

/// The close code of a socket the server closes next.
async fn closed(socket: &mut Socket) -> Option<u16> {
    use futures_util::StreamExt as _;
    use tokio_tungstenite::tungstenite::Message;
    loop {
        match bounded(socket.next()).await {
            Some(Ok(Message::Close(frame))) => return frame.map(|frame| u16::from(frame.code)),
            Some(Ok(Message::Pong(_))) => {}
            None => return None,
            other => panic!("expected a close, got {other:?}"),
        }
    }
}

/// One stable and one lazer case pair: initial, update, resource and removal
/// frames are byte-equal across the runtimes.
#[test]
fn parity_osu_api_v1_frames_match_across_runtimes() {
    for (first, second) in [
        ("stable-play-osu-fl", "stable-result-catch-ht"),
        ("lazer-menu-da", "lazer-play-taiko-dt"),
        // The two cases with an oracle chart: its resource bytes, served
        // by the child's own resource routing in the process runtime.
        ("sc-equal-bpm", "sc-range-bpm"),
    ] {
        let input = replay::input(first);
        let update = replay::input(second);
        let in_process = Run::in_process(&input);
        let expected = frames(&in_process, &update);
        assert_eq!(in_process.runtime, Runtime::InProcess);
        drop(in_process);
        let process = Run::process(&input, Some(second));
        assert_eq!(process.runtime, Runtime::Process);
        let actual = frames(&process, &update);
        let actual = serde_json::to_value(&actual).unwrap();
        assert_no_chart_root(&process, actual.to_string().as_bytes());
        let actual = ResourceIds::default().normalize(&actual);
        let expected = ResourceIds::default().normalize(&serde_json::to_value(&expected).unwrap());
        assert_ne!(
            expected["initial"], expected["updates"][0],
            "{first}→{second}: the update changed nothing"
        );
        assert_eq!(expected["close"], 1000, "{first}: detach closes normally");
        if let Some((pointer, left, right)) = first_difference(&actual, &expected, String::new()) {
            panic!(
                "{first}→{second}: API v1 frames differ at {pointer}: process {left} in-process {right}"
            );
        }
        let chart = actual["chart"].as_array().map_or(0, Vec::len);
        println!(
            "RECORD parity api case={first} update={second} update_frames={} chart_bytes={chart}",
            actual["updates"].as_array().unwrap().len(),
        );
        if first.starts_with("sc-") {
            assert!(
                chart > 0,
                "{first}: the oracle chart resource was not served"
            );
        }
    }
}

/// The first JSON pointer at which `left` and `right` differ, with both
/// values there.
fn first_difference(left: &Value, right: &Value, at: String) -> Option<(String, Value, Value)> {
    match (left, right) {
        (Value::Object(l), Value::Object(r)) => {
            let keys: BTreeSet<&String> = l.keys().chain(r.keys()).collect();
            keys.into_iter().find_map(|key| {
                let null = Value::Null;
                first_difference(
                    l.get(key).unwrap_or(&null),
                    r.get(key).unwrap_or(&null),
                    format!("{at}/{key}"),
                )
            })
        }
        (Value::Array(l), Value::Array(r)) if l.len() == r.len() => l
            .iter()
            .zip(r)
            .enumerate()
            .find_map(|(index, (l, r))| first_difference(l, r, format!("{at}/{index}"))),
        _ => (left != right).then(|| (at, left.clone(), right.clone())),
    }
}
