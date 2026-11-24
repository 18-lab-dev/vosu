use super::*;

/// Every committed oracle source and nonzero seed traverses the real batch/store.
pub(super) fn check(s: &StateBundleSnapshot, doc: &Value, v2: &Value, precise: &Value) {
    let o = &doc["vosu.osu"];
    let g = &s.regular.gameplay;
    assert_eq!(o["play"]["score"].as_f64(), Some(g.score));
    assert_eq!(o["play"]["accuracyPercent"].as_f64(), Some(g.accuracy));
    assert_eq!(o["play"]["healthRaw"]["normal"].as_f64(), Some(g.player_hp));
    assert_eq!(
        o["play"]["healthRaw"]["smooth"].as_f64(),
        Some(g.player_hp_smooth)
    );
    assert_eq!(
        o["play"]["combo"],
        json!({"current":g.combo,"max":g.max_combo})
    );
    assert_eq!(o["play"]["retries"], json!(g.retries));
    assert_eq!(
        o["play"]["preciseUnstableRate"].as_f64(),
        Some(s.precise.unstable_rate)
    );
    assert_eq!(
        o["play"]["judgements"],
        json!({"miss":g.statistics.miss,"meh":g.statistics.meh,"ok":g.statistics.ok,"good":g.statistics.good,"great":g.statistics.great,"perfect":g.statistics.perfect})
    );
    assert_eq!(
        o["session"]["watchingReplay"],
        s.regular.global.is_watching_replay
    );
    assert_eq!(
        o["beatmap"]["selectionMode"],
        json!(s.regular.menu.gamemode)
    );
    assert_eq!(
        o["beatmap"]["time"]["firstHitObject"],
        json!(
            s.regular
                .beatmap
                .parsed
                .as_ref()
                .map(|p| p.times.first_object)
        )
    );
    assert_eq!(
        o["beatmap"]["time"]["preview"],
        json!(s.regular.beatmap.parsed.as_ref().map(|p| p.times.preview))
    );
    let p = &g.leaderboard_player;
    assert_eq!(o["leaderboardState"]["visible"], g.leaderboard_visible);
    assert_eq!(o["leaderboardState"]["player"]["name"], p.name);
    assert_eq!(
        o["leaderboardState"]["player"]["score"].as_f64(),
        Some(p.score)
    );
    for (i, key) in ["k1", "k2", "m1", "m2"].into_iter().enumerate() {
        let actual = &o["keys"]["overlay"][key];
        if let Some(slot) = s.precise.key_overlay.get(i) {
            assert_eq!(
                actual,
                &json!({"isPressed":slot.is_pressed,"count":slot.count})
            );
            // Pinned legacy writer is the byte/rounding oracle, not a reverse transform.
            if !precise.is_null() && precise.get("keys").is_some() {
                assert_eq!(actual, &precise["keys"][key]);
            }
        } else {
            assert!(actual.is_null());
        }
    }
    if let Some(play) = v2.get("play") {
        equal(
            "source fidelity",
            "accuracy",
            &play["accuracy"],
            &o["play"]["accuracyPercent"],
        );
        equal(
            "source fidelity",
            "health",
            &play["healthBar"]["normal"],
            &json!((g.player_hp / 200.0) * 100.0),
        );
    }
}

pub(super) fn independent_sources(
    s: &StateBundleSnapshot,
    doc: &Value,
    private: &Value,
    v1: &Value,
) {
    let primary = &doc["vosu.osu"]["performance"]["graph"]["primary"];
    equal(
        "source fidelity",
        "primary",
        &json!(s.regular.beatmap.graph.primary),
        primary,
    );
    equal(
        "source fidelity",
        "v1 strains",
        &v1["menu"]["pp"]["strains"],
        primary,
    );
    let current = &private["vosu.osu"]["internal"]["paths"]["current"];
    for (source, legacy) in [
        ("beatmapFolder", "folder"),
        ("beatmapFile", "file"),
        ("background", "bg"),
        ("audio", "audio"),
    ] {
        assert_eq!(&current[source], &v1["menu"]["bm"]["path"][legacy]);
    }
}

#[test]
fn independent_file_names_match_legacy_v1_through_bus_for_every_presence_mask() {
    for mask in 0_u8..16 {
        let mut input = load_input("stable-defaults/input.json");
        for (index, key, value) in [
            (0, "folder", "directory"),
            (1, "filename", "chart.osu"),
            (2, "backgroundFilename", "背景.png"),
            (3, "audioFilename", ""),
        ] {
            input.services.menu[key] = if mask & (1 << index) != 0 {
                json!(value)
            } else {
                Value::Null
            };
        }
        let mut bus = Bus::new();
        bus.open(&input, 10);
        let snapshot = oracle::state(&input);
        assert!(bus.publish(&snapshot, &facts(&input)) > 0);
        let public = bus.document(Audience::Api);
        let grant = InternalGrant::from_subtrees(vec![Pointer::parse("/internal/paths").unwrap()]);
        let private = bus.document(Audience::Plugin(&grant));
        let manager = oracle::manager_with(
            &input,
            ApiRuntimeSettings {
                enable_key_overlay: true,
                enable_mania_key_hit_errors: true,
                enable_hit_offsets: true,
            },
        );
        let v1: Value = serde_json::from_slice(&oracle::encode("v1", &manager)).unwrap();
        independent_sources(&snapshot, &public, &private, &v1);
        assert!(public["vosu.osu"].get("internal").is_none());
        if mask != 0 {
            assert!(private["vosu.osu"]["internal"]["paths"]["current"].is_object());
        }
    }
}
