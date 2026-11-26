use super::*;
use serde_json::{Value, json};
mod fixture;

fn json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
}
fn dto_bytes(value: &impl serde::Serialize) -> Vec<u8> {
    vosu_adapter_support::json::encode(value, 0).unwrap()
}

#[test]
fn regular_bus_projection_has_no_extensions_and_direct_matches_dto() {
    let source = fixture::source();
    let selected = fixture::capture(&source, &regular::selectors());
    // Positive controls: this is real producer -> ABI batch -> state-store data.
    let paths: Value = view::decode(&selected, "/internal/paths").unwrap();
    assert_eq!(paths["stable"]["game"], "/private-game-positive-control");
    let view = regular::View::from_capture(&selected).unwrap();
    let bytes = regular::encode(&view).unwrap();
    assert_eq!(bytes.as_ref(), dto_bytes(regular::build(&view)));
    let document = json(&bytes);
    assert!(document.get("keys").is_none());
    for key in [
        "pressHitErrors",
        "releaseHitErrors",
        "keyHitErrors",
        "hitOffsets",
        "catcher",
    ] {
        assert!(document["play"].get(key).is_none(), "{key}");
        for client in document["tourney"]["clients"].as_array().unwrap() {
            assert!(client["play"].get(key).is_none());
        }
    }
    assert_eq!(document["play"]["score"], 123456.789);
    assert_eq!(document["play"]["accuracy"], 98.123456789);
    assert_eq!(
        document["play"]["healthBar"]["normal"],
        (source.regular.gameplay.player_hp / 200.0) * 100.0
    );
    assert_eq!(document["play"]["hitErrorArray"], json!([-7.5, 2.25]));
    assert_eq!(document["play"]["hits"]["geki"], 6);
    assert_eq!(document["play"]["hits"]["sliderEndHits"], 9);
    assert_eq!(
        document["folders"]["songs"],
        "/private-songs-positive-control"
    );
    assert_eq!(
        document["directPath"]["beatmapFile"],
        paths::direct_join(Some("folder/../distinct-chart"), Some("a \" unicode🎵.osu"))
    );
    #[cfg(target_os = "windows")]
    assert_eq!(document["game"]["focused"], true);
    #[cfg(not(target_os = "windows"))]
    assert!(document["game"].get("focused").is_none());
}

#[test]
fn precise_uses_independent_overlay_slots_and_retains_only_upstream_history() {
    let source = fixture::source();
    let selected = fixture::capture(&source, &precise::selectors());
    let view = precise::View::from_capture(&selected).unwrap();
    let bytes = precise::encode(&view).unwrap();
    assert_eq!(bytes.as_ref(), dto_bytes(precise::build(&view)));
    assert_eq!(
        String::from_utf8(bytes.to_vec()).unwrap(),
        r#"{"keys":{"k1":{"isPressed":true,"count":11},"k2":{"isPressed":false,"count":22},"m1":{"isPressed":true,"count":33},"m2":{"isPressed":false,"count":44}},"hitErrors":[-7.5,2.25],"tourney":[]}"#
    );
    assert_eq!(source.precise.keypresses[0].count, 999);
    assert_eq!(source.precise.hit_errors.press, [31.0]);
    assert_eq!(source.precise.hit_errors.release, [42.0]);
}

#[test]
fn null_slots_default_but_absent_required_selection_is_not_ready() {
    let mut selected = fixture::capture(&fixture::source(), &precise::selectors());
    fixture::replace(
        &mut selected,
        "/keys/overlay",
        json!({"k1":null,"k2":{"isPressed":true,"count":29},"m1":null,"m2":null}),
    );
    let projected = precise::View::from_capture(&selected).unwrap();
    assert_eq!(precise::build(&projected).keys.k1.count, 0);
    assert_eq!(precise::build(&projected).keys.k2.count, 29);
    let entries = std::sync::Arc::make_mut(&mut selected.values);
    entries
        .iter_mut()
        .find(|entry| entry.pointer == "/keys/overlay")
        .unwrap()
        .value = None;
    assert!(matches!(
        precise::View::from_capture(&selected),
        Err(Error::NotReady)
    ));
}

#[test]
fn availability_distinguishes_omission_from_whole_response_failure() {
    let mut selected = fixture::capture(&fixture::source(), &regular::selectors());
    fixture::replace(&mut selected, "/tourney", Value::Null);
    fixture::replace(&mut selected, "/tourneyAvailability", json!("pending"));
    let pending = regular::View::from_capture(&selected).unwrap();
    assert!(
        json(&regular::encode(&pending).unwrap())
            .get("tourney")
            .is_none()
    );
    fixture::replace(&mut selected, "/tourneyAvailability", json!("missingScore"));
    assert!(matches!(
        regular::View::from_capture(&selected),
        Err(Error::MissingTourneyScore)
    ));
}

#[test]
fn inverse_schema_names_and_nested_removal_apply_before_filtering() {
    let mut selected = fixture::capture(&fixture::source(), &regular::selectors());
    fixture::replace(&mut selected, "/room", json!({"roomId":123,"users":[]}));
    // A real, nonempty nested source play, including extension source histories.
    let mut tourney: Value = view::decode(&selected, "/tourney").unwrap();
    tourney["bestOf"] = json!(7);
    tourney["clients"] = json!([{
        "ipcId":3.0,"team":"left","settings":{"mania":{"scrollSpeed":19}},
        "user":{"id":4,"name":"spectator","country":"Japan","accuracy":98.0,"rankedScore":123.0,"playCount":7,"globalRank":8,"totalPP":9.0},
        "beatmap":{"stats": view::decode::<Value>(&selected, "/beatmap").unwrap()["stats"]},
        "play":{
            "source":"spectator","failed":false,"playerName":"player","mode":{"number":0,"name":"osu!"},
            "score":99.0,"accuracy":95.0,"healthBar":{"normal":75.0,"smooth":65.0},
            "judgements":{"miss":1,"meh":2,"ok":3,"good":4,"great":5,"perfect":6},
            "statistics":{"sliderBreaks":7,"sliderEndHits":8,"smallTickHit":9,"largeTickHit":10},
            "hitErrors":{"all":[11.0],"press":[12.0],"release":[13.0]},
            "keyHitErrors":{"secret":true},"hitOffsets":[14.0],"catcher":{"secret":true},
            "combo":{"current":12,"max":15},"mods": view::decode::<Value>(&selected, "/play/mods").unwrap(),
            "rank":{"current":"A","maxThisPlay":"S"},"pp":view::decode::<Value>(&selected, "/play/pp").unwrap(),"unstableRate":17.0
        }
    }]);
    fixture::replace(&mut selected, "/tourney", tourney);
    let projected = regular::View::from_capture(&selected).unwrap();
    let encoded = regular::encode(&projected).unwrap();
    assert_eq!(encoded.as_ref(), dto_bytes(regular::build(&projected)));
    let document = json(&encoded);
    assert_eq!(document["room"]["roomID"], 123);
    assert!(document["room"].get("roomId").is_none());
    assert_eq!(document["tourney"]["bestOF"], 7);
    assert!(document["tourney"].get("bestOf").is_none());
    assert_eq!(
        document["tourney"]["clients"][0]["play"]["hitErrorArray"],
        json!([11])
    );
    for field in [
        "pressHitErrors",
        "releaseHitErrors",
        "keyHitErrors",
        "hitOffsets",
        "catcher",
    ] {
        assert!(
            document["tourney"]["clients"][0]["play"]
                .get(field)
                .is_none()
        );
    }
    use vosu_surface::filter::Filter;
    let filtered = regular::filtered(&projected)
        .encode(&[
            Filter::Field("keys".into()),
            Filter::Nested {
                field: "play".into(),
                keys: vec![
                    Filter::Field("pressHitErrors".into()),
                    Filter::Field("hitErrorArray".into()),
                ],
            },
        ])
        .unwrap();
    assert_eq!(
        json(&filtered),
        json!({"play":{"hitErrorArray":[-7.5,2.25]}})
    );
}

#[test]
fn precise_tourney_is_ipc_sorted_and_each_spectator_keeps_own_data() {
    let mut selected = fixture::capture(&fixture::source(), &precise::selectors());
    let slot = |n| json!({"k1":{"isPressed":true,"count":n},"k2":null,"m1":null,"m2":null});
    fixture::replace(
        &mut selected,
        "/tourney",
        json!({"clients":[
            {"ipcId":30.25,"keysOverlay":slot(31),"play":{"hitErrors":{"all":[32.0],"press":[33.0],"release":[34.0]}}},
            {"ipcId":2.5,"keysOverlay":slot(41),"play":{"hitErrors":{"all":[42.0],"press":[43.0],"release":[44.0]}}}
        ]}),
    );
    let projected = precise::View::from_capture(&selected).unwrap();
    let encoded = precise::encode(&projected).unwrap();
    assert_eq!(encoded.as_ref(), dto_bytes(precise::build(&projected)));
    let document = json(&encoded);
    assert_eq!(document["tourney"][0]["ipcId"], 2.5);
    assert_eq!(document["tourney"][0]["keys"]["k1"]["count"], 41);
    assert_eq!(document["tourney"][1]["hitErrors"], json!([32]));
    assert_eq!(document["tourney"][0].as_object().unwrap().len(), 3);
    assert!(document["tourney"][0]["keys"].get("all").is_none());
}

#[test]
fn regular_selectors_do_not_demand_removed_precise_only_sources() {
    let regular = regular::selectors();
    for pointer in [
        "/keys/overlay",
        "/keys/all",
        "/play/hitErrors/press",
        "/play/hitErrors/release",
        "/play/keyHitErrors",
        "/play/hitOffsets",
        "/play/catcher",
    ] {
        assert!(!regular.iter().any(|selector| selector.pointer == pointer));
    }
}

#[test]
fn precise_ecmascript_numbers_match_owned_encoding() {
    let mut selected = fixture::capture(&fixture::source(), &precise::selectors());
    let values = vosu_adapter_support::wire_fixtures::NUMBERS;
    let entry = std::sync::Arc::make_mut(&mut selected.values)
        .iter_mut()
        .find(|entry| entry.pointer == "/play/hitErrors/all")
        .unwrap();
    entry.value = Some(ciborium::Value::Array(
        values
            .iter()
            .map(|&value| ciborium::Value::Float(value))
            .collect(),
    ));
    let projected = precise::View::from_capture(&selected).unwrap();
    assert_eq!(
        precise::encode(&projected).unwrap().as_ref(),
        dto_bytes(precise::build(&projected))
    );
}

#[test]
fn accurate_percent_and_raw_health_never_invert_the_normalized_bus_values() {
    let mut source = fixture::source();
    let boundary = 0.0001_f64;
    assert_ne!((boundary / 100.0) * 100.0, boundary);
    for value in [boundary, 0.1, 99.999_999_999, -0.0] {
        let regular = std::sync::Arc::make_mut(&mut source.regular);
        regular.gameplay.accuracy = value;
        regular.gameplay.player_hp = value;
        regular.gameplay.player_hp_smooth = value + 0.123_456_789;
        let selected = fixture::capture(&source, &regular::selectors());
        let projected = regular::View::from_capture(&selected).unwrap();
        let response = regular::build(&projected);
        assert_eq!(response.play.accuracy.to_bits(), value.to_bits());
        assert_eq!(
            response.play.health_bar.normal.to_bits(),
            ((value / 200.0) * 100.0).to_bits()
        );
        assert_eq!(
            regular::encode(&projected).unwrap().as_ref(),
            dto_bytes(response)
        );
    }
}

#[test]
fn one_shot_projection_uses_exactly_one_coherent_capture_and_no_subscription() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use vosu_plugin_api::Selector;
    use vosu_surface::files::{FileError, FilesRequester, OpenedFile, Operation, RootLease};
    use vosu_surface::resources::{PublishedResource, ResourceRequester};
    use vosu_surface::routes::*;
    use vosu_surface::{requester::BundledRequester, state::*};
    struct Requester {
        image: CapturedSelection,
        captures: AtomicUsize,
    }
    impl FilesRequester for Requester {
        fn declared_root(&self, _: &IdentifiedSession, _: &str) -> Result<RootLease, FileError> {
            Err(FileError::PermissionDenied)
        }
        fn open_calculation_path(
            &self,
            _: &IdentifiedSession,
            _: &str,
        ) -> Operation<'_, OpenedFile> {
            Box::pin(async { Err(FileError::PermissionDenied) })
        }
    }
    impl ResourceRequester for Requester {
        fn open_published_resource(
            &self,
            _: &IdentifiedSession,
            _: &str,
        ) -> Operation<'_, PublishedResource> {
            Box::pin(async { Err(FileError::PermissionDenied) })
        }
    }
    impl BundledRequester for Requester {
        fn capture(
            &self,
            target: SessionTarget,
            selectors: &[Selector],
        ) -> Result<CapturedSelection, SurfaceError> {
            assert_eq!(target, SessionTarget::Focused);
            assert_eq!(selectors, regular::selectors());
            assert_eq!(self.captures.fetch_add(1, Ordering::SeqCst), 0);
            Ok(self.image.clone())
        }
        fn capture_roster(&self, _: &str) -> Result<IdentifiedRoster, SurfaceError> {
            panic!("roster must come from the same capture");
        }
        fn subscribe(
            &self,
            _: SessionTarget,
            _: Vec<Selector>,
        ) -> Result<SurfaceSubscription, SurfaceError> {
            panic!("one-shot reads must create no demand");
        }
        fn register_routes(
            &self,
            _: Vec<(RouteDescriptor, Arc<dyn RouteProvider>)>,
        ) -> Result<RegistrationLease, RegistrationError> {
            panic!("builder does not mount production routes");
        }
    }
    let requester = Requester {
        image: fixture::capture(&fixture::source(), &regular::selectors()),
        captures: AtomicUsize::new(0),
    };
    let projected = regular::View::capture(&requester, SessionTarget::Focused).unwrap();
    assert_eq!(requester.captures.load(Ordering::SeqCst), 1);
    assert_eq!(projected.identity.session.session, "fixture-session");
    assert_eq!(regular::build(&projected).play.score, 123456.789);
}

#[test]
fn source_chat_decoding_restores_client_specific_property_order() {
    let mut selected = fixture::capture(&fixture::source(), &regular::selectors());
    let mut tourney: Value = view::decode(&selected, "/tourney").unwrap();
    tourney["chat"] =
        json!([{"team":"none","timestamp":"12:34","name":"speaker","message":"hello"}]);
    fixture::replace(&mut selected, "/tourney", tourney);
    for (lazer, expected) in [
        (
            false,
            r#""chat":[{"team":"none","name":"speaker","message":"hello","timestamp":"12:34"}]"#,
        ),
        (
            true,
            r#""chat":[{"team":"none","timestamp":"12:34","name":"speaker","message":"hello"}]"#,
        ),
    ] {
        let source: source::SourceTourney = view::decode(&selected, "/tourney").unwrap();
        let encoded = String::from_utf8(dto_bytes(&source.into_wire(lazer))).unwrap();
        assert!(encoded.contains(expected), "{encoded}");
    }
}

#[test]
fn lazer_null_skin_path_echoes_the_pinned_empty_string() {
    let mut source = fixture::source();
    source.source = vosu_osu_model::enums::ClientType::Lazer;
    std::sync::Arc::make_mut(&mut source.regular)
        .global
        .skin_folder
        .clear();
    let selected = fixture::capture(&source, &regular::selectors());
    let paths: Value = view::decode(&selected, "/internal/paths").unwrap();
    assert!(paths["skinFolder"].is_null());
    let projected = regular::View::from_capture(&selected).unwrap();
    let emitted = json(&regular::encode(&projected).unwrap());
    let pinned: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/oracle/lazer-menu-hd/v2.json"
    ))
    .unwrap();
    assert_eq!(emitted["folders"]["skin"], pinned["folders"]["skin"]);
    assert_eq!(
        emitted["directPath"]["skinFolder"],
        pinned["directPath"]["skinFolder"]
    );
    assert_eq!(emitted["directPath"]["skinFolder"], "");
}

#[test]
fn clock_and_play_source_remain_bus_only_in_dto_direct_and_filtered_output() {
    use std::sync::Arc;
    use vosu_osu_model::memory::{ClientRenderState, PlaybackClock};
    use vosu_surface::filter::Filter;
    let mut source = fixture::source();
    Arc::make_mut(&mut source.regular).global.render = Some(ClientRenderState {
        spectating: true,
        clock: Some(PlaybackClock {
            rate: 1.37,
            running: false,
        }),
    });
    let facts = fixture::capture(
        &source,
        &view::selectors(&["/beatmap/time", "/play/source"]),
    );
    let time: Value = view::decode(&facts, "/beatmap/time").unwrap();
    assert_eq!(time["rate"], 1.37);
    assert_eq!(time["running"], false);
    assert_eq!(
        view::decode::<Value>(&facts, "/play/source").unwrap(),
        "spectator"
    );
    let selected = fixture::capture(&source, &regular::selectors());
    let view = regular::View::from_capture(&selected).unwrap();
    let direct = regular::encode(&view).unwrap();
    assert_eq!(direct.as_ref(), dto_bytes(regular::build(&view)));
    let document = json(&direct);
    for pointer in [
        "/beatmap/time/rate",
        "/beatmap/time/running",
        "/play/source",
    ] {
        assert!(document.pointer(pointer).is_none(), "{pointer}");
    }
    let filtered = regular::filtered(&view)
        .encode(&[
            Filter::Nested {
                field: "beatmap".into(),
                keys: vec![Filter::Nested {
                    field: "time".into(),
                    keys: vec![
                        Filter::Field("live".into()),
                        Filter::Field("rate".into()),
                        Filter::Field("running".into()),
                    ],
                }],
            },
            Filter::Nested {
                field: "play".into(),
                keys: vec![
                    Filter::Field("source".into()),
                    Filter::Field("score".into()),
                ],
            },
        ])
        .unwrap();
    assert_eq!(
        json(&filtered),
        json!({"beatmap":{"time":{"live":1234.5}},"play":{"score":123456.789}})
    );
}
