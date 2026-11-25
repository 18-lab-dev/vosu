//! M5 X0, the testkit half of runtime parity: the same services-mode
//! testkit library (`crates/vosu-plugin-testkit/examples/
//! testkit_process_plugin/services.rs`), loaded once in-process and once in
//! a real `vosu plugin-host` child, publishes the same state, resources and
//! API v1 frames to a consumer.
//!
//! Both runs compose the same host in the same order (one instance, one
//! synthetic target, the same plugin id, process name and drives), so
//! session ids, instance ids, target PIDs and revisions are equal by
//! construction and compared as they are. Resource and tree URLs, whose
//! ids end in a MAC under each host's own secret, go through the parent
//! module's documented per-run bijection ([`super::ResourceIds`]); nothing
//! else is rewritten. Resource bodies are compared byte for byte with their
//! status, completeness and content headers, so a stream's truthful EOF and
//! a short stream's failed body must match too. The `files` root's
//! per-run directory never reaches a consumer.

use std::path::Path;

use reqwest::header;
use serde_json::{Value, json};

use super::{ResourceIds, closed, drain, first_difference};
use crate::support::api::{Api, bounded, frame, select};
use crate::support::services::{BOUND, Services, Spec, op};

/// Three chunks, so a full read crosses two chunk boundaries.
const STREAM: u64 = 2 * vosu_plugin_api::VOSU_STREAM_CHUNK_MAX_BYTES as u64 + 4321;
/// The one synthetic target both runs attach.
const TARGET: u32 = 0x7E40_0001;
/// The response headers a consumer's caching and range logic reads.
const HEADERS: [header::HeaderName; 5] = [
    header::CONTENT_TYPE,
    header::CONTENT_LENGTH,
    header::CONTENT_RANGE,
    header::ACCEPT_RANGES,
    header::ETAG,
];

/// One resource request as a consumer sees it.
#[derive(Debug, serde::Serialize)]
struct Served {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    complete: bool,
}

async fn fetch(api: &Api, url: &str, range: Option<&str>) -> Served {
    let mut request = api.client.get(api.url(url));
    if let Some(range) = range {
        request = request.header(header::RANGE, range);
    }
    let mut response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let headers = HEADERS
        .iter()
        .filter_map(|name| {
            let value = response.headers().get(name)?;
            Some((name.to_string(), value.to_str().unwrap().to_owned()))
        })
        .collect();
    let mut body = Vec::new();
    let complete = loop {
        match response.chunk().await {
            Ok(Some(chunk)) => body.extend_from_slice(&chunk),
            Ok(None) => break true,
            Err(_) => break false,
        }
    };
    Served {
        status,
        headers,
        body,
        complete,
    }
}

/// Everything one consumer saw of one runtime's session.
#[derive(Debug, serde::Serialize)]
struct Observed {
    sessions_initial: Value,
    initial: Value,
    session: Value,
    state: Value,
    namespace: Value,
    resources: Vec<(String, Served)>,
    published: Vec<Value>,
    republished: Vec<Value>,
    republished_state: Value,
    removal: Value,
    close: Option<u16>,
    sessions_removal: Value,
}

/// Runs the services testkit under one runtime through attach, every
/// resource, a publication, a republication of its references and the
/// target's exit, recording what the consumer receives.
fn observe(in_process: bool, files: &Path) -> Observed {
    let mut run = Services::empty();
    run.add(&Spec {
        name: "x0.parity",
        root: Some(files.to_owned()),
        stream: STREAM,
        in_process,
        ..Spec::default()
    });
    let plugin = run.plugin(0).plugin_id.clone();
    let api = Api::new(run.host(), BOUND);
    let attach = run.attach(0, TARGET);
    let session = attach.get("session").to_owned();

    let (mut events, mut socket, sessions_initial, initial) = api.runtime.block_on(async {
        let mut events = api.ws("/sessions/subscribe").await;
        let sessions_initial = select(&mut events, &[""]).await;
        let mut socket = api.ws(&format!("/sessions/{session}/subscribe")).await;
        let initial = select(&mut socket, &[""]).await;
        (events, socket, sessions_initial, initial)
    });
    let session_view = api.json(&format!("/sessions/{session}"));
    let state = api.json(&format!("/sessions/{session}/state"));
    let namespace = api.json(&format!("/sessions/{session}/state/{plugin}"));
    let url = |name: &str| {
        namespace["resources"][name]["url"]
            .as_str()
            .unwrap_or_else(|| panic!("no {name} reference: {namespace}"))
            .to_owned()
    };
    let tree = url("tree");
    let tree = tree.trim_end_matches('/');
    let requests = [
        ("file", url("file"), None),
        ("file-range", url("file"), Some("bytes=2-6")),
        ("escape", url("escape"), None),
        ("tree", url("tree"), None),
        ("tree-member", format!("{tree}/beta.osu"), None),
        ("tree-absent", format!("{tree}/gamma.osu"), None),
        ("stream", url("stream"), None),
        ("stream-range", url("stream"), Some("bytes=-7")),
        ("short", url("short"), None),
    ];
    let resources = api.runtime.block_on(bounded(async {
        let mut served = Vec::new();
        for (name, url, range) in &requests {
            served.push(((*name).to_owned(), fetch(&api, url, *range).await));
        }
        served
    }));

    let publish = run.drive(
        0,
        op("publish", json!({"text": "x0-parity-update", "count": 7})),
    );
    assert_eq!(publish.status(), 0);
    let published = api.runtime.block_on(drain(&mut socket));
    let refs = run.drive(0, op("refs", json!({})));
    assert_eq!(refs.status(), 0);
    let republished = api.runtime.block_on(drain(&mut socket));
    let republished_state = api.json(&format!("/sessions/{session}/state/{plugin}"));

    run.host.process_exited(TARGET);
    let (removal, close, sessions_removal) = api.runtime.block_on(async {
        let removal = frame(&mut socket).await;
        let close = closed(&mut socket).await;
        let sessions_removal = frame(&mut events).await;
        (removal, close, sessions_removal)
    });
    run.wait(0, "detach", |_| true);
    Observed {
        sessions_initial,
        initial,
        session: session_view,
        state,
        namespace,
        resources,
        published,
        republished,
        republished_state,
        removal,
        close,
        sessions_removal,
    }
}

/// A `files` root under `base` whose directory name is a sentinel no
/// consumer may receive.
fn root(base: &Path) -> std::path::PathBuf {
    let root = base.join("x0-testkit-parity-root-sentinel");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("alpha.osu"), b"alpha x0 parity 7001\n").unwrap();
    std::fs::write(root.join("beta.osu"), b"beta x0 parity 7002\n").unwrap();
    std::fs::write(base.join("outside.osu"), b"OUTSIDE x0 parity 7003\n").unwrap();
    root
}

/// The services-mode testkit serves equal state, resources and API v1
/// frames in-process and as a process plugin.
#[test]
fn parity_testkit_services_state_resources_and_frames_match_across_runtimes() {
    // One root for both runs: a file's validators derive from its
    // filesystem identity, which two copies would not share.
    let scratch = crate::support::scratch();
    let files = root(scratch.path());
    let expected = observe(true, &files);
    let actual = observe(false, &files);

    let expected = serde_json::to_value(&expected).unwrap();
    let actual = serde_json::to_value(&actual).unwrap();
    for (runtime, observed) in [("in-process", &expected), ("process", &actual)] {
        assert!(
            !observed
                .to_string()
                .contains("x0-testkit-parity-root-sentinel"),
            "{runtime}: the files root {} reached a consumer",
            files.display()
        );
    }
    let expected = ResourceIds::default().normalize(&expected);
    let actual = ResourceIds::default().normalize(&actual);

    // Nonzero, distinct inputs: each step changed what the consumer saw.
    assert_ne!(expected["initial"], expected["published"][0]);
    assert_ne!(expected["published"], expected["republished"]);
    assert_ne!(
        expected["namespace"]["resources"]["file"]["url"],
        expected["republished_state"]["resources"]["file"]["url"],
        "republication kept the old reference"
    );
    assert_eq!(expected["close"], 1000, "detach closes normally");
    let served: Vec<(&str, u64, bool)> = expected["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            (
                entry[0].as_str().unwrap(),
                entry[1]["status"].as_u64().unwrap(),
                entry[1]["complete"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        served,
        [
            ("file", 200, true),
            ("file-range", 206, true),
            ("escape", 502, true),
            ("tree", 200, true),
            ("tree-member", 200, true),
            ("tree-absent", 404, true),
            ("stream", 200, true),
            ("stream-range", 206, true),
            ("short", 200, false),
        ]
    );
    if let Some((pointer, process, in_process)) =
        first_difference(&actual, &expected, String::new())
    {
        panic!("testkit parity differs at {pointer}: process {process} in-process {in_process}");
    }
    let stream = &expected["resources"][6][1]["body"];
    println!(
        "RECORD parity testkit resources={} stream_bytes={} published_frames={} republished_frames={}",
        served.len(),
        stream.as_array().map_or(0, Vec::len),
        expected["published"].as_array().unwrap().len(),
        expected["republished"].as_array().unwrap().len(),
    );
}
