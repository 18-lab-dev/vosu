//! Synthetic clones exercise mapped values without altering the pinned corpus.
use super::{Value, json, oracle};

pub fn seeded(original: &oracle::Input) -> oracle::Input {
    let mut input = original.clone();
    if input.source.is_none() {
        return input;
    }
    let s = &mut input.services;
    for (key, leaf) in [
        ("gameFolder", "install"),
        ("songsFolder", "songs"),
        ("skinFolder", "skin"),
    ] {
        s.global[key] = json!(format!("/compat-private/{}/{leaf}", input.id));
    }
    for (key, leaf) in [
        ("folder", "chart-dir"),
        ("filename", "chart.osu"),
        ("audioFilename", "audio.ogg"),
        ("backgroundFilename", "background.jpg"),
    ] {
        s.menu[key] = json!(format!("{}-{leaf}", input.id));
    }
    s.global["precisePlayTime"] = json!(2500.0);
    s.gameplay["score"] = json!(1234567);
    s.gameplay["combo"] = json!(71);
    s.gameplay["maxCombo"] = json!(89);
    s.gameplay["playerHP"] = json!(142);
    s.gameplay["playerHPSmooth"] = json!(118);
    s.gameplay["accuracy"] = json!(97.125);
    for (key, count) in [
        ("perfect", 11),
        ("great", 23),
        ("good", 37),
        ("ok", 41),
        ("meh", 53),
        ("miss", 67),
        ("smallTickHit", 79),
        ("largeTickHit", 83),
        ("sliderTailHit", 97),
    ] {
        s.gameplay["statistics"][key] = json!(count);
    }
    s.gameplay["hitSB"] = json!(13);
    s.gameplay["hitErrors"] = json!([1.25, -2.5]);
    s.gameplay["pressHitErrors"] = json!([3.75]);
    s.gameplay["releaseHitErrors"] = json!([-4.5]);
    s.gameplay["hitOffsets"] = json!([5.25, -6.5]);
    if input.mode == 3 {
        s.gameplay["keyHitErrors"] = json!({"press":[[7.75],[-8.25],[9.5],[-10.75]],"release":[[-11.5],[12.25],[-13.75],[14.5]]});
    }
    if input.mode == 3 {
        s.gameplay["keyOverlay"] = Value::Array(
            (0..4)
                .map(|i| json!({"name":format!("B{}",i+1),"isPressed":i%2==0,"count":i+17}))
                .collect(),
        );
    }
    // The committed catch case is a result screen. Its clone exercises the
    // active catcher lane as well; the original still checks results gating.
    if input.mode == 2 {
        input.status = 2;
        s.gameplay["catcher"] = json!({"x":123.5,"dashing":true});
    }
    s.beatmap_pp["keyCount"] = json!(if input.mode == 3 { 4 } else { 0 });
    // Exact 320 ms beat length avoids the .osu parser's timing precision
    // becoming the subject of this BPM-rate mapping test.
    s.beatmap_pp["commonBPM"] = json!(187.5);
    s.beatmap_pp["minBPM"] = json!(187.5);
    s.beatmap_pp["maxBPM"] = json!(187.5);
    let mods = if input.status == 2 {
        &s.gameplay["mods"]
    } else {
        &s.global["menuMods"]
    };
    s.beatmap_pp["clockRate"] = mods["rate"].clone();
    s.room = json!({"roomID":314,"users":[{"id":271,"username":"Room player","countryCode":111,"avatarUrl":"https://example.invalid/avatar"}]});
    s.ranked_play = json!({"stage":2,"currentRound":3,"starRating":6.5,"damageMultiplier":1.2,"activeUserId":1,"winningUserId":2,"users":[{"id":1,"rating":9.0,"life":80.0,"roundsWon":3,"damageMultiplier":1.5}]});
    input
}

pub fn coverage(input: &oracle::Input, v2: &Value, precise: &Value) {
    assert_ne!(v2["folders"]["game"], v2["folders"]["songs"]);
    for group in ["folders", "files", "directPath"] {
        assert!(
            v2[group]
                .as_object()
                .unwrap()
                .values()
                .all(|v| v.as_str().is_some_and(|s| !s.is_empty() && s != ".")),
            "{}: empty path source {group}",
            input.id
        );
    }
    assert!(v2["beatmap"]["time"]["lastObject"].as_f64().unwrap() > 2500.0);
    assert!(!v2["rankedPlay"].is_null());
    assert!(!v2["room"].is_null());
    if input.status == 2 {
        assert_eq!(v2["beatmap"]["time"]["live"].as_f64(), Some(2500.0));
        assert!(v2["play"]["score"].as_f64().unwrap() > 0.0);
        assert_ne!(v2["play"]["combo"]["current"], v2["play"]["combo"]["max"]);
        assert!(v2["play"]["healthBar"]["normal"].as_f64().unwrap() > 0.0);
        assert!(!precise["hitErrors"].as_array().unwrap().is_empty());
    }
    if input.id == "lazer-play-mania-cl" {
        assert!(
            !precise["keyHitErrors"]["press"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            precise["keyHitErrors"]["press"]
                .as_array()
                .unwrap()
                .iter()
                .all(|column| !column.as_array().unwrap().is_empty()),
            "§7 row 251 must exercise populated mania key errors"
        );
        assert_eq!(precise["keys"]["all"].as_array().unwrap().len(), 4);
        let counts: std::collections::BTreeSet<_> = ["geki", "300", "katu", "100", "50", "0"]
            .map(|key| v2["play"]["hits"][key].as_u64().unwrap())
            .into();
        assert_eq!(counts.len(), 6, "all judgement counts must be distinct");
        assert!(!counts.contains(&0));
    }
    if input.id == "stable-play-osu-fl" {
        assert!(
            !v2["leaderboard"].as_array().unwrap().is_empty(),
            "§7 row 261 requires a populated leaderboard"
        );
        assert!(!precise["hitOffsets"].as_array().unwrap().is_empty());
        assert_ne!(precise["hitOffsets"], precise["hitErrors"]);
    }
    if matches!(
        input.id.as_str(),
        "lazer-play-taiko-dt" | "stable-result-catch-ht"
    ) {
        assert!(input.services.beatmap_pp["commonBPM"].as_f64().unwrap() > 0.0);
        assert_ne!(
            v2["beatmap"]["stats"]["bpm"]["common"], input.services.beatmap_pp["commonBPM"],
            "rate cases must distinguish clock BPM from map BPM"
        );
    }
    if input.mode == 2 {
        assert!(!precise["catcher"].is_null());
    }
}
