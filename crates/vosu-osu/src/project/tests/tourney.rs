use super::o5_tests::{Collect, facts, sample};
use super::*;
use serde_json::json;
#[test]
fn lazer_status_11_without_data_is_null() {
    let mut s = sample();
    Arc::make_mut(&mut s.precise).status = 11;
    let mut out = Collect::default();
    tourney::project(&s, &facts(ClientType::Lazer), &mut out);
    assert!(out.get(OsuPointer::OsuTourney).is_null());
}
#[test]
fn stable_and_lazer_non_11_publish_default_object() {
    let mut s = sample();
    for client in [ClientType::Stable, ClientType::Lazer] {
        for status in [0, 2, 7, 11] {
            if client == ClientType::Lazer && status == 11 {
                continue;
            }
            Arc::make_mut(&mut s.precise).status = status;
            let mut out = Collect::default();
            tourney::project(&s, &facts(client), &mut out);
            assert_eq!(
                out.get(OsuPointer::OsuTourney),
                &json!({"scoreVisible":false,"starsVisible":false,"ipcState":0,"bestOf":0,"team":{"left":"","right":""},"points":{"left":0,"right":0},"chat":[],"totalScore":{"left":0.0,"right":0.0},"clients":[]})
            );
        }
    }
}
#[test]
fn lazer_lobby_populates_clients() {
    let mut s = sample();
    Arc::make_mut(&mut s.precise).status = 11;
    let user = json!({"name":"spectated","accuracy":97.5,"rankedScore":123.0,"id":4,"level":1.0,"playCount":5,"playMode":3,"rank":6,"countryCode":1,"performancePoints":7.0,"rawBanchoStatus":0,"backgroundColour":0,"rawLoginStatus":0,"matchmaking":null});
    let h = json!({"perfect":11,"great":12,"good":13,"ok":14,"meh":15,"miss":0,"smallTickMiss":1,"smallTickHit":2,"largeTickMiss":3,"largeTickHit":4,"smallBonus":5,"largeBonus":6,"ignoreMiss":7,"ignoreHit":8,"comboBreak":9,"sliderTailHit":10});
    let mods =
        json!({"checksum":"HD","number":8,"name":"HD","array":[{"acronym":"HD"}],"rate":1.0});
    let score = json!({"failed":true,"retries":0,"playerName":"player","mods":mods,"mode":3,"score":12345.0,"playerHPSmooth":143.0,"playerHP":181.0,"accuracy":97.5,"statistics":h,"maximumStatistics":h,"combo":21,"maxCombo":34,"pp":12.345});
    Arc::make_mut(&mut s.regular).lazer_multi_spectating.data = Some(
        serde_json::from_value(
            json!({"chat":[],"spectatingClients":[{"team":"red","user":user,"score":score}]}),
        )
        .unwrap(),
    );
    let mut out = Collect::default();
    tourney::project(&s, &facts(ClientType::Lazer), &mut out);
    let t = out.get(OsuPointer::OsuTourney);
    assert_eq!(t["ipcState"], 11);
    assert_eq!(t["scoreVisible"], true);
    let client = &t["clients"][0];
    assert_eq!(client["user"]["name"], "spectated");
    assert_eq!(client["settings"]["mania"]["scrollSpeed"], 0);
    let p = &client["play"];
    assert_eq!(p["score"], 12345.0);
    assert_eq!(p["accuracy"], 97.5);
    assert_eq!(p["combo"], json!({"current":21,"max":34}));
    assert_eq!(p["judgements"]["good"], 13);
    assert_eq!(p["statistics"]["sliderEndHits"], 10);
    assert_eq!(p["rank"]["current"], "SH");
    assert_eq!(p["healthBar"]["normal"], (181.0 / 200.0) * 100.0);
    assert_eq!(p["failed"], false);
    assert_eq!(p["mods"]["name"], s.regular.global.menu_mods.name);
    assert_eq!(p["hitErrors"], json!({"all":[],"press":[],"release":[]}));
    let mut last = LastValues::default();
    let mut writer = vosu_plugin_api::PublishBatchWriter::new();
    writer.begin(61);
    tourney::project(
        &s,
        &facts(ClientType::Lazer),
        &mut DiffSink::new(&mut last, &mut writer),
    );
    writer.begin(61);
    let (_, allocs) = crate::test_alloc::allocations_during(|| {
        tourney::project(
            &s,
            &facts(ClientType::Lazer),
            &mut DiffSink::new(&mut last, &mut writer),
        )
    });
    assert_eq!(allocs, 0);
    assert!(writer.is_empty());
    let f = facts(ClientType::Lazer);
    let mut availability = tourney::AvailabilityCache::default();
    availability.update(&s, &f);
    assert!(!availability.take_transition());
    let score = Arc::make_mut(&mut s.regular)
        .lazer_multi_spectating
        .data
        .as_mut()
        .unwrap()
        .spectating_clients[0]
        .score
        .take()
        .unwrap();
    availability.update(&s, &f);
    assert!(availability.take_transition());
    availability.update(&s, &f);
    assert!(
        !availability.take_transition(),
        "one debug log per unavailable transition"
    );
    out.0.clear();
    tourney::project(&s, &f, &mut out);
    assert!(
        out.get(OsuPointer::OsuTourney).is_null(),
        "never publish a partial play"
    );
    Arc::make_mut(&mut s.regular)
        .lazer_multi_spectating
        .data
        .as_mut()
        .unwrap()
        .spectating_clients[0]
        .score = Some(score);
    availability.update(&s, &f);
    assert!(!availability.take_transition());
    Arc::make_mut(&mut s.regular)
        .lazer_multi_spectating
        .data
        .as_mut()
        .unwrap()
        .spectating_clients[0]
        .score = None;
    availability.update(&s, &f);
    assert!(
        availability.take_transition(),
        "a later unavailable transition is logged"
    );
}
