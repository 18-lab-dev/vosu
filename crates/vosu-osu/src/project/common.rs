//! Common fields and allocation-free CBOR primitives for this projection partition.
use super::{OsuPointer as P, ProjectionSink, SessionFacts, beatmap, session, status_map};
use vosu_osu_model::fix_decimals;
use vosu_osu_state::{RegularState, StateBundleSnapshot};
use vosu_plugin_api::PublishValueRef as V;

pub(super) fn project(
    s: &StateBundleSnapshot,
    facts: &SessionFacts,
    sink: &mut dyn ProjectionSink,
) {
    session::project(s, facts, sink);
    beatmap::project(s, facts, sink);
    let r = &s.regular;
    sink.put(
        P::StatusStatus,
        text(status_map::status(s.precise.status, r.global.paused)),
    );
    for (at, value) in [
        (P::ChartTitle, &r.menu.title),
        (P::ChartArtist, &r.menu.artist),
        (P::ChartCharter, &r.menu.creator),
        (P::ChartDifficultyName, &r.menu.difficulty),
    ] {
        sink.put(at, text(value.as_deref()));
    }
    let parsed = r.beatmap.parsed.as_deref();
    let length = parsed.map_or(0.0, |p| p.times.full);
    sink.put(P::ChartLength, V::F64(length));
    sink.put(
        P::ChartBpm,
        V::F64(
            vosu_osu_model::fix_decimals_to(parsed.map_or(0.0, |p| p.bpm_base.common), 4)
                .unwrap_or(0.0),
        ),
    );
    sink.put(
        P::ChartKeyCount,
        if mode(r, s.precise.status) == Some(3) {
            V::F64(
                fix_decimals(parsed.map_or(0.0, |p| p.engine.converted_difficulty.circle_size))
                    .round(),
            )
        } else {
            V::Null
        },
    );
    sink.put(
        P::SkinName,
        V::String(
            r.settings
                .values
                .get("skin")
                .and_then(|v| v.get("name"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or(""),
        ),
    );
    let in_play = s.precise.status == 2;
    let g = &r.gameplay;
    for (at, value) in [
        (P::PlayScore, V::F64(g.score)),
        (P::PlayCombo, V::I64(g.combo)),
        (P::PlayMaxCombo, V::I64(g.max_combo)),
        (P::PlayAccuracy, V::F64(g.accuracy / 100.0)),
        (
            P::PlayHealth,
            V::F64(((g.player_hp / 200.0) * 100.0) / 100.0),
        ),
        (
            P::PlayAccuracyKind,
            text(match g.mode {
                0 => Some("osu-standard"),
                1 => Some("osu-taiko"),
                2 => Some("osu-catch"),
                3 => Some("osu-mania"),
                _ => None,
            }),
        ),
        (
            P::PlayProgress,
            if length > 0.0 {
                V::F64((s.precise.play_time / length).clamp(0.0, 1.0))
            } else {
                V::Null
            },
        ),
        (P::PlayRate, number(beatmap::rate(s, facts))),
    ] {
        sink.put(at, if in_play { value } else { V::Null });
    }
    if in_play {
        let st = &g.statistics;
        sink.put_cbor(P::PlayJudgements, &mut |out| {
            head(out, 4, 6);
            for (ordinal, (id, name, count)) in [
                ("perfect", "Perfect", st.perfect),
                ("great", "Great", st.great),
                ("good", "Good", st.good),
                ("ok", "Ok", st.ok),
                ("meh", "Meh", st.meh),
                ("miss", "Miss", st.miss),
            ]
            .into_iter()
            .enumerate()
            {
                map(
                    out,
                    &[
                        ("id", V::String(id)),
                        ("displayName", V::String(name)),
                        ("ordinal", V::U64(ordinal as u64)),
                        ("count", V::I64(count)),
                    ],
                );
            }
        });
    } else {
        sink.put(P::PlayJudgements, V::Null);
    }
}

pub(super) fn mode(r: &RegularState, status: i64) -> Option<i64> {
    match status {
        2 => Some(r.gameplay.mode),
        7 => Some(r.result_screen.raw.mode),
        _ => r.menu.gamemode,
    }
}
pub(super) fn text(v: Option<&str>) -> V<'_> {
    v.map_or(V::Null, V::String)
}
pub(super) fn number(v: Option<f64>) -> V<'static> {
    v.map_or(V::Null, V::F64)
}
pub(super) fn number_name(sink: &mut dyn ProjectionSink, at: P, number: V<'_>, name: &str) {
    sink.put_cbor(at, &mut |out| {
        map(out, &[("number", number), ("name", V::String(name))])
    });
}
// Only definite containers and scalar values are needed here. Writing directly into
// the diff sink's retained buffer avoids constructing an owned CBOR value tree.
pub(super) fn head(out: &mut Vec<u8>, major: u8, value: u64) {
    let prefix = major << 5;
    match value {
        0..=23 => out.push(prefix | value as u8),
        24..=255 => out.extend_from_slice(&[prefix | 24, value as u8]),
        256..=65535 => {
            out.push(prefix | 25);
            out.extend_from_slice(&(value as u16).to_be_bytes());
        }
        65536..=4294967295 => {
            out.push(prefix | 26);
            out.extend_from_slice(&(value as u32).to_be_bytes());
        }
        _ => {
            out.push(prefix | 27);
            out.extend_from_slice(&value.to_be_bytes());
        }
    }
}
pub(super) fn scalar(out: &mut Vec<u8>, value: V<'_>) {
    match value {
        V::Null => out.push(0xf6),
        V::Bool(v) => out.push(if v { 0xf5 } else { 0xf4 }),
        V::U64(v) => head(out, 0, v),
        V::I64(v) if v >= 0 => head(out, 0, v as u64),
        V::I64(v) => head(out, 1, (-1 - v) as u64),
        V::F64(v) => {
            out.push(0xfb);
            out.extend_from_slice(&v.to_be_bytes());
        }
        V::String(v) => {
            head(out, 3, v.len() as u64);
            out.extend_from_slice(v.as_bytes());
        }
        _ => unreachable!("O3 encodes scalar fields only"),
    }
}
pub(super) fn map(out: &mut Vec<u8>, fields: &[(&str, V<'_>)]) {
    head(out, 5, fields.len() as u64);
    for (key, value) in fields {
        scalar(out, V::String(key));
        scalar(out, *value);
    }
}

#[cfg(test)]
#[path = "tests/common.rs"]
mod tests;
