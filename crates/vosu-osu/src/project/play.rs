//! Allocation-free osu!-specific projection of the two immutable capture lanes.
use super::{OsuPointer as P, ProjectionSink, SessionFacts};
use vosu_osu_model::{
    enums::ClientType,
    fix_decimals,
    names::ruleset_name,
    states::{CalculateMods, Statistics},
};
use vosu_osu_state::StateBundleSnapshot;
use vosu_plugin_api::PublishValueRef as V;

pub(super) fn project(
    s: &StateBundleSnapshot,
    facts: &SessionFacts,
    sink: &mut dyn ProjectionSink,
) {
    super::keys::project(s, sink);
    super::leaderboard::project(s, sink);
    super::performance::project(s, sink);
    super::result::project(s, sink);
    let g = &s.regular.gameplay;
    sink.put(P::OsuPlayScore, V::F64(g.score));
    sink.put(P::OsuPlayAccuracyPercent, V::F64(g.accuracy));
    sink.put(P::OsuPlayRetries, V::I64(g.retries));
    sink.put_cbor(P::OsuPlayCombo, &mut |b| {
        map(b, 2);
        field(b, "current", &g.combo);
        field(b, "max", &g.max_combo);
    });
    sink.put_cbor(P::OsuPlayHealthRaw, &mut |b| {
        map(b, 2);
        field(b, "normal", &g.player_hp);
        field(b, "smooth", &g.player_hp_smooth);
    });
    sink.put_cbor(P::OsuPlayJudgements, &mut |b| {
        map(b, 6);
        counts(b, &g.statistics);
    });
    if ![P::OsuPlaySource, P::OsuPlayPp]
        .into_iter()
        .any(|at| sink.wants(at))
    {
        return;
    }
    let r = &s.regular;
    let g = &r.gameplay;
    let status = s.precise.status;
    let mods = match status {
        2 => &g.mods,
        7 => &r.result_screen.raw.mods,
        _ => &r.global.menu_mods,
    };
    // Unlike beatmap.mode, 1.x play.mode always comes from gameplay.
    let mode = Some(g.mode);
    let source = if status != 2 {
        V::Null
    } else {
        V::String(
            if r.global.render.is_some_and(|v| v.spectating) || r.global.is_multi_spectating {
                "spectator"
            } else if r.global.is_watching_replay {
                if facts.client == ClientType::Lazer {
                    "replay"
                } else {
                    "watching"
                }
            } else if mods
                .array
                .iter()
                .any(|m| matches!(m.acronym.as_str(), "AT" | "CN"))
            {
                "autoplay"
            } else {
                "player"
            },
        )
    };
    sink.put(P::OsuPlaySource, source);
    sink.put(P::OsuPlayFailed, V::Bool(g.failed));
    sink.put(P::OsuPlayPlayerName, V::String(&g.player_name));
    sink.put_cbor(P::OsuPlayMode, &mut |b| number_name(b, mode));
    sink.put_cbor(P::OsuPlayHealthBar, &mut |b| {
        map(b, 2);
        field(b, "normal", &((g.player_hp / 200.0) * 100.0));
        field(b, "smooth", &((g.player_hp_smooth / 200.0) * 100.0));
    });
    sink.put_cbor(P::OsuPlayStatistics, &mut |b| {
        statistics(b, &g.statistics, g.slider_breaks)
    });
    sink.put_cbor(P::OsuPlayMods, &mut |b| mods.write(b));
    sink.put_cbor(P::OsuPlayRank, &mut |b| {
        map(b, 2);
        field(b, "current", g.grade_current.as_str());
        field(b, "maxThisPlay", g.grade_expected.as_str());
    });
    sink.put_cbor(P::OsuPlayPp, &mut |b| {
        let a = r.beatmap.attributes.unwrap_or_default();
        map(b, 5);
        field(b, "current", &fix_decimals(a.pp));
        field(b, "fc", &fix_decimals(a.fc_pp));
        field(b, "maxAchieved", &fix_decimals(a.max_achieved));
        field(b, "maxAchievable", &fix_decimals(a.max_achievable));
        text(b, "detailed");
        map(b, 2);
        for (name, attrs, total) in [("current", a.current, a.pp), ("fc", a.fc, a.fc_pp)] {
            text(b, name);
            map(b, 6);
            for (name, value) in [
                ("aim", attrs.aim),
                ("speed", attrs.speed),
                ("accuracy", attrs.accuracy),
                ("difficulty", attrs.difficulty),
                ("flashlight", attrs.flashlight),
                ("total", total),
            ] {
                field(b, name, &fix_decimals(value));
            }
        }
    });
    // 1.x's regular play field is regular gameplay UR, not the precise estimator.
    sink.put(P::OsuPlayUnstableRate, V::F64(g.unstable_rate));
}

pub(super) fn number_name(b: &mut Vec<u8>, mode: Option<i64>) {
    map(b, 2);
    field(b, "number", &mode);
    field(b, "name", ruleset_name(mode.unwrap_or(-1)));
}

pub(super) fn counts(b: &mut Vec<u8>, s: &Statistics) {
    for (name, value) in [
        ("miss", s.miss),
        ("meh", s.meh),
        ("ok", s.ok),
        ("great", s.great),
        ("perfect", s.perfect),
        ("good", s.good),
    ] {
        field(b, name, &value);
    }
}
fn statistics(b: &mut Vec<u8>, s: &Statistics, breaks: i64) {
    map(b, 14);
    for (name, value) in [
        ("smallTickMiss", s.small_tick_miss),
        ("smallTickHit", s.small_tick_hit),
        ("largeTickMiss", s.large_tick_miss),
        ("largeTickHit", s.large_tick_hit),
        ("smallBonus", s.small_bonus),
        ("largeBonus", s.large_bonus),
        ("ignoreMiss", s.ignore_miss),
        ("ignoreHit", s.ignore_hit),
        ("comboBreak", s.combo_break),
        ("sliderTailHit", s.slider_tail_hit),
        ("sliderBreaks", breaks),
        ("sliderEndHits", s.slider_tail_hit),
        ("good", s.good),
    ] {
        field(b, name, &value);
    }
    field(b, "legacyComboIncrease", &s.legacy_combo_increase);
}

// Small direct CBOR writer: no intermediate Value tree, cloned histories or DTOs.
// Whole subtrees are encoded into DiffSink's retained scratch allocation.
pub(super) trait Encode {
    fn write(&self, b: &mut Vec<u8>);
}
pub(super) fn head(b: &mut Vec<u8>, major: u8, n: u64) {
    if n < 24 {
        b.push(major | n as u8);
    } else if n <= u8::MAX as u64 {
        b.extend_from_slice(&[major | 24, n as u8]);
    } else if n <= u16::MAX as u64 {
        b.push(major | 25);
        b.extend_from_slice(&(n as u16).to_be_bytes());
    } else if n <= u32::MAX as u64 {
        b.push(major | 26);
        b.extend_from_slice(&(n as u32).to_be_bytes());
    } else {
        b.push(major | 27);
        b.extend_from_slice(&n.to_be_bytes());
    }
}
pub(super) fn map(b: &mut Vec<u8>, n: u64) {
    head(b, 0xa0, n);
}
pub(super) fn array(b: &mut Vec<u8>, n: usize) {
    head(b, 0x80, n as u64);
}
pub(super) fn text(b: &mut Vec<u8>, s: &str) {
    head(b, 0x60, s.len() as u64);
    b.extend_from_slice(s.as_bytes());
}
pub(super) fn field<T: Encode + ?Sized>(b: &mut Vec<u8>, name: &str, v: &T) {
    text(b, name);
    v.write(b);
}
impl Encode for str {
    fn write(&self, b: &mut Vec<u8>) {
        text(b, self);
    }
}
impl Encode for bool {
    fn write(&self, b: &mut Vec<u8>) {
        b.push(if *self { 0xf5 } else { 0xf4 });
    }
}
impl Encode for i64 {
    fn write(&self, b: &mut Vec<u8>) {
        if *self >= 0 {
            head(b, 0, *self as u64);
        } else {
            head(b, 0x20, !*self as u64);
        }
    }
}
impl Encode for f64 {
    fn write(&self, b: &mut Vec<u8>) {
        b.push(0xfb);
        b.extend_from_slice(&self.to_bits().to_be_bytes());
    }
}
impl Encode for f32 {
    fn write(&self, b: &mut Vec<u8>) {
        // 1.x's JsFormatter and Catcher writer widen before formatting.
        f64::from(*self).write(b);
    }
}
impl<T: Encode> Encode for Option<T> {
    fn write(&self, b: &mut Vec<u8>) {
        if let Some(v) = self {
            v.write(b);
        } else {
            b.push(0xf6);
        }
    }
}
impl<T: Encode> Encode for [T] {
    fn write(&self, b: &mut Vec<u8>) {
        array(b, self.len());
        for v in self {
            v.write(b);
        }
    }
}
impl<T: Encode> Encode for Vec<T> {
    fn write(&self, b: &mut Vec<u8>) {
        self.as_slice().write(b);
    }
}
impl Encode for CalculateMods {
    fn write(&self, b: &mut Vec<u8>) {
        map(b, 5);
        field(b, "checksum", self.checksum.as_str());
        field(b, "number", &self.number);
        field(b, "name", self.name.as_str());
        field(b, "rate", &self.rate);
        text(b, "array");
        array(b, self.array.len());
        for m in &self.array {
            map(b, if m.settings.is_some() { 2 } else { 1 });
            field(b, "acronym", m.acronym.as_str());
            if let Some(settings) = &m.settings {
                text(b, "settings");
                map(b, settings.len() as u64);
                for (k, v) in settings {
                    field(b, k, v);
                }
            }
        }
    }
}
impl Encode for serde_json::Value {
    fn write(&self, b: &mut Vec<u8>) {
        use serde_json::Value;
        match self {
            Value::Null => b.push(0xf6),
            Value::Bool(v) => v.write(b),
            Value::Number(v) => {
                if let Some(n) = v.as_i64() {
                    n.write(b);
                } else if let Some(n) = v.as_u64() {
                    head(b, 0, n);
                } else {
                    v.as_f64().unwrap().write(b);
                }
            }
            Value::String(v) => text(b, v),
            Value::Array(v) => v.write(b),
            Value::Object(v) => {
                map(b, v.len() as u64);
                for (k, v) in v {
                    field(b, k, v);
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "tests/play.rs"]
mod tests;
