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
