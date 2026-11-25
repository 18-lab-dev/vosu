//! M5 I0: resources published by a real `vosu plugin-host` child, served
//! through core's existing resource services and the production API v1
//! router (`docs/v2/plugin-abi.md` §6.5; `docs/v2/api-v1.md` resources):
//! a declared-root file, a tree index and member, and a generated stream
//! read by arbitrary ranges, `HEAD` and to a truthful EOF in chunks no
//! larger than one frame. Each stream is a request-owned lease closed
//! exactly once; root confinement, re-declaration and republication
//! retire stale authority; and a child's death ends a waiting body before
//! its next generation serves the same reference again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use reqwest::{Method, StatusCode, header};
use serde_json::{Value, json};
use vosu_core::plugin::process::supervisor::ProcessPhase;
use vosu_plugin_api::VOSU_STREAM_CHUNK_MAX_BYTES;

use super::support::api::{Api, bounded};
use super::support::host::{Identity, wait_until};
use super::support::services::{BOUND, Services, Spec, generated, kill, op};

const CHUNK: u64 = VOSU_STREAM_CHUNK_MAX_BYTES as u64;
/// Three chunks: every full read crosses two chunk boundaries.
const SMALL: u64 = 2 * CHUNK + 4321;
/// Larger than any loopback socket buffering, so a body that is not read
/// leaves most of the stream unrequested.
const LARGE: u64 = 12 * CHUNK + 4321;
const ALPHA: &[u8] = b"alpha root-one 6113\n";
const BETA: &[u8] = b"beta root-one 6114\n";
const OUTSIDE: &[u8] = b"OUTSIDE-THE-ROOT 6115\n";

fn bytes(range: std::ops::Range<u64>) -> Vec<u8> {
    range.map(generated).collect()
}

/// A `files` root whose directory name is a sentinel no API response may
/// contain, with `outside.osu` beside it (outside the root).
fn root(base: &Path, name: &str, alpha: &[u8]) -> PathBuf {
    let root = base.join(format!("i0-private-root-sentinel-{name}"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("alpha.osu"), alpha).unwrap();
    std::fs::write(root.join("beta.osu"), BETA).unwrap();
    std::fs::write(base.join("outside.osu"), OUTSIDE).unwrap();
    root
}

/// One services owner with a root and a generated stream, attached to
/// `target`; returns the session id.
fn owner(name: &'static str, stream: u64, target: u32) -> (Services, PathBuf, String) {
    let mut run = Services::empty();
    let files = root(run.root.path(), name, ALPHA);
    run.add(&Spec {
        name,
        root: Some(files.clone()),
        stream,
        ..Spec::default()
    });
    let session = run.attach(0, target).get("session").to_owned();
    (run, files, session)
}

/// The session's resource URLs, by name.
fn urls(run: &Services, api: &Api, session: &str) -> HashMap<String, String> {
    let document = api.json(&format!(
        "/sessions/{session}/state/{}",
        run.plugin(0).plugin_id
    ));
    document["resources"]
        .as_object()
        .unwrap_or_else(|| panic!("no resources in {document}"))
        .iter()
        .map(|(name, reference)| {
            let url = reference["url"]
                .as_str()
                .unwrap_or_else(|| panic!("{name} has no url: {document}"));
            (name.clone(), url.to_owned())
        })
        .collect()
}

/// One request: status, headers and the whole body (or the bytes that
/// arrived before the body failed, and `false`).
async fn fetch(api: &Api, method: Method, url: &str, range: Option<&str>) -> Fetched {
    let mut request = api.client.request(method, api.url(url));
    if let Some(range) = range {
        request = request.header(header::RANGE, range);
    }
    let mut response = request.send().await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let mut body = Vec::new();
    let complete = loop {
        match response.chunk().await {
            Ok(Some(chunk)) => body.extend_from_slice(&chunk),
            Ok(None) => break true,
            Err(_) => break false,
        }
    };
    Fetched {
        status,
        headers,
        body,
        complete,
    }
}

struct Fetched {
    status: StatusCode,
    headers: reqwest::header::HeaderMap,
    body: Vec<u8>,
    complete: bool,
}

impl Fetched {
    fn text(&self) -> String {
        let headers: String = self
            .headers
            .iter()
            .map(|(name, value)| format!("{name}: {}\n", String::from_utf8_lossy(value.as_bytes())))
            .collect();
        format!("{headers}\n{}", String::from_utf8_lossy(&self.body))
    }
}

/// `(stream, offset, len)` of every read the plugin answered.
fn reads(run: &Services) -> Vec<(u64, u64, u64)> {
    run.plugin(0)
        .fixture
        .receipts("read")
        .iter()
        .map(|read| {
            (
                read.get("stream").parse().unwrap(),
                read.get("offset").parse().unwrap(),
                read.get("len").parse().unwrap(),
            )
        })
        .collect()
}

/// Waits until every stream lease the plugin resolved was closed, then
/// asserts each was closed exactly once and, in the child's own journal
/// order, never read after its close. Returns the streams whose chunk
/// core refused: a read answered after its request had already ended.
fn assert_closed_exactly_once(run: &Services) -> Vec<String> {
    let fixture = &run.plugin(0).fixture;
    let opened = || -> Vec<String> {
        fixture
            .receipts("resolve")
            .iter()
            .map(|resolve| resolve.get("stream").to_owned())
            .filter(|stream| stream != "0")
            .collect()
    };
    assert!(
        wait_until(BOUND, || fixture.receipts("close").len() >= opened().len()),
        "leases {:?} were not all closed: {:?}",
        opened(),
        fixture.receipts("close")
    );
    let mut closed: Vec<String> = fixture
        .receipts("close")
        .iter()
        .map(|close| {
            assert_eq!(close.get("known"), "true", "{close:?}");
            close.get("stream").to_owned()
        })
        .collect();
    closed.sort();
    let mut expected = opened();
    expected.sort();
    assert_eq!(closed, expected, "each lease closed exactly once");
    let mut ended = std::collections::HashSet::new();
    for receipt in fixture.journal(run.child(0)) {
        match receipt.probe.as_str() {
            "close" => {
                ended.insert(receipt.get("stream").to_owned());
            }
            "read" => assert!(
                !ended.contains(receipt.get("stream")),
                "a read after its lease closed: {receipt:?}"
            ),
            _ => {}
        }
    }
    fixture
        .receipts("chunk_refused")
        .iter()
        .map(|refused| refused.get("stream").to_owned())
        .collect()
}

/// Files, a tree and a generated stream from a real child: full bodies,
/// ranges across chunk boundaries, `HEAD`, a short stream ending in a
/// failed body rather than invented bytes, a reply escaping the root
/// refused, every lease closed once, and the root's path never in any
/// response, before or after core's cached resolution.
#[test]
fn resources_process_file_tree_and_stream_serve_ranges_head_and_truthful_eof() {
    let (run, files, session) = owner("res.serve", SMALL, 0x7E20_0001);
    let sentinel = files.file_name().unwrap().to_str().unwrap().to_owned();
    let api = Api::new(run.host(), Duration::from_secs(5));
    let urls = urls(&run, &api, &session);
    let mut seen = Vec::new();
    api.runtime.block_on(bounded(async {
        for _ in 0..2 {
            let file = fetch(&api, Method::GET, &urls["file"], None).await;
            assert_eq!(file.status, StatusCode::OK, "{}", file.text());
            assert_eq!(file.body, ALPHA);
            assert_eq!(
                file.headers[header::CONTENT_TYPE],
                "text/plain; charset=utf-8"
            );
            let range = fetch(&api, Method::GET, &urls["file"], Some("bytes=2-6")).await;
            assert_eq!(range.status, StatusCode::PARTIAL_CONTENT);
            assert_eq!(range.body, &ALPHA[2..7]);
            let head = fetch(&api, Method::HEAD, &urls["file"], None).await;
            assert_eq!(head.status, StatusCode::OK);
            assert_eq!(
                head.headers[header::CONTENT_LENGTH],
                ALPHA.len().to_string().as_str()
            );
            assert!(head.body.is_empty());
            seen.extend([file.text(), range.text(), head.text()]);
        }

        let index = fetch(&api, Method::GET, &urls["tree"], None).await;
        assert_eq!(index.status, StatusCode::OK, "{}", index.text());
        let listed: Value = serde_json::from_slice(&index.body).unwrap();
        let paths: Vec<&str> = listed["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["path"].as_str().unwrap())
            .collect();
        assert_eq!(paths, ["alpha.osu", "beta.osu"], "{listed}");
        let tree = urls["tree"].trim_end_matches('/');
        let member = fetch(&api, Method::GET, &format!("{tree}/beta.osu"), None).await;
        assert_eq!(member.status, StatusCode::OK, "{}", member.text());
        assert_eq!(member.body, BETA);
        let absent = fetch(&api, Method::GET, &format!("{tree}/gamma.osu"), None).await;
        assert_eq!(absent.status, StatusCode::NOT_FOUND, "{}", absent.text());
        seen.extend([index.text(), member.text(), absent.text()]);

        let full = fetch(&api, Method::GET, &urls["stream"], None).await;
        assert_eq!(full.status, StatusCode::OK, "{}", full.text());
        assert!(full.complete);
        assert_eq!(
            full.headers[header::CONTENT_LENGTH],
            SMALL.to_string().as_str()
        );
        assert!(
            full.body == bytes(0..SMALL),
            "the generated bytes, in order"
        );
        for (range, expected) in [
            (
                format!("bytes={}-{}", CHUNK - 3, CHUNK + 2),
                CHUNK - 3..CHUNK + 3,
            ),
            ("bytes=-7".to_owned(), SMALL - 7..SMALL),
            (format!("bytes={}-", SMALL - 4), SMALL - 4..SMALL),
            (
                format!("bytes={}-{}", 2 * CHUNK - 1, 2 * CHUNK),
                2 * CHUNK - 1..2 * CHUNK + 1,
            ),
        ] {
            let part = fetch(&api, Method::GET, &urls["stream"], Some(&range)).await;
            assert_eq!(part.status, StatusCode::PARTIAL_CONTENT, "{range}");
            assert_eq!(part.body, bytes(expected), "{range}");
        }
        let head = fetch(&api, Method::HEAD, &urls["stream"], None).await;
        assert_eq!(head.status, StatusCode::OK);
        assert_eq!(
            head.headers[header::CONTENT_LENGTH],
            SMALL.to_string().as_str()
        );
        assert!(head.body.is_empty());

        let short = fetch(&api, Method::GET, &urls["short"], None).await;
        assert_eq!(short.status, StatusCode::OK);
        assert!(!short.complete, "a stream ending early must fail its body");
        assert!(
            short.body.len() as u64 <= SMALL / 2,
            "bytes past the plugin's EOF"
        );
        assert!(
            short.body == bytes(0..short.body.len() as u64),
            "no invented bytes"
        );

        let escape = fetch(&api, Method::GET, &urls["escape"], None).await;
        assert_eq!(escape.status, StatusCode::BAD_GATEWAY, "{}", escape.text());
        assert!(
            !escape
                .body
                .windows(OUTSIDE.len())
                .any(|window| window == OUTSIDE)
        );
        seen.extend([head.text(), escape.text()]);
    }));
    for read in reads(&run) {
        assert!(read.2 <= CHUNK, "a read past one chunk: {read:?}");
    }
    assert_eq!(
        assert_closed_exactly_once(&run),
        Vec::<String>::new(),
        "no request ended while its read was in flight"
    );
    let document = api.get(&format!("/sessions/{session}/state")).1;
    seen.push(document);
    for text in &seen {
        assert!(!text.contains(&sentinel), "the root path leaked: {text}");
    }
}

/// Two concurrent requests for one generated stream hold two leases
/// under ring pressure (one-megabyte chunks through both directions):
/// disconnecting one closes only its own lease, the other still reads to
/// its end with the right bytes, and a body held unread leaves most of
/// the stream unrequested.
#[test]
fn resources_process_concurrent_streams_close_only_their_own_lease() {
    let (run, _files, session) = owner("res.leases", LARGE, 0x7E20_0002);
    let api = Api::new(run.host(), Duration::from_secs(5));
    let url = api.url(&urls(&run, &api, &session)["stream"]);
    let fixture = &run.plugin(0).fixture;
    let (dropped, kept, first) = api.runtime.block_on(bounded(async {
        let mut dropped = api.client.get(&url).send().await.unwrap();
        let mut kept = api.client.get(&url).send().await.unwrap();
        assert_eq!(
            (dropped.status(), kept.status()),
            (StatusCode::OK, StatusCode::OK)
        );
        let mut first = Vec::new();
        while (first.len() as u64) < CHUNK {
            first.extend_from_slice(&kept.chunk().await.unwrap().unwrap());
        }
        dropped.chunk().await.unwrap().unwrap();
        (dropped, kept, first)
    }));
    assert!(wait_until(BOUND, || fixture.receipts("resolve").len() == 2));
    drop(dropped);
    let closed = run.wait(0, "close", |_| true);
    let streams: Vec<String> = fixture
        .receipts("resolve")
        .iter()
        .map(|resolve| resolve.get("stream").to_owned())
        .collect();
    let kept_stream: u64 = streams
        .iter()
        .find(|stream| **stream != closed.get("stream"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        fixture.receipts("close").len(),
        1,
        "only the dropped lease closed"
    );
    let held_reads = reads(&run)
        .iter()
        .filter(|read| read.0 == kept_stream)
        .count() as u64;
    assert!(
        held_reads < LARGE.div_ceil(CHUNK),
        "an unread body requested all {held_reads} chunks: buffered whole"
    );

    let rest = api.runtime.block_on(bounded(async {
        let mut kept = kept;
        let mut rest = Vec::new();
        while let Some(chunk) = kept.chunk().await.unwrap() {
            rest.extend_from_slice(&chunk);
        }
        rest
    }));
    let mut body = first;
    body.extend_from_slice(&rest);
    assert!(
        body.len() as u64 == LARGE && body == bytes(0..LARGE),
        "the kept body is whole"
    );
    let refused = assert_closed_exactly_once(&run);
    assert!(
        refused.iter().all(|stream| *stream == closed.get("stream")),
        "only the disconnected request's late chunk may be refused: {refused:?}"
    );
    let kept_reads: Vec<_> = reads(&run)
        .into_iter()
        .filter(|read| read.0 == kept_stream)
        .collect();
    assert!(kept_reads.iter().all(|read| read.2 <= CHUNK));
    assert_eq!(
        kept_reads
            .iter()
            .map(|read| read.2.min(LARGE.saturating_sub(read.1)))
            .sum::<u64>(),
        LARGE,
        "each byte requested once, never the whole resource at once"
    );
}

/// Root confinement and current authority: a reference resolves against
/// the root as currently declared (a re-declared root never serves the
/// old directory's bytes), republication retires the old URL with 410,
/// and detach retires the new one.
#[test]
fn resources_process_redeclared_root_and_republished_refs_retire_stale_authority() {
    let (run, files, session) = owner("res.authority", SMALL, 0x7E20_0003);
    let second = files.parent().unwrap().join("second-root");
    std::fs::create_dir_all(&second).unwrap();
    std::fs::write(second.join("alpha.osu"), b"alpha root-two 6116\n").unwrap();
    std::fs::write(second.join("beta.osu"), BETA).unwrap();
    let api = Api::new(run.host(), Duration::from_secs(5));
    let before = urls(&run, &api, &session);
    let get = |url: &str| {
        api.runtime
            .block_on(bounded(fetch(&api, Method::GET, url, None)))
    };
    assert_eq!(get(&before["file"]).body, ALPHA);
    assert_eq!(get(&before["file"]).body, ALPHA, "a cached resolution");

    assert_eq!(
        run.drive(0, op("declare", json!({"path": second.to_str().unwrap()})))
            .status(),
        0
    );
    let redeclared = get(&before["file"]);
    assert_eq!(redeclared.status, StatusCode::OK, "{}", redeclared.text());
    assert_eq!(
        redeclared.body, b"alpha root-two 6116\n",
        "never the old root's bytes"
    );
    let tree = before["tree"].trim_end_matches('/');
    assert_eq!(
        get(&format!("{tree}/alpha.osu")).body,
        b"alpha root-two 6116\n"
    );
    let escape = get(&before["escape"]);
    assert_eq!(escape.status, StatusCode::BAD_GATEWAY, "{}", escape.text());

    assert_eq!(run.drive(0, op("refs", json!({}))).status(), 0);
    let after = urls(&run, &api, &session);
    assert_ne!(
        after["file"], before["file"],
        "a new content id is a new reference"
    );
    let retired = get(&before["file"]);
    assert_eq!(retired.status, StatusCode::GONE, "{}", retired.text());
    assert_eq!(get(&after["file"]).body, b"alpha root-two 6116\n");

    run.host.process_exited(0x7E20_0003);
    run.wait(0, "detach", |_| true);
    let detached = get(&after["file"]);
    assert_eq!(detached.status, StatusCode::GONE, "{}", detached.text());
    assert!(assert_closed_exactly_once(&run).is_empty());
}

/// A child killed while a client waits on its stream body: the body ends
/// in an error promptly instead of hanging, the lost generation's lease is
/// gone, and the next generation, attached to the same target and
/// republishing the same reference, serves the same URL from its own
/// process.
#[test]
fn resources_process_child_death_ends_a_waiting_body_and_the_restart_serves_it() {
    let target = 0x7E20_0004;
    let (run, _files, session) = owner("res.death", LARGE, target);
    let api = Api::new(run.host(), Duration::from_secs(5));
    let url = urls(&run, &api, &session)["stream"].clone();
    let child = run.child(0);
    let identity = Identity::of(child);
    let mut body = api.runtime.block_on(bounded(async {
        let mut body = api.client.get(api.url(&url)).send().await.unwrap();
        assert_eq!(body.status(), StatusCode::OK);
        body.chunk().await.unwrap().unwrap();
        body
    }));
    kill(child);
    let killed = Instant::now();
    let ended = api.runtime.block_on(bounded(async {
        loop {
            match body.chunk().await {
                Ok(Some(_)) => {}
                Ok(None) => break false,
                Err(_) => break true,
            }
        }
    }));
    assert!(
        ended,
        "a lost child's body must fail, not end as if complete"
    );
    println!(
        "RECORD waiting body ended {:?} after the kill",
        killed.elapsed()
    );
    assert!(
        wait_until(BOUND, || !identity.alive()),
        "the child was never reaped"
    );
    // The lost generation was the reference's only owner.
    assert!(
        wait_until(BOUND, || {
            api.runtime
                .block_on(bounded(fetch(&api, Method::GET, &url, None)))
                .status
                == StatusCode::GONE
        }),
        "the lost owner's reference still served"
    );

    // Discovery re-offers tracked processes once the restarted instance's
    // matcher is registered again; this host has no discovery service.
    assert!(
        wait_until(BOUND, || run
            .host()
            .process_status(run.instance(0))
            .is_some_and(
                |status| status.phase == ProcessPhase::Running && status.restarts == 1
            )),
        "the instance never restarted"
    );
    run.host.offer_process(
        target,
        &run.plugin(0).process,
        vosu_plugin_api::Bitness::X64,
    );
    let next = run.wait(0, "attach", |attach| attach.pid != child);
    assert_eq!(next.get("target"), target.to_string());
    let session = next.get("session").to_owned();
    let again = urls(&run, &api, &session)["stream"].clone();
    assert_eq!(again, url, "the same reference identity across generations");
    let served = api
        .runtime
        .block_on(bounded(fetch(&api, Method::GET, &url, None)));
    assert_eq!(served.status, StatusCode::OK, "{}", served.text());
    assert!(served.complete && served.body == bytes(0..LARGE));
    let new_reads = run
        .plugin(0)
        .fixture
        .receipts("read")
        .iter()
        .filter(|read| read.pid == next.pid)
        .count();
    assert!(new_reads > 0, "the next generation served the body");
}
