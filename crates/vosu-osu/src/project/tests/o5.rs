use super::*;
use serde_json::{Value, json};
use vosu_osu_model::states::HitErrorArray;
use vosu_osu_state::*;
pub(crate) fn sample() -> StateBundleSnapshot {
    StateBundleSnapshot {
        source: ClientType::Stable,
        generation: 1,
        reset_epoch: 0,
        regular: Arc::new(RegularState {
            global: GlobalState::default(),
            settings: SettingsState::default(),
            beatmap: BeatmapPPState::default(),
            menu: MenuState::default(),
            bass_density: BassDensityState::default(),
            gameplay: GameplayState::default(),
            result_screen: ResultScreenState::default(),
            tourney: TourneyManagerState::default(),
            user: UserState::default(),
            lazer_multi_spectating: LazerMultiSpectatingState::default(),
            ranked_play: RankedPlayState::default(),
            room: Default::default(),
        }),
        precise: Arc::new(PreciseState {
            catcher: None,
            status: 0,
            play_time: 0.0,
            mode: 0,
            key_overlay: Arc::new(vec![]),
            keypresses: Arc::new(vec![]),
            hit_errors: Arc::new(HitErrorArray::default()),
            total_hit_errors: 0.0,
            committed_hit_error_index: 0,
            unstable_rate: 0.0,
        }),
    }
}
pub(super) fn facts(client: ClientType) -> SessionFacts {
    SessionFacts {
        client,
        settings: vosu_osu_model::launch::InstanceSettings {
            tournament_spectator: false,
            tournament_ipc_id: None,
            devserver_endpoint: None,
        },
        foreground: None,
    }
}
#[derive(Default)]
pub(super) struct Collect(pub Vec<(OsuPointer, Value)>);
impl ProjectionSink for Collect {
    fn put(&mut self, at: OsuPointer, v: vosu_plugin_api::PublishValueRef<'_>) {
        use vosu_plugin_api::PublishValueRef as V;
        let v = match v {
            V::Null => Value::Null,
            V::Bool(b) => json!(b),
            V::String(s) => json!(s),
            V::F64(n) => json!(n),
            _ => panic!("unexpected scalar"),
        };
        self.0.push((at, v))
    }
    fn put_cbor(&mut self, at: OsuPointer, f: &mut dyn FnMut(&mut Vec<u8>)) {
        let mut bytes = vec![];
        f(&mut bytes);
        let v: Value = vosu_plugin_api::decode(&bytes).unwrap();
        self.0.push((at, v));
    }
}
impl Collect {
    pub(super) fn get(&self, at: OsuPointer) -> &Value {
        &self.0.iter().find(|(p, _)| *p == at).unwrap().1
    }
}
#[test]
fn settings_all_groups_follow_v2_defaults_and_overrides() {
    let mut s = sample();
    let r = Arc::make_mut(&mut s.regular);
    r.settings.values=serde_json::from_value(json!({"mania":{"scrollSpeed":2.5,"scrollDirection":99,"timingBasedNoteColouring":true,"perBeatmapScrollSpeed":1.25},"visual":{"hitLighting":true},"skin":{"name":"not-a-toggle","tintSliderBall":true},"keybinds":{"fruits":{"Dash":"Shift"}},"scoreMeter":{"size":null}})).unwrap();
    let mut out = Collect::default();
    settings::project(&s, &facts(ClientType::Lazer), &mut out);
    assert_eq!(out.0.len(), 21);
    assert_eq!(
        out.get(OsuPointer::OsuSettingsMania),
        &json!({"speedBPMScale":false,"usePerBeatmapSpeedScale":false,"scrollSpeed":2,"scrollDirection":{"number":99},"scrollSpeedValue":2.5,"scrollTime":4594.0,"timingBasedNoteColouring":true,"perBeatmapScrollSpeed":1.25})
    );
    assert_eq!(
        out.get(OsuPointer::OsuSettingsSkin),
        &json!({"useDefaultSkinInEditor":false,"ignoreBeatmapSkins":false,"tintSliderBall":true,"useTaikoSkin":false})
    );
    assert_eq!(
        out.get(OsuPointer::OsuSettingsKeybinds)["fruits"]["Dash"],
        "Shift"
    );
    assert_eq!(out.get(OsuPointer::OsuSettingsVisual)["hitLighting"], true);
    assert!(out.get(OsuPointer::OsuSettingsScoreMeter)["size"].is_null());
    for at in [
        OsuPointer::OsuSettingsResolution,
        OsuPointer::OsuSettingsClient,
        OsuPointer::OsuSettingsCursor,
        OsuPointer::OsuSettingsMouse,
        OsuPointer::OsuSettingsTablet,
        OsuPointer::OsuSettingsBackground,
        OsuPointer::OsuSettingsAudio,
        OsuPointer::OsuSettingsLeaderboard,
    ] {
        assert!(out.get(at).is_object(), "{at:?}")
    }
}
#[test]
fn profile_all_fields_match_v2() {
    let mut s = sample();
    let mut out = Collect::default();
    profile::project(&s, &mut out);
    assert_eq!(
        out.get(OsuPointer::OsuProfile)["userStatus"],
        json!({"number":null,"name":""})
    );
    Arc::make_mut(&mut s.regular).user.profile=Some(serde_json::from_value(json!({"name":"player","accuracy":98.5,"rankedScore":123.0,"id":42,"level":99.0,"playCount":12,"playMode":3,"rank":7,"countryCode":1,"performancePoints":456.0,"rawBanchoStatus":0,"backgroundColour":-255,"rawLoginStatus":0,"matchmaking":{"rating":12.5,"rank":null,"plays":4,"wins":3,"isProvisional":true}})).unwrap());
    out.0.clear();
    profile::project(&s, &mut out);
    let p = out.get(OsuPointer::OsuProfile);
    assert_eq!(p.as_object().unwrap().len(), 14);
    assert_eq!(p["backgroundColour"], "-ff");
    assert_eq!(p["name"], "player");
    assert_eq!(p["pp"], 456.0);
    assert_eq!(p["matchmaking"]["wins"], 3);
    assert_eq!(p["mode"]["number"], 3);
}
#[test]
fn room_and_ranked_play_preserve_values_and_null() {
    let mut s = sample();
    let mut out = Collect::default();
    room::project(&s, &mut out);
    assert!(out.get(OsuPointer::OsuRoom).is_null());
    assert!(out.get(OsuPointer::OsuRankedPlay).is_null());
    let r = Arc::make_mut(&mut s.regular);
    r.room.info = Some(
        serde_json::from_value(
            json!({"roomID":17,"users":[{"id":1,"username":"u","countryCode":4}]}),
        )
        .unwrap(),
    );
    let ranked = json!({"stage":2,"currentRound":3,"starRating":6.5,"damageMultiplier":1.2,"activeUserId":1,"winningUserId":2,"users":[{"id":1,"rating":9.0,"life":80.0,"roundsWon":3,"damageMultiplier":1.5}]});
    r.ranked_play.info = Some(serde_json::from_value(ranked.clone()).unwrap());
    out.0.clear();
    room::project(&s, &mut out);
    assert_eq!(out.get(OsuPointer::OsuRankedPlay), &ranked);
    assert_eq!(
        out.get(OsuPointer::OsuRoom),
        &json!({"roomId":17,"users":[{"id":1,"username":"u","countryCode":4}]})
    );
    let schema: Value = serde_json::from_str(crate::OSU_NAMESPACE_SCHEMA).unwrap();
    assert_eq!(
        schema["properties"]["rankedPlay"]["oneOf"][0]["properties"]["stage"]["type"],
        "integer"
    );
}
#[test]
fn internal_paths_per_variant() {
    let mut s = sample();
    let r = Arc::make_mut(&mut s.regular);
    r.global.game_folder = "/game".into();
    r.global.songs_folder = "/songs".into();
    r.global.skin_folder = "/skin".into();
    r.menu.folder = Some("map".into());
    r.menu.filename = Some("map.osu".into());
    let mut out = Collect::default();
    internal::project(&s, &facts(ClientType::Stable), &mut out);
    let p = out.get(OsuPointer::OsuInternalPaths);
    assert_eq!(p["stable"]["game"], "/game");
    assert_eq!(
        p["stable"]["skins"],
        std::path::Path::new("/game")
            .join("Skins")
            .to_string_lossy()
            .as_ref()
    );
    assert!(p["lazer"].is_null());
    assert_eq!(p["skinFolder"], "/skin");
    assert_eq!(
        p["current"],
        json!({"beatmapFolder":"map","beatmapFile":"map.osu","background":null,"audio":null})
    );
    out.0.clear();
    internal::project(&s, &facts(ClientType::Lazer), &mut out);
    let p = out.get(OsuPointer::OsuInternalPaths);
    assert!(p["stable"].is_null());
    assert_eq!(p["lazer"], json!({"data":"/game","files":"/songs"}));
    assert!(p["skinFolder"].is_null());
}
#[test]
fn o5_diff_sink_steady_state_allocates_nothing() {
    let s = sample();
    let f = facts(ClientType::Stable);
    let mut last = LastValues::default();
    let mut writer = vosu_plugin_api::PublishBatchWriter::new();
    let mut cache = skin::SkinCache::default();
    writer.begin(51);
    let mut sink = DiffSink::new(&mut last, &mut writer);
    settings::project_all(&s, &f, &mut sink);
    cache.project(&f, &s, &mut sink);
    writer.begin(51);
    let (_, allocs) = crate::test_alloc::allocations_during(|| {
        let mut sink = DiffSink::new(&mut last, &mut writer);
        settings::project_all(&s, &f, &mut sink);
        cache.project(&f, &s, &mut sink)
    });
    assert_eq!(allocs, 0);
    assert!(writer.is_empty());
}
