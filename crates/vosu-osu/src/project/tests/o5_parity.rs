//! These committed 1.x responses guard every scalar under O5's mapping rows.
use super::o5_tests::{Collect, facts, sample};
use super::*;
use serde_json::Value;
fn eq(actual: &Value, expected: &Value, at: &str) {
    match (actual, expected) {
        (Value::Number(a), Value::Number(b)) => assert_eq!(a.as_f64(), b.as_f64(), "{at}"),
        (Value::Array(a), Value::Array(b)) => {
            assert_eq!(a.len(), b.len(), "{at}");
            for (i, (a, b)) in a.iter().zip(b).enumerate() {
                eq(a, b, &format!("{at}/{i}"))
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            assert_eq!(a.len(), b.len(), "{at}: key count");
            for (k, b) in b {
                eq(
                    a.get(k).unwrap_or_else(|| panic!("{at}/{k}: absent")),
                    b,
                    &format!("{at}/{k}"),
                )
            }
        }
        _ => assert_eq!(actual, expected, "{at}"),
    }
}
#[test]
fn all_oracle_settings_profile_room_ranked_and_tourney_mapping_rows() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/oracle");
    let mut count = 0;
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if !path.is_dir() || path.file_name().unwrap() == "no-client" {
            continue;
        }
        let input: Value =
            serde_json::from_slice(&std::fs::read(path.join("input.json")).unwrap()).unwrap();
        let expected: Value =
            serde_json::from_slice(&std::fs::read(path.join("v2.json")).unwrap()).unwrap();
        let svc = &input["services"];
        let client = if input["source"] == "lazer" {
            ClientType::Lazer
        } else {
            ClientType::Stable
        };
        let f = facts(client);
        let mut s = sample();
        s.source = client;
        Arc::make_mut(&mut s.precise).status = input["status"].as_i64().unwrap();
        let r = Arc::make_mut(&mut s.regular);
        r.settings.values = serde_json::from_value(svc["settings"].clone()).unwrap();
        r.menu.gamemode = svc["menu"]["gamemode"].as_i64();
        r.global.show_interface = svc["global"]["showInterface"].as_bool().unwrap();
        r.global.is_replay_ui_hidden = svc["global"]["isReplayUiHidden"].as_bool().unwrap();
        r.global.chat_status = svc["global"]["chatStatus"].as_i64().unwrap();
        r.bass_density.density = svc["bassDensity"]["density"].as_f64().unwrap();
        r.gameplay.leaderboard_visible = svc["gameplay"]["isLeaderboardVisible"]
            .as_bool()
            .unwrap_or(false);
        r.global.menu_mods = serde_json::from_value(svc["global"]["menuMods"].clone()).unwrap();
        r.user.profile = Some(serde_json::from_value(svc["user"].clone()).unwrap());
        if !svc["lazerMultiSpectating"]["lazerSpectatingData"].is_null() {
            r.lazer_multi_spectating.data = Some(
                serde_json::from_value(svc["lazerMultiSpectating"]["lazerSpectatingData"].clone())
                    .unwrap(),
            )
        }
        let mut out = Collect::default();
        settings::project(&s, &f, &mut out);
        profile::project(&s, &mut out);
        room::project(&s, &mut out);
        tourney::project(&s, &f, &mut out);
        for (at, v) in &out.0 {
            if at.pointer().starts_with("/settings/") {
                let mut e = if *at == OsuPointer::OsuSettingsVisual {
                    expected
                        .pointer(at.pointer())
                        .unwrap_or(&Value::Null)
                        .clone()
                } else {
                    expected
                        .pointer(at.pointer())
                        .unwrap_or_else(|| panic!("{} {}", input["id"], at.pointer()))
                        .clone()
                };
                if *at == OsuPointer::OsuSettingsSkin {
                    e.as_object_mut().unwrap().remove("name");
                }
                eq(v, &e, at.pointer());
            }
        }
        eq(
            out.get(OsuPointer::OsuProfile),
            &expected["profile"],
            "profile",
        );
        eq(out.get(OsuPointer::OsuRoom), &expected["room"], "room");
        eq(
            out.get(OsuPointer::OsuRankedPlay),
            &expected["rankedPlay"],
            "rankedPlay",
        );
        {
            // This is the single-session projector without a registered manager;
            // production stable aggregation is tested through the plugin host.
            let mut e = if input["id"] == "stable-tournament" {
                serde_json::json!({"scoreVisible":false,"starsVisible":false,"ipcState":0,"bestOF":0,"team":{"left":"","right":""},"points":{"left":0,"right":0},"chat":[],"totalScore":{"left":0,"right":0},"clients":[]})
            } else {
                expected["tourney"].clone()
            };
            if let Some(best) = e.as_object_mut().and_then(|o| o.remove("bestOF")) {
                e.as_object_mut().unwrap().insert("bestOf".into(), best);
            }
            if let Some(clients) = e["clients"].as_array_mut() {
                for client in clients {
                    let index = client["ipcId"].as_f64().unwrap() as usize;
                    client
                        .as_object_mut()
                        .unwrap()
                        .insert("keysOverlay".into(), Value::Null);
                    let p = client["play"].as_object_mut().unwrap();
                    p.insert("source".into(), Value::String("spectator".into()));
                    let all = p.remove("hitErrorArray").unwrap();
                    p.insert(
                        "hitErrors".into(),
                        serde_json::json!({"all":all,"press":[],"release":[]}),
                    );
                    let h = p.remove("hits").unwrap();
                    p.insert("judgements".into(),serde_json::json!({"miss":h["0"],"meh":h["50"],"ok":h["100"],"good":h["katu"],"great":h["300"],"perfect":h["geki"]}));
                    let score = &s
                        .regular
                        .lazer_multi_spectating
                        .data
                        .as_ref()
                        .unwrap()
                        .spectating_clients[index]
                        .score
                        .as_ref()
                        .unwrap();
                    let raw = &score.statistics;
                    p.insert("healthRaw".into(), serde_json::json!({"normal":score.player_hp,"smooth":score.player_hp_smooth}));
                    p.insert("rawJudgements".into(), serde_json::json!({"miss":raw.miss,"meh":raw.meh,"ok":raw.ok,"good":raw.good,"great":raw.great,"perfect":raw.perfect}));
                    p.insert("preciseUnstableRate".into(), serde_json::json!(0.0));
                    p.insert("statistics".into(),serde_json::json!({"smallTickMiss":raw.small_tick_miss,"smallTickHit":h["smallTickHits"],"largeTickMiss":raw.large_tick_miss,"largeTickHit":h["largeTickHits"],"smallBonus":raw.small_bonus,"largeBonus":raw.large_bonus,"ignoreMiss":raw.ignore_miss,"ignoreHit":raw.ignore_hit,"comboBreak":raw.combo_break,"sliderTailHit":raw.slider_tail_hit,"sliderBreaks":h["sliderBreaks"],"sliderEndHits":h["sliderEndHits"],"good":h["katu"],"legacyComboIncrease":raw.legacy_combo_increase}));
                }
            }
            eq(out.get(OsuPointer::OsuTourney), &e, "tourney");
        }
        count += 1;
    }
    assert_eq!(count, 12);
}
