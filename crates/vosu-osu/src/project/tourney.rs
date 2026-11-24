//! Lazer lobby projection; the stable manager aggregate is intentionally deferred (D6).
use super::settings::{Cbor, emit};
use super::{OsuPointer, ProjectionSink, SessionFacts};
use vosu_osu_model::{
    enums::ClientType, memory::LazerSpectatorEntry, names::*, states::MultiplayerTeamType,
};
use vosu_osu_state::StateBundleSnapshot;
pub(super) fn missing_score(snap: &StateBundleSnapshot, facts: &SessionFacts) -> bool {
    facts.client == ClientType::Lazer && snap.precise.status == 11
        && snap.regular.lazer_multi_spectating.data.as_ref().is_some_and(|d|
            d.spectating_clients.iter().any(|c| c.score.is_none()))
}
#[derive(Default)]
pub(super) struct AvailabilityCache { unavailable: bool, pending: bool }
impl AvailabilityCache {
    pub(super) fn update(&mut self, snap: &StateBundleSnapshot, facts: &SessionFacts) {
        let unavailable = missing_score(snap, facts);
        self.pending |= unavailable && !self.unavailable;
        self.unavailable = unavailable;
    }
    pub(super) fn take_transition(&mut self) -> bool { std::mem::take(&mut self.pending) }
}

pub(super) fn project(
    snap: &StateBundleSnapshot,
    facts: &SessionFacts,
    sink: &mut dyn ProjectionSink,
) {
    emit(sink, OsuPointer::OsuTourney, |c| {
        let active = facts.client == ClientType::Lazer && snap.precise.status == 11;
        let data = active
            .then_some(snap.regular.lazer_multi_spectating.data.as_ref())
            .flatten();
        // 1.x tourney.rs:225-228 bubbles this through providers.rs:30-33 as a
        // whole-response HTTP 500. The bus publishes unavailable data instead
        // of propagating that defect or fabricating a partial client play.
        if active && (data.is_none() || missing_score(snap, facts)) {
            c.null();
            return;
        }
        c.map(9);
        c.text("scoreVisible");
        c.boolean(active);
        c.text("starsVisible");
        c.boolean(false);
        c.text("ipcState");
        c.int(if active { 11 } else { 0 });
        c.text("bestOf");
        c.int(0);
        c.text("team");
        c.map(2);
        for side in ["left", "right"] {
            c.text(side);
            c.text("")
        }
        c.text("points");
        c.map(2);
        for side in ["left", "right"] {
            c.text(side);
            c.int(0)
        }
        c.text("chat");
        c.array(data.map_or(0, |d| d.chat.len()));
        if let Some(d) = data {
            for m in &d.chat {
                c.map(4);
                c.text("team");
                c.text("none");
                c.text("timestamp");
                c.text(&m.time);
                c.text("name");
                c.text(&m.name);
                c.text("message");
                c.text(&m.content)
            }
        }
        c.text("totalScore");
        c.map(2);
        for (side, team) in [
            ("left", MultiplayerTeamType::Red),
            ("right", MultiplayerTeamType::Blue),
        ] {
            c.text(side);
            c.float(data.map_or(0.0, |d| {
                d.spectating_clients
                    .iter()
                    .filter(|v| v.team == team)
                    .map(|v| v.score.as_ref().map_or(0.0, |s| s.score))
                    .sum()
            }))
        }
        c.text("clients");
        c.array(data.map_or(0, |d| d.spectating_clients.len()));
        if let Some(d) = data {
            for (i, client) in d.spectating_clients.iter().enumerate() {
                client_value(c, snap, client, i)
            }
        }
    });
}
fn client_value(
    c: &mut Cbor<'_>,
    snap: &StateBundleSnapshot,
    client: &LazerSpectatorEntry,
    index: usize,
) {
    c.map(6);
    c.text("ipcId");
    c.float(index as f64);
    c.text("team");
    c.text(if client.team == MultiplayerTeamType::Red {
        "left"
    } else {
        "right"
    });
    c.text("settings");
    c.map(1);
    c.text("mania");
    c.map(1);
    c.text("scrollSpeed");
    c.int(0);
    let u = &client.user;
    c.text("user");
    c.map(8);
    c.text("id");
    c.int(u.id);
    c.text("name");
    c.text(&u.name);
    c.text("country");
    c.text(source_country_name(u.country_code));
    c.text("accuracy");
    c.float(u.accuracy);
    c.text("rankedScore");
    c.float(u.ranked_score);
    c.text("playCount");
    c.int(u.play_count);
    c.text("globalRank");
    c.int(u.rank);
    c.text("totalPP");
    c.float(u.performance_points);
    c.text("beatmap");
    c.map(1);
    c.text("stats");
    zero_stats(c, snap);
    c.text("play");
    let Some(s) = client.score.as_ref() else {
        c.null();
        return;
    };
    c.map(15);
    c.text("source");
    c.text("spectator");
    c.text("failed");
    c.boolean(false);
    c.text("playerName");
    c.text(&s.player_name);
    c.text("mode");
    c.number_name(Some(s.mode), ruleset_name(s.mode));
    c.text("score");
    c.float(s.score);
    c.text("accuracy");
    c.float(s.accuracy);
    c.text("combo");
    c.map(2);
    c.text("current");
    c.int(s.combo);
    c.text("max");
    c.int(s.max_combo);
    c.text("healthBar");
    c.map(2);
    c.text("normal");
    c.float((s.player_hp / 200.0) * 100.0);
    c.text("smooth");
    c.float((s.player_hp_smooth / 200.0) * 100.0);
    let h = &s.statistics;
    let hits = vosu_osu_model::to_legacy_hits(s.mode as i32, h);
    c.text("judgements");
    c.map(6);
    for (key, n) in [
        ("miss", hits.n0),
        ("meh", hits.n50),
        ("ok", hits.n100),
        ("good", hits.katu),
        ("great", hits.n300),
        ("perfect", hits.geki),
    ] {
        c.text(key);
        c.int(n)
    }
    c.text("statistics");
    c.map(13);
    for (key, n) in [
        ("smallTickMiss", h.small_tick_miss),
        ("smallTickHit", h.small_tick_hit),
        ("largeTickMiss", h.large_tick_miss),
        ("largeTickHit", h.large_tick_hit),
        ("smallBonus", h.small_bonus),
        ("largeBonus", h.large_bonus),
        ("ignoreMiss", h.ignore_miss),
        ("ignoreHit", h.ignore_hit),
        ("comboBreak", h.combo_break),
        ("sliderTailHit", h.slider_tail_hit),
        ("sliderBreaks", 0),
        ("sliderEndHits", h.slider_tail_hit),
    ] {
        c.text(key);
        c.int(n)
    }
    c.text("legacyComboIncrease");
    if let Some(n) = h.legacy_combo_increase {
        c.int(n)
    } else {
        c.null()
    }
    c.text("hitErrors");
    c.map(3);
    for key in ["all", "press", "release"] {
        c.text(key);
        c.array(0)
    }
    // Status 11 uses the menu mods, not the spectator score mods (1.x).
    let m = &snap.regular.global.menu_mods;
    c.text("mods");
    c.map(5);
    c.text("checksum");
    c.text(&m.checksum);
    c.text("number");
    c.int(m.number);
    c.text("name");
    c.text(&m.name);
    c.text("rate");
    c.float(m.rate);
    c.text("array");
    c.array(m.array.len());
    for m in &m.array {
        c.map(1 + usize::from(m.settings.is_some()));
        c.text("acronym");
        c.text(&m.acronym);
        if let Some(settings) = &m.settings {
            c.text("settings");
            c.map(settings.len());
            for (k, v) in settings {
                c.text(k);
                c.json(v)
            }
        }
    }
    let rank = grade(s);
    c.text("rank");
    c.map(2);
    for key in ["current", "maxThisPlay"] {
        c.text(key);
        c.text(rank)
    }
    c.text("pp");
    c.map(5);
    c.text("current");
    c.float(vosu_osu_model::fix_decimals_to(s.pp.unwrap_or(0.0), 2).unwrap_or(f64::NAN));
    for key in ["fc", "maxAchieved", "maxAchievable"] {
        c.text(key);
        c.float(0.0)
    }
    c.text("detailed");
    c.map(2);
    for key in ["current", "fc"] {
        c.text(key);
        c.map(6);
        for key in [
            "aim",
            "speed",
            "accuracy",
            "difficulty",
            "flashlight",
            "total",
        ] {
            c.text(key);
            c.float(0.0)
        }
    }
    c.text("unstableRate");
    c.float(0.0);
}
fn grade(s: &vosu_osu_model::memory::Score) -> &'static str {
    let silver = s
        .mods
        .array
        .iter()
        .any(|m| matches!(m.acronym.as_str(), "HD" | "FL"));
    let a = s.accuracy / 100.0;
    if !(0..=3).contains(&s.mode) {
        return "";
    }
    if a == 1.0 {
        return if silver { "XH" } else { "X" };
    }
    let (s_threshold, a_threshold, b_threshold, c_threshold) = if s.mode == 2 {
        (0.98, 0.94, 0.9, 0.85)
    } else {
        (0.95, 0.9, 0.8, 0.7)
    };
    if a >= s_threshold && (s.mode >= 2 || s.statistics.miss == 0) {
        if silver { "SH" } else { "S" }
    } else if a >= a_threshold {
        "A"
    } else if a >= b_threshold {
        "B"
    } else if a >= c_threshold {
        "C"
    } else {
        "D"
    }
}
fn zero_stats(c: &mut Cbor<'_>, snap: &StateBundleSnapshot) {
    c.map(9);
    c.text("stars");
    c.map(11);
    for key in [
        "live",
        "aim",
        "speed",
        "flashlight",
        "sliderFactor",
        "stamina",
        "rhythm",
        "color",
        "reading",
        "hitWindow",
        "total",
    ] {
        c.text(key);
        c.float(0.0)
    }
    for key in ["ar", "cs", "od", "hp"] {
        c.text(key);
        c.map(2);
        for key in ["original", "converted"] {
            c.text(key);
            c.float(0.0)
        }
    }
    c.text("bpm");
    c.map(4);
    for key in ["realtime", "common", "min", "max"] {
        c.text(key);
        c.float(0.0)
    }
    c.text("objects");
    c.map(5);
    for key in ["circles", "sliders", "spinners", "holds", "total"] {
        c.text(key);
        c.int(0)
    }
    c.text("maxCombo");
    c.int(0);
    c.text("hitWindow");
    let w = snap
        .regular
        .beatmap
        .parsed
        .as_ref()
        .map(|p| &p.engine.hit_windows);
    let fields = [
        ("miss", w.and_then(|w| w.miss)),
        ("meh", w.and_then(|w| w.meh)),
        ("ok", w.and_then(|w| w.ok)),
        ("good", w.and_then(|w| w.good)),
        ("great", w.and_then(|w| w.great)),
        ("perfect", w.and_then(|w| w.perfect)),
    ];
    c.map(fields.iter().filter(|(_, n)| n.is_some()).count());
    for (key, n) in fields {
        if let Some(n) = n {
            c.text(key);
            c.float(n)
        }
    }
}
