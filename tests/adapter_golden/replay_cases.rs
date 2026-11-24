//! R0 replay mechanics. These are in-process round trips and counterexamples,
//! never golden evidence: the envelopes come from a vosu source, not from
//! an upstream observation.
use super::{
    Value,
    assets::{AssetFile, ReplayAssets, RootAsset},
    compare::replay_assets,
    envelope::{Envelope, TREE_POSITIONS, selection_digest, tokens},
    fixture::Asset,
    json,
    replay::Replay,
    routes::{self, state_path},
    source::{self, Source},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::sync::LazyLock;
use vosu::server::websocket::Schema;
use vosu_plugin_api::{CborValue, vosu_status};

const SENTINEL: &[u8] = b"R0 private-path sentinel";

/// One selected stable client.
static SINGLE: LazyLock<Source> = LazyLock::new(|| source::capture(1));
/// Two stable clients; the second is selected and owns the foreground.
static PAIR: LazyLock<Source> = LazyLock::new(|| source::capture(2));

fn single() -> &'static Source {
    &SINGLE
}
fn pair() -> &'static Source {
    &PAIR
}

fn index(schema: Schema) -> usize {
    match schema {
        Schema::V1 => 0,
        Schema::StreamCompanion => 1,
        Schema::V2 => 2,
        Schema::V2Precise => 3,
    }
}
fn raw(source: &Source, schema: Schema) -> Value {
    serde_json::from_slice(&source.envelopes[index(schema)]).unwrap()
}
fn envelope(source: &Source, schema: Schema) -> Envelope {
    Envelope::parse(&source.envelopes[index(schema)]).unwrap()
}
/// Re-digests an edited envelope, as a forged dump would.
fn reseal(mut value: Value) -> Envelope {
    value["digest"] = json!(selection_digest(&value).unwrap());
    Envelope::parse(&serde_json::to_vec(&value).unwrap()).unwrap()
}
fn start(envelopes: &[Envelope], assets: &ReplayAssets) -> Replay {
    Replay::start(envelopes, assets).unwrap_or_else(|error| panic!("replay: {error}"))
}
fn rejected(envelopes: &[Envelope], assets: &ReplayAssets) -> String {
    match Replay::start(envelopes, assets) {
        Ok(_) => panic!("replay was accepted"),
        Err(error) => error,
    }
}
/// The route bytes served by the real listener equal the recorded result.
fn assert_route(replay: &Replay, envelope: &Envelope) {
    let path = state_path(envelope.schema);
    let response = routes::get(replay.address, path);
    assert_eq!(response.status, envelope.http.status, "{path}");
    assert_eq!(
        String::from_utf8_lossy(&response.body),
        envelope.http.body,
        "{path}: route bytes differ from the recording"
    );
}
fn entry_mut<'a>(value: &'a mut Value, ns: &str, pointer: &str) -> &'a mut Value {
    value["entries"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|entry| entry["ns"] == ns && entry["pointer"] == pointer)
        .unwrap_or_else(|| panic!("no recorded {ns}{pointer}"))
}
fn cbor(value: &CborValue) -> Value {
    json!(STANDARD.encode(vosu_plugin_api::encode(value).unwrap()))
}
/// Rewrites the recorded `/internal/paths` current chart location.
fn with_chart_path(source: &Source, folder: &str, file: &str) -> Envelope {
    with_chart_path_in(source, Schema::V2, folder, file)
}
fn with_chart_path_in(source: &Source, schema: Schema, folder: &str, file: &str) -> Envelope {
    let mut value = raw(source, schema);
    let entry = entry_mut(&mut value, "vosu.osu", "/internal/paths");
    let bytes = STANDARD
        .decode(entry["value_cbor_b64"].as_str().unwrap())
        .unwrap();
    let mut paths: CborValue = vosu_plugin_api::decode(&bytes).unwrap();
    let CborValue::Map(fields) = &mut paths else {
        panic!("/internal/paths is not a map");
    };
    let (_, current) = fields
        .iter_mut()
        .find(|(key, _)| key.as_text() == Some("current"))
        .expect("recorded current paths");
    let CborValue::Map(current) = current else {
        panic!("current paths are not a map");
    };
    for (name, text) in [("beatmapFolder", folder), ("beatmapFile", file)] {
        let (_, slot) = current
            .iter_mut()
            .find(|(key, _)| key.as_text() == Some(name))
            .unwrap_or_else(|| panic!("no recorded {name}"));
        *slot = CborValue::Text(text.into());
    }
    entry["value_cbor_b64"] = cbor(&paths);
    reseal(value)
}

#[test]
fn replay_round_trip_serves_the_recorded_route_bytes_for_every_schema() {
    let source = single();
    let envelopes = source.parsed();
    assert_eq!(envelopes.len(), 4);
    for envelope in &envelopes {
        assert_eq!(envelope.http.kind, "ready", "{}", envelope.name());
        assert_eq!(envelope.http.status, 200);
        assert!(envelope.entries.iter().any(|entry| entry.value.is_some()));
        assert_eq!(envelope.session.as_ref().unwrap().ordinal, 0);
    }
    // The chart-derived streamCompanion view is part of the recording.
    assert_eq!(envelopes[index(Schema::StreamCompanion)].derived, "ready");
    for envelope in &envelopes {
        let replay = start(std::slice::from_ref(envelope), &source.assets);
        assert_route(&replay, envelope);
    }
    // All four selections replay into one coherent bus and session.
    let replay = start(&envelopes, &source.assets);
    for envelope in &envelopes {
        assert_route(&replay, envelope);
    }
}

#[test]
fn replay_without_a_client_serves_the_recorded_not_ready_response() {
    let source = source::capture(0);
    let envelopes = source.parsed();
    for envelope in &envelopes {
        assert_eq!(envelope.http.kind, "not_ready", "{}", envelope.name());
        assert!(envelope.session.is_none() && envelope.roster.is_empty());
    }
    let replay = start(&envelopes, &ReplayAssets::default());
    assert!(replay.attached.is_empty());
    for envelope in &envelopes {
        assert_route(&replay, envelope);
    }
    // Replay roots need a selected session to be declared for.
    let roots_only = ReplayAssets {
        roots: single().assets.roots.clone(),
        resources: Vec::new(),
    };
    let error = rejected(&envelopes, &roots_only);
    assert!(error.contains("selected session"), "{error}");
}

#[test]
fn replay_rejects_an_invalid_batch_without_committing_any_entry() {
    let source = single();
    let recorded = envelope(source, Schema::V2);
    let mut value = raw(source, Schema::V2);
    // The same session with nothing published: the uncommitted baseline.
    let mut unpublished = value.clone();
    for entry in unpublished["entries"].as_array_mut().unwrap() {
        entry["value_cbor_b64"] = Value::Null;
    }
    // The osu! grant covers only `vosu.status` `/status`.
    value["entries"].as_array_mut().unwrap().push(json!({
        "ns": "vosu.status",
        "pointer": "/unlisted",
        "value_cbor_b64": cbor(&CborValue::Text("outside the grant".into())),
        "error": null,
    }));
    let forged = reseal(value);
    let assets = ReplayAssets::default();
    let error = rejected(std::slice::from_ref(&forged), &assets);
    assert!(error.contains("publish batch"), "{error}");

    let replay = Replay::launch(std::slice::from_ref(&forged), &assets).unwrap();
    assert_eq!(replay.attached.len(), 1);
    assert_eq!(replay.attached[0].identify, vosu_status::VOSU_OK);
    let publish = replay.attached[0].publish.expect("a batch was sent");
    assert_ne!(publish, vosu_status::VOSU_OK);
    let rejected_dump = replay.dump(Schema::V2).unwrap();
    let response = routes::get(replay.address, "/json/v2");
    assert_ne!(response.body, recorded.http.body.as_bytes());
    drop(replay);

    // Not one of the valid entries in that batch was committed.
    let baseline = Replay::launch(&[reseal(unpublished)], &assets).unwrap();
    assert_eq!(baseline.attached[0].publish, None);
    let unpublished = baseline.dump(Schema::V2).unwrap();
    assert_eq!(rejected_dump.bus_revision, unpublished.bus_revision);
    assert_eq!(rejected_dump.entries, unpublished.entries);
    assert_ne!(rejected_dump.entries, recorded.entries);
}

#[test]
fn replay_rejects_a_session_owned_by_another_plugin() {
    let mut value = raw(single(), Schema::V2);
    value["session"]["owner"] = json!("vosu.other");
    value["roster"]["sessions"][0]["owner"] = json!("vosu.other");
    let error = rejected(&[reseal(value)], &single().assets);
    assert!(error.contains("owned by vosu.other"), "{error}");
}

#[test]
fn replay_rejects_a_selection_edited_after_capture() {
    let mut value = raw(single(), Schema::V2);
    let entry = &mut value["entries"][0];
    entry["value_cbor_b64"] = cbor(&CborValue::Text("edited".into()));
    let error = Envelope::parse(&serde_json::to_vec(&value).unwrap()).unwrap_err();
    assert!(error.contains("digest"), "{error}");
}

#[test]
fn replay_never_falls_back_to_a_recorded_path_for_a_missing_chart() {
    let source = single();
    let sc = envelope(source, Schema::StreamCompanion);
    let error = rejected(std::slice::from_ref(&sc), &ReplayAssets::default());
    assert!(error.contains("needs a replay asset"), "{error}");
    // A root holding the chart file under its recorded name is still not a
    // resource: only a declared replay resource can back the reference.
    let roots_only = ReplayAssets {
        roots: source.assets.roots.clone(),
        resources: Vec::new(),
    };
    let error = rejected(std::slice::from_ref(&sc), &roots_only);
    assert!(error.contains("needs a replay asset"), "{error}");
    // A replay resource cannot stand in for a recorded absence either.
    let mut value = raw(source, Schema::StreamCompanion);
    entry_mut(&mut value, "vosu.chart", "/resources/file")["value_cbor_b64"] = Value::Null;
    let error = rejected(&[reseal(value)], &source.assets);
    assert!(error.contains("contradicts"), "{error}");
}

#[test]
fn replay_never_uses_a_recorded_private_path_as_file_authority() {
    let source = single();
    let outside = tempfile::TempDir::new().unwrap();
    std::fs::write(outside.path().join("sentinel.osu"), SENTINEL).unwrap();
    let private = outside.path().to_string_lossy().into_owned();
    let replay_root = ReplayAssets {
        roots: vec![RootAsset {
            slot: "songs".into(),
            files: vec![AssetFile {
                path: "replay/chart.osu".into(),
                bytes: b"replay chart".to_vec(),
                modified: None,
            }],
        }],
        resources: Vec::new(),
    };
    // The recorded folder names a real private file, with and without a
    // declared replay root: neither serves it.
    let private_paths = with_chart_path(source, &private, "sentinel.osu");
    for assets in [ReplayAssets::default(), replay_root.clone()] {
        let replay = start(std::slice::from_ref(&private_paths), &assets);
        let response = routes::get(replay.address, "/files/beatmap/file");
        assert_ne!(response.status, 200);
        assert_ne!(response.body, SENTINEL);
    }
    // A recorded relative path resolves only inside the declared replay root.
    let relative = with_chart_path(source, "replay", "chart.osu");
    let replay = start(std::slice::from_ref(&relative), &replay_root);
    let response = routes::get(replay.address, "/files/beatmap/file");
    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"replay chart");
}

#[test]
fn replay_identifies_sessions_in_recorded_order() {
    let source = pair();
    let recorded = envelope(source, Schema::V2);
    let foreground: Vec<_> = recorded.roster.iter().map(|s| s.foreground).collect();
    assert_eq!(foreground, [Some(false), Some(true)]);
    assert_eq!(recorded.session.as_ref().unwrap().ordinal, 1);
    let replay = start(std::slice::from_ref(&recorded), &source.assets);
    assert_eq!(replay.attached.len(), 2);
    assert_route(&replay, &recorded);
    drop(replay);

    // Distinguishable sessions come back in roster order, selection intact.
    let mut value = raw(source, Schema::V2);
    value["roster"]["sessions"][0]["variant"] = json!("lazer");
    let replay = start(&[reseal(value)], &source.assets);
    let dump = replay.dump(Schema::V2).unwrap();
    let variants: Vec<_> = dump.roster.iter().map(|s| s.variant.as_str()).collect();
    assert_eq!(variants, ["lazer", "stable"]);
    assert_eq!(dump.session.unwrap().ordinal, 1);
    drop(replay);

    // A roster whose generations run against roster order cannot be replayed.
    let mut value = raw(source, Schema::V2);
    let sessions = &mut value["roster"]["sessions"];
    let (first, second) = (
        sessions[0]["generation"].clone(),
        sessions[1]["generation"].clone(),
    );
    sessions[0]["generation"] = second.clone();
    sessions[1]["generation"] = first;
    value["session"]["generation"] = second;
    let error = rejected(&[reseal(value)], &source.assets);
    assert!(error.contains("generations"), "{error}");
}

#[test]
fn replay_keeps_foreground_on_a_nonselected_session() {
    let source = pair();
    let mut value = raw(source, Schema::V2);
    // Select the first session while the second keeps the foreground.
    value["session"] = value["roster"]["sessions"][0].clone();
    let forged = reseal(value);
    let replay = start(std::slice::from_ref(&forged), &source.assets);
    let dump = replay.dump(Schema::V2).unwrap();
    let selected = dump.session.unwrap();
    assert_eq!((selected.ordinal, selected.foreground), (0, Some(false)));
    let foreground: Vec<_> = dump.roster.iter().map(|s| s.foreground).collect();
    assert_eq!(foreground, [Some(false), Some(true)]);
}

#[test]
fn replay_listener_and_workers_stop_when_dropped() {
    let source = single();
    let sc = envelope(source, Schema::StreamCompanion);
    let replay = start(std::slice::from_ref(&sc), &source.assets);
    let address = replay.address;
    assert_eq!(routes::get(address, "/json/sc").status, 200);
    drop(replay);
    assert!(
        std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_secs(2)).is_err(),
        "the replay listener outlived its owner"
    );
    // The process-global game setup and serial lock were released.
    let replay = start(std::slice::from_ref(&sc), &source.assets);
    assert_route(&replay, &sc);
}

/// Replay refuses recorded tree references instead of serving trees, because
/// no adapter selects one. This fails the day an adapter starts to, both in
/// its own selector list and in the selection the production composition
/// actually dumps for each schema.
#[test]
fn adapter_selections_never_include_tree_refs() {
    let covers_tree = |ns: &str, pointer: &str| {
        let selected = tokens(pointer).expect("valid selector pointer");
        TREE_POSITIONS.iter().any(|(tree_ns, tree_pointer)| {
            ns == *tree_ns && tokens(tree_pointer).unwrap().starts_with(&selected)
        })
    };
    let adapters = [
        (Schema::V1, vosu_adapter_gosumemory::view::selectors()),
        (
            Schema::StreamCompanion,
            vosu_adapter_streamcompanion::view::selectors(),
        ),
        (Schema::V2, vosu_adapter_upstream::regular::selectors()),
        (
            Schema::V2Precise,
            vosu_adapter_upstream::precise::selectors(),
        ),
    ];
    let source = single();
    let replay = start(&source.parsed(), &source.assets);
    for (schema, selectors) in adapters {
        assert!(!selectors.is_empty());
        for selector in &selectors {
            assert!(
                !covers_tree(&selector.ns, &selector.pointer),
                "{schema:?} selects {}{}, which holds a tree reference; extend replay to \
                 serve trees before allowing it",
                selector.ns,
                selector.pointer
            );
        }
        // The production composition dumps exactly these selectors.
        let dumped: Vec<_> = replay
            .dump(schema)
            .unwrap()
            .entries
            .into_iter()
            .map(|entry| (entry.ns, entry.pointer))
            .collect();
        let declared: Vec<_> = selectors
            .into_iter()
            .map(|selector| (selector.ns, selector.pointer))
            .collect();
        assert_eq!(dumped, declared, "{schema:?}");
    }
}

#[test]
fn replay_refuses_a_recorded_tree_reference() {
    let tree = CborValue::Map(vec![(
        CborValue::Text("url".into()),
        CborValue::Text("/api/v1/trees/recorded/".into()),
    )]);
    for (ns, pointer) in TREE_POSITIONS {
        let mut value = raw(single(), Schema::V2);
        value["entries"].as_array_mut().unwrap().push(json!({
            "ns": ns,
            "pointer": pointer,
            "value_cbor_b64": cbor(&tree),
            "error": null,
        }));
        let error = rejected(&[reseal(value)], &single().assets);
        assert!(
            error.contains("adapter selections never include tree refs"),
            "{ns}{pointer}: {error}"
        );
    }
}

/// Linux hosts have no OS-foreground source and record `null`. Replay uses a
/// source that is unavailable through the same core seam, so the replayed
/// roster says `null` too, never `false` (`Replay::start` also checks every
/// replayed fact against the recording).
#[test]
fn replay_reproduces_a_recorded_unknown_foreground() {
    let source = single();
    let mut value = raw(source, Schema::V2);
    value["session"]["foreground"] = Value::Null;
    value["roster"]["sessions"][0]["foreground"] = Value::Null;
    let recorded = reseal(value);
    let replay = start(std::slice::from_ref(&recorded), &source.assets);
    let dump = replay.dump(Schema::V2).unwrap();
    assert_eq!(dump.session.unwrap().foreground, None);
    assert_eq!(dump.roster[0].foreground, None);
    drop(replay);

    // One host source cannot report a fact for one session and none for
    // another.
    let mut value = raw(pair(), Schema::V2);
    value["roster"]["sessions"][0]["foreground"] = Value::Null;
    let error = rejected(&[reseal(value)], &pair().assets);
    assert!(error.contains("partly unknown"), "{error}");
}

/// Two approved assets of one media type are told apart by the recorded
/// location the osu! plugin resolves its reference from. A recorded private
/// absolute location selects no asset, so its file is never served.
#[test]
fn replay_assets_follow_the_recorded_location_never_a_private_path() {
    let source = single();
    let chart = source.assets.roots[0].files[0].bytes.clone();
    let asset = |id: &str, relative: &str, bytes: Vec<u8>| Asset {
        id: id.into(),
        slot: "songs".into(),
        relative: relative.into(),
        media_type: "text/plain; charset=utf-8".into(),
        bytes,
    };
    let mut second = chart.clone();
    second.extend_from_slice(b"\n");
    let approved = [
        asset("first", "first/chart.osu", chart.clone()),
        asset("second", "second/chart.osu", second),
    ];
    for (folder, approved_asset) in ["first", "second"].into_iter().zip(&approved) {
        let sc = with_chart_path_in(source, Schema::StreamCompanion, folder, "chart.osu");
        let assets = replay_assets(&approved, &sc).unwrap();
        assert_eq!(assets.resources.len(), 1);
        assert_eq!(assets.resources[0].path, format!("{folder}/chart.osu"));
        // The replayed chart reference resolved (derived work is ready) and
        // the recorded location serves that asset's bytes.
        let replay = start(std::slice::from_ref(&sc), &assets);
        let response = routes::get(replay.address, "/files/beatmap/file");
        assert_eq!(response.status, 200);
        assert_eq!(response.body, approved_asset.bytes);
    }

    let outside = tempfile::TempDir::new().unwrap();
    std::fs::create_dir(outside.path().join("first")).unwrap();
    std::fs::write(outside.path().join("first/chart.osu"), SENTINEL).unwrap();
    let private = outside.path().join("first").to_string_lossy().into_owned();
    let sc = with_chart_path_in(source, Schema::StreamCompanion, &private, "chart.osu");
    let error = replay_assets(&approved, &sc).unwrap_err();
    assert!(error.contains("never file authority"), "{error}");
    // Without that reference the replay still serves only declared roots.
    let v2 = with_chart_path(source, &private, "chart.osu");
    let assets = replay_assets(&approved, &v2).unwrap();
    assert!(assets.resources.is_empty());
    let replay = start(std::slice::from_ref(&v2), &assets);
    let response = routes::get(replay.address, "/files/beatmap/file");
    assert_ne!(response.status, 200);
    assert_ne!(response.body, SENTINEL);
}
