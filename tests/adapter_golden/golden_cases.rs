//! The pinned golden run, and comparator self-tests over the committed
//! synthetic mechanics fixture.
//!
//! `golden_fixtures_replay_at_the_cargo_pin` is the M4b evidence test. It
//! reads only `tests/fixtures/upstream/<Cargo pin>/`, replays every case and
//! fails on any mismatch or unused divergence rule. Until the pilot commits
//! fixtures, it passes only because `tests/fixtures/upstream/PENDING_PILOT`
//! names exactly the Cargo pin, and it says loudly that it compared nothing.
//! Every other test here runs the same comparator over
//! `tests/fixtures/golden-synthetic/`: vosu-generated mechanics input that is
//! never pilot or full evidence.
use super::{
    Value,
    compare::{self, Mismatch, Observed},
    divergence::{self, LedgerRow, Rule},
    fixture::{self, Body, Case, Fixture, Pinned, Platform, Provenance, repo_root},
    json,
    replay::Replay,
    routes, synthetic,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::LazyLock,
    time::Duration,
};

pub const SYNTHETIC_DIR: &str = "tests/fixtures/golden-synthetic";

pub fn pin() -> String {
    fixture::pin(&repo_root()).unwrap_or_else(|e| panic!("{e}"))
}
pub fn synthetic_dir() -> PathBuf {
    repo_root().join(SYNTHETIC_DIR)
}

pub static SYNTHETIC: LazyLock<Fixture> = LazyLock::new(|| {
    fixture::load(&synthetic_dir(), &pin(), Provenance::Synthetic)
        .unwrap_or_else(|e| panic!("synthetic fixture: {e}"))
});
/// One replay of this host's primary synthetic platform, shared by the
/// comparator tests.
static OBSERVED: LazyLock<Vec<Result<Observed, String>>> =
    LazyLock::new(|| compare::observe(platform()).unwrap_or_else(|e| panic!("{e}")));

/// The synthetic platforms this host replays: route bytes depend on the
/// replaying OS, so only platforms named for it compare exactly here.
fn host_platforms() -> Vec<&'static Platform> {
    let prefix = format!("{}-", synthetic::host_os());
    let platforms: Vec<_> = SYNTHETIC
        .platforms
        .iter()
        .filter(|platform| platform.name.starts_with(&prefix))
        .collect();
    assert!(
        !platforms.is_empty(),
        "no synthetic platform replays on {}: this OS would have zero comparator coverage",
        synthetic::host_os()
    );
    platforms
}
fn platform() -> &'static Platform {
    host_platforms()[0]
}
fn ledger() -> BTreeMap<String, LedgerRow> {
    let path = repo_root().join("tests/fixtures/upstream/DIVERGENCES.md");
    divergence::ledger(&std::fs::read_to_string(path).unwrap()).unwrap()
}
fn case_mut<'a>(platform: &'a mut Platform, id: &str) -> &'a mut Case {
    platform
        .cases
        .iter_mut()
        .find(|case| case.id == id)
        .unwrap_or_else(|| panic!("no synthetic case {id}"))
}
fn set_header(case: &mut Case, name: &str, value: &str) {
    let header = case
        .expected
        .headers
        .iter_mut()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("{}: no {name}", case.id));
    header.1 = value.to_owned();
}
fn text_mut(case: &mut Case) -> &mut String {
    let Body::Text(text) = &mut case.expected.body else {
        panic!("{}: binary body", case.id)
    };
    text
}
/// The (case, part) pairs the comparator reports.
fn mismatches(platform: &Platform, rules: &[Rule]) -> Vec<(String, String)> {
    compare::compare(platform, &OBSERVED, rules)
        .mismatches
        .into_iter()
        .map(|Mismatch { case, part, .. }| (case, part))
        .collect()
}
fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(c, p)| ((*c).to_owned(), (*p).to_owned()))
        .collect()
}
fn with(platform: Platform) -> Fixture {
    Fixture {
        platforms: vec![platform],
        ..SYNTHETIC.clone()
    }
}
fn rules(fixture: &Fixture, list: Vec<Value>) -> Result<Vec<Rule>, String> {
    divergence::rules(
        Some(&json!({"format": divergence::FORMAT, "rules": list})),
        &ledger(),
        fixture,
    )
}

#[test]
fn golden_fixtures_replay_at_the_cargo_pin() {
    let repo = repo_root();
    let pin = pin();
    match fixture::discover(&repo.join("tests/fixtures/upstream"), &pin) {
        Err(error) => panic!("{error}"),
        Ok(Pinned::Pending { reason }) => eprintln!(
            "\n=== adapter_golden: NO GOLDEN COVERAGE for pin {pin} ===\n{reason}\nZero \
             upstream cases were compared. This run is not pilot or full evidence.\n"
        ),
        Ok(Pinned::Present(fixture)) => {
            let rules = divergence::rules(fixture.rules.as_ref(), &ledger(), &fixture)
                .unwrap_or_else(|e| panic!("{e}"));
            let counts = compare::run(&fixture.platforms, &rules)
                .unwrap_or_else(|e| panic!("golden mismatches at pin {pin}:\n{e}"));
            for (platform, families) in counts {
                eprintln!("adapter_golden {pin} {platform}: {families:?}");
            }
        }
    }
}

#[test]
fn golden_synthetic_fixture_compares_exactly_with_nonzero_coverage() {
    assert_eq!(SYNTHETIC.provenance, Provenance::Synthetic);
    for platform in host_platforms() {
        let counts = platform.counts();
        assert!(counts.values().all(|count| *count > 0), "{counts:?}");
        let observed = if platform.name == self::platform().name {
            OBSERVED.clone()
        } else {
            compare::observe(platform).unwrap_or_else(|e| panic!("{e}"))
        };
        let report = compare::compare(platform, &observed, &[]);
        assert_eq!(report.mismatches, Vec::<Mismatch>::new());
        assert!(report.used.is_empty());
        let normalized = |rule: &str| {
            platform
                .cases
                .iter()
                .filter(|case| case.normalized.iter().any(|r| r == rule))
                .count()
        };
        // Every §7 header rule the synthetic set can exercise is exercised.
        assert_eq!(normalized("header-date"), platform.cases.len());
        assert!(normalized("header-etag") >= 4);
        assert!(normalized("header-last-modified") >= 4);
        let conditional: BTreeSet<_> = platform
            .cases
            .iter()
            .filter_map(|case| case.conditional.as_deref())
            .collect();
        assert_eq!(conditional, BTreeSet::from(["etag", "last-modified"]));
        let frames: usize = platform
            .cases
            .iter()
            .filter_map(|case| case.expected.frames.as_ref())
            .map(Vec::len)
            .sum();
        assert!(frames > 0);
        eprintln!(
            "adapter_golden synthetic mechanics fixture (not evidence) {}: {} cases {counts:?}",
            platform.name,
            platform.cases.len()
        );
    }
}

/// Every synthetic platform, whichever OS replays it, records the
/// foreground facts of its own host: `true`/`false` from a Windows host and
/// `null` from the lazer-shaped Linux host. Both hold the same case IDs, so
/// each OS's comparator tests see the same cases.
#[test]
fn golden_synthetic_platforms_record_their_host_foreground_facts() {
    let mut ids = Vec::new();
    for platform in &SYNTHETIC.platforms {
        let mut sessions = 0;
        for case in &platform.cases {
            for session in case.replay.roster.iter().chain(&case.replay.session) {
                if platform.name == "linux-lazer" {
                    assert_eq!(
                        (session.variant.as_str(), session.foreground),
                        ("lazer", None)
                    );
                } else {
                    assert!(session.foreground.is_some(), "{}", platform.name);
                }
                sessions += 1;
            }
        }
        assert!(sessions > 0, "{}: no client case", platform.name);
        ids.push(
            platform
                .cases
                .iter()
                .map(|c| c.id.as_str())
                .collect::<Vec<_>>(),
        );
    }
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], ids[1]);
}

/// Two approved charts share one media type. Each recorded location selects
/// its own chart: the streamCompanion case of the second chart replays to
/// different, still exact, chart values.
#[test]
fn golden_two_assets_of_one_media_type_follow_their_recorded_location() {
    let platform = platform();
    let types: Vec<_> = platform
        .assets
        .iter()
        .map(|a| a.media_type.as_str())
        .collect();
    assert_eq!(types.len(), 2);
    assert_eq!(types[0], types[1]);
    let body = |id: &str| {
        let case = platform.cases.iter().find(|c| c.id == id).unwrap();
        let assets = compare::replay_assets(&platform.assets, &case.replay).unwrap();
        assert_eq!(assets.resources.len(), 1, "{id}");
        (assets.resources[0].path.clone(), case.expected.body.clone())
    };
    let (first, first_body) = body("state-http.get-json-sc");
    let (second, second_body) = body("state-http.get-json-sc-second-chart");
    let approved = |id: &str| platform.asset(id).unwrap().relative.clone();
    assert_eq!(first, approved("synthetic-chart"));
    assert_eq!(second, approved("synthetic-second-chart"));
    assert_ne!(first, second);
    assert_ne!(first_body, second_body);
    let report = compare::compare(platform, &OBSERVED, &[]);
    assert_eq!(report.mismatches, Vec::<Mismatch>::new());
}

#[test]
fn golden_last_modified_must_equal_the_declared_replay_file_mtime() {
    const ID: &str = "files.get-beatmap-file";
    let case = platform().cases.iter().find(|c| c.id == ID).unwrap();
    let mut assets = compare::replay_assets(&platform().assets, &case.replay).unwrap();
    let shifted = compare::replay_mtime() + Duration::from_secs(86_400);
    for file in assets
        .roots
        .iter_mut()
        .flat_map(|root| root.files.iter_mut())
    {
        assert_eq!(file.modified, Some(compare::replay_mtime()));
        file.modified = Some(shifted);
    }
    let replay = Replay::start(std::slice::from_ref(&case.replay), &assets).unwrap();
    let response = routes::exchange(replay.address, case.request.as_bytes()).unwrap();
    // The real file mtime reaches the route.
    let served = httpdate::fmt_http_date(shifted);
    assert_eq!(response.header("last-modified"), Some(served.as_str()));
    drop(replay);
    let observed = compare::normalize(response, Vec::new(), true, &[], Vec::new(), Vec::new());
    assert!(
        observed.invariants.iter().any(|(part, detail)| {
            part == "headers.last-modified" && detail.contains("declared replay file mtime")
        }),
        "{:?}",
        observed.invariants
    );
    // The placeholder alone would have hidden it; the comparator does not.
    // The shifted replay's file identity also changed its ETag, which is a
    // different invariant, so the original tag is kept here.
    let index = platform().cases.iter().position(|c| c.id == ID).unwrap();
    let mut all = OBSERVED.clone();
    let original = all[index].as_ref().unwrap().headers.clone();
    let mut observed = observed;
    for (name, value) in &mut observed.headers {
        if name == "etag" {
            value.clone_from(&original.iter().find(|(n, _)| n == "etag").unwrap().1);
        }
    }
    all[index] = Ok(observed);
    let report = compare::compare(platform(), &all, &[]);
    let found: Vec<_> = report
        .mismatches
        .iter()
        .map(|m| (m.case.as_str(), m.part.as_str()))
        .collect();
    assert_eq!(found, [(ID, "headers.last-modified")]);
}

#[test]
fn golden_root_prefix_rewrites_only_replay_roots_and_recomputes_length() {
    let roots = [
        (
            "/tmp/.tmpA1/songs/".to_owned(),
            "/vosu-golden/songs/".to_owned(),
        ),
        (
            r"C:\Temp\.tmpB2\files\".to_owned(),
            r"C:\vosu-golden\files\".to_owned(),
        ),
    ];
    let body = r#"{"a":"/tmp/.tmpA1/songs/x.osu","b":"C:\\Temp\\.tmpB2\\files\\y","c":"/tmp/.tmpA1/songsx","n":0}"#;
    let response = routes::RawResponse {
        status_line: "HTTP/1.1 200 OK".into(),
        status: 200,
        headers: vec![
            ("Content-Length".into(), body.len().to_string()),
            ("X-Powered-By".into(), "replay".into()),
        ],
        body: body.as_bytes().to_vec(),
    };
    let observed = compare::normalize(response, Vec::new(), false, &roots, Vec::new(), Vec::new());
    let expected = r#"{"a":"/vosu-golden/songs/x.osu","b":"C:\\vosu-golden\\files\\y","c":"/tmp/.tmpA1/songsx","n":0}"#;
    assert_eq!(String::from_utf8(observed.body).unwrap(), expected);
    // Only compared headers remain, and the length follows the new body.
    assert_eq!(
        observed.headers,
        [("content-length".to_owned(), expected.len().to_string())]
    );
}

#[test]
fn golden_comparator_reports_exactly_the_part_that_differs() {
    let check = |edit: &dyn Fn(&mut Platform), expected: &[(&str, &str)]| {
        let mut changed = platform().clone();
        edit(&mut changed);
        assert_eq!(mismatches(&changed, &[]), pairs(expected));
    };
    const V2: &str = "state-http.get-json-v2";
    const WS: &str = "ws.upgrade-websocket-v2";
    check(
        &|p| case_mut(p, V2).expected.status = 203,
        &[(V2, "status")],
    );
    check(
        &|p| set_header(case_mut(p, V2), "content-type", "text/plain"),
        &[(V2, "headers.content-type")],
    );
    check(
        &|p| {
            case_mut(p, V2)
                .expected
                .headers
                .retain(|(n, _)| n != "connection")
        },
        &[(V2, "headers.connection")],
    );
    check(
        &|p| {
            let text = text_mut(case_mut(p, V2));
            assert!(text.contains("1234567"));
            *text = text.replacen("1234567", "1234568", 1);
        },
        &[(V2, "body")],
    );
    // Numbers, nullness and order are never normalized.
    check(
        &|p| {
            let text = text_mut(case_mut(p, V2));
            *text = text.replacen("\"paused\":false", "\"paused\":null", 1);
        },
        &[(V2, "body")],
    );
    // header-etag keeps distinct upstream tags distinct: one vosu tag cannot
    // stand for two upstream ordinals.
    check(
        &|p| {
            set_header(
                case_mut(p, "files.head-beatmap-file"),
                "etag",
                "\"<golden:etag:2>\"",
            )
        },
        &[("files.head-beatmap-file", "headers.etag")],
    );
    check(
        &|p| {
            let frame = &mut case_mut(p, WS).expected.frames.as_mut().unwrap()[0];
            frame.payload[2] ^= 1;
        },
        &[(WS, "ws.frame")],
    );
    check(
        &|p| case_mut(p, WS).expected.frames.as_mut().unwrap().clear(),
        &[(WS, "ws.frame")],
    );
    // A WS frame is compared as a frame, never against an HTTP body: the
    // handshake body stays its own part.
    check(
        &|p| {
            let case = case_mut(p, WS);
            let payload = case.expected.frames.as_ref().unwrap()[0].payload.clone();
            case.expected.body = Body::Text(String::from_utf8(payload).unwrap());
        },
        &[(WS, "body")],
    );
}

const CALC: &str = "calculation.no-query";

fn calc_rule(part: &str, key: &str, value: Value) -> Value {
    json!({
        "ledger_id": "legacy-calculation-engine",
        "platforms": [platform().name],
        "cases": [CALC],
        "part": part,
        key: value,
    })
}

/// The synthetic platform with an "upstream" calculation error wording that
/// differs from vosu's.
fn diverging() -> Platform {
    let mut changed = platform().clone();
    let text = text_mut(case_mut(&mut changed, CALC));
    assert_eq!(text, r#"{"error":"osu is not ready/running"}"#);
    *text = r#"{"error":"upstream wording"}"#.into();
    changed
}

#[test]
fn golden_rules_subtract_only_their_own_part() {
    let upstream = diverging();
    assert_eq!(mismatches(&upstream, &[]), pairs(&[(CALC, "body")]));
    let body = calc_rule(
        "body",
        "vosu_replacements",
        json!([{"pointer": "/error", "value": "osu is not ready/running"}]),
    );
    let subtracted = rules(&with(upstream.clone()), vec![body.clone()]).unwrap();
    let report = compare::compare(&upstream, &OBSERVED, &subtracted);
    assert_eq!(report.mismatches, Vec::<Mismatch>::new());
    assert_eq!(report.used, BTreeSet::from([0]));

    // A body rule never hides a header difference on the same case.
    let mut header_too = upstream.clone();
    let vosu_type = case_mut(&mut header_too, CALC)
        .expected
        .headers
        .iter()
        .find(|(n, _)| n == "content-type")
        .unwrap()
        .1
        .clone();
    set_header(case_mut(&mut header_too, CALC), "content-type", "text/html");
    let only_body = rules(&with(header_too.clone()), vec![body.clone()]).unwrap();
    assert_eq!(
        mismatches(&header_too, &only_body),
        pairs(&[(CALC, "headers.content-type")])
    );
    // A header rule subtracts exactly that header.
    let header = calc_rule("headers.content-type", "vosu_value", json!(vosu_type));
    let both = rules(&with(header_too.clone()), vec![body, header]).unwrap();
    let report = compare::compare(&header_too, &OBSERVED, &both);
    assert_eq!(report.mismatches, Vec::<Mismatch>::new());
    assert_eq!(report.used, BTreeSet::from([0, 1]));

    // A rule whose vosu value is not what vosu serves leaves the mismatch.
    let wrong = calc_rule(
        "body",
        "vosu_replacements",
        json!([{"pointer": "/error", "value": "another wording"}]),
    );
    let wrong = rules(&with(upstream.clone()), vec![wrong]).unwrap();
    assert_eq!(mismatches(&upstream, &wrong), pairs(&[(CALC, "body")]));
}

#[test]
fn golden_rules_refuse_unknown_temporary_overbroad_and_unused_rules() {
    let upstream = diverging();
    let fixture = with(upstream);
    let ledger = ledger();
    assert!(
        ledger
            .values()
            .all(|row| { matches!(row.class.as_str(), "security" | "contract" | "licensing") })
    );
    let valid = calc_rule(
        "body",
        "vosu_replacements",
        json!([{"pointer": "/error", "value": "osu is not ready/running"}]),
    );
    rules(&fixture, vec![valid.clone()]).unwrap();
    let rejected = |list: Vec<Value>, message: &str| {
        let error = rules(&fixture, list).unwrap_err();
        assert!(error.contains(message), "{message:?} not in {error:?}");
    };
    let edit = |key: &str, value: Value| {
        let mut rule = valid.clone();
        rule[key] = value;
        rule
    };
    rejected(
        vec![edit("ledger_id", json!("no-such-row"))],
        "not a DIVERGENCES.md row",
    );
    rejected(
        vec![edit("platforms", json!(["windows-lazer"]))],
        "has no fixtures",
    );
    rejected(
        vec![edit("cases", json!(["calculation.absent"]))],
        "has no case",
    );
    rejected(vec![edit("cases", json!([CALC, CALC]))], "without repeats");
    // A rule that changes nothing is unused.
    rejected(
        vec![edit(
            "vosu_replacements",
            json!([{"pointer": "/error", "value": "upstream wording"}]),
        )],
        "changes nothing",
    );
    rejected(
        vec![edit(
            "vosu_replacements",
            json!([{"pointer": "/missing", "value": 1}]),
        )],
        "not in the upstream JSON",
    );
    rejected(
        vec![edit(
            "vosu_replacements",
            json!([{"pointer": "/error", "value": "a"}, {"pointer": "", "value": "b"}]),
        )],
        "overlaps",
    );
    rejected(vec![edit("vosu_value", json!("x"))], "exactly one of");
    rejected(
        vec![edit("part", json!("headers.date"))],
        "not a §10 record part",
    );
    rejected(
        vec![edit("part", json!("headers.x-powered-by"))],
        "not a §10 record part",
    );
    rejected(
        vec![edit("part", json!("ws.frame"))],
        "applies only to ws cases",
    );
    rejected(vec![valid.clone(), valid.clone()], "both claim");
    // A header issue subtracted through a whole body, on a route the ledger
    // row does not name, is a whole-route subtraction and refused.
    let whole = json!({
        "ledger_id": "legacy-game-file-sandbox",
        "platforms": [platform().name],
        "cases": ["files.get-beatmap-file"],
        "part": "body",
        "vosu_value": "anything",
    });
    rejected(
        vec![whole],
        "needs a ledger scope naming /files/beatmap/file",
    );
    let status = json!({
        "ledger_id": "adapter-listener-policy",
        "platforms": [platform().name],
        "cases": ["state-http.trailing-slash-json-v2"],
        "part": "status",
        "vosu_value": 410,
    });
    rejected(vec![status], "needs a ledger scope naming /json/v2/");
    let format = divergence::rules(
        Some(&json!({"format": "v0", "rules": []})),
        &ledger,
        &fixture,
    );
    assert!(format.unwrap_err().contains("format"));
    // Temporary and unknown ledger classes never subtract.
    for (class, message) in [("temporary", "temporary"), ("provisional", "unknown class")] {
        let mut ledger = ledger.clone();
        ledger.get_mut("legacy-calculation-engine").unwrap().class = class.into();
        let list = json!({"format": divergence::FORMAT, "rules": [valid.clone()]});
        let error = divergence::rules(Some(&list), &ledger, &fixture).unwrap_err();
        assert!(error.contains(message), "{error}");
    }
    // An absent rules file subtracts nothing.
    assert!(
        divergence::rules(None, &ledger, &fixture)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn golden_ledger_and_scope_matching_are_exact() {
    let text = "| id | class | scope | reason | closed by |\n| --- | --- | --- | --- | --- |\n\
                | a | contract | `/json/v2`, `/websocket/v2` | r | c |\n";
    let ledger = divergence::ledger(text).unwrap();
    let scope = &ledger["a"].scope;
    assert!(divergence::scope_names(scope, "/json/v2"));
    assert!(divergence::scope_names(scope, "/websocket/v2"));
    assert!(!divergence::scope_names(scope, "/json"));
    assert!(!divergence::scope_names(scope, "/json/v2/"));
    assert!(!divergence::scope_names(scope, "/websocket/v2/precise"));
    let broken = format!("{text}| b | contract | scope | with | extra | cell |\n");
    assert!(
        divergence::ledger(&broken)
            .unwrap_err()
            .contains("five cells")
    );
    let duplicate = format!("{text}| a | security | s | r | c |\n");
    assert!(
        divergence::ledger(&duplicate)
            .unwrap_err()
            .contains("twice")
    );
    // Replacements splice the upstream bytes and keep everything else.
    let change = divergence::Change::Replacements(vec![("/b/1".into(), json!(2.5))]);
    let spliced = divergence::apply_text(&change, r#"{"a":1.0,"b":[0, 1 ,2],"c":null}"#).unwrap();
    assert_eq!(spliced, r#"{"a":1.0,"b":[0, 2.5 ,2],"c":null}"#);
}
