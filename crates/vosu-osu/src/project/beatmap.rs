//! Exact 1.x beatmap values, retaining the original/converted distinction.
use super::{
    OsuPointer as P, ProjectionSink, SessionFacts,
    common::{head, map, mode, number, number_name, scalar, text},
};
use vosu_osu_model::{
    enums::ClientType,
    fix_decimals, fix_decimals_to,
    names::{beatmap_status_name, ruleset_name},
};
use vosu_osu_state::StateBundleSnapshot;
use vosu_plugin_api::PublishValueRef as V;

pub(super) fn rate(s: &StateBundleSnapshot, facts: &SessionFacts) -> Option<f64> {
    s.regular
        .global
        .render
        .and_then(|r| r.clock)
        .map(|c| c.rate)
        .or_else(|| {
            (facts.client != ClientType::Lazer).then(|| match s.precise.status {
                2 => s.regular.gameplay.mods.rate,
                7 => s.regular.result_screen.raw.mods.rate,
                _ => s.regular.global.menu_mods.rate,
            })
        })
}
pub(super) fn project(
    s: &StateBundleSnapshot,
    facts: &SessionFacts,
    sink: &mut dyn ProjectionSink,
) {
    let r = &s.regular;
    let p = r.beatmap.parsed.as_deref();
    let original_mode = r.beatmap.mode.unwrap_or(0);
    for (at, value) in [
        (P::OsuBeatmapIsKiai, r.beatmap.event_status.kiai),
        (P::OsuBeatmapIsBreak, r.beatmap.event_status.break_time),
        (
            P::OsuBeatmapIsConvert,
            original_mode == 0 && Some(original_mode) != mode(r, s.precise.status),
        ),
    ] {
        sink.put(at, V::Bool(value));
    }
    sink.put(P::OsuBeatmapTimeLive, V::F64(s.precise.play_time));
    sink.put(P::OsuBeatmapTimeRate, number(rate(s, facts)));
    sink.put(
        P::OsuBeatmapTimeRunning,
        r.global
            .render
            .and_then(|r| r.clock)
            .map_or(V::Null, |c| V::Bool(c.running)),
    );
    sink.put(
        P::OsuBeatmapTimeFirstObject,
        V::I64(r.beatmap.first_object_time.unwrap_or(0)),
    );
    sink.put(
        P::OsuBeatmapTimeLastObject,
        V::F64(p.map_or(0.0, |p| p.times.full)),
    );
    sink.put(P::OsuBeatmapTimeMp3Length, number(r.menu.mp3_length));
    sink.put(
        P::OsuBeatmapSelectionMode,
        r.menu.gamemode.map_or(V::Null, V::I64),
    );
    sink.put(
        P::OsuBeatmapTimeFirstHitObject,
        p.map_or(V::Null, |p| V::I64(i64::from(p.times.first_object))),
    );
    sink.put(
        P::OsuBeatmapTimePreview,
        p.map_or(V::Null, |p| V::I64(i64::from(p.times.preview))),
    );
    number_name(
        sink,
        P::OsuBeatmapStatus,
        number(r.menu.ranked_status),
        r.menu
            .ranked_status
            .filter(|v| v.is_finite() && *v != 0.0)
            .map_or("", |v| beatmap_status_name(v as i64)),
    );
    for (at, value) in [
        (P::OsuBeatmapChecksum, &r.menu.checksum),
        (P::OsuBeatmapArtist, &r.menu.artist),
        (P::OsuBeatmapArtistUnicode, &r.menu.artist_original),
        (P::OsuBeatmapTitle, &r.menu.title),
        (P::OsuBeatmapTitleUnicode, &r.menu.title_original),
        (P::OsuBeatmapMapper, &r.menu.creator),
        (P::OsuBeatmapVersion, &r.menu.difficulty),
    ] {
        sink.put(at, text(value.as_deref()));
    }
    sink.put(P::OsuBeatmapId, r.menu.map_id.map_or(V::Null, V::I64));
    sink.put(P::OsuBeatmapSet, r.menu.set_id.map_or(V::Null, V::I64));
    number_name(
        sink,
        P::OsuBeatmapMode,
        V::I64(original_mode),
        ruleset_name(original_mode),
    );
    sink.put(
        P::OsuBeatmapSource,
        V::String(p.map_or("", |p| &p.metadata.source)),
    );
    sink.put(
        P::OsuBeatmapTags,
        V::String(p.map_or("", |p| &p.metadata.tags)),
    );
    sink.put_cbor(P::OsuBeatmapStats, &mut |out| {
        let a = p.map(|p| &p.engine.difficulty);
        let orig = p.map(|p| &p.engine.original_difficulty);
        let conv = p.map(|p| &p.engine.converted_difficulty);
        head(out, 5, 9);
        scalar(out, V::String("stars"));
        let truthy =
            |v: Option<f64>| number(v.filter(|v| *v != 0.0 && !v.is_nan()).map(fix_decimals));
        map_present(
            out,
            &[
                (
                    "live",
                    V::F64(fix_decimals(r.beatmap.attributes.map_or(0.0, |v| v.stars))),
                ),
                ("aim", truthy(a.and_then(|v| v.aim))),
                ("speed", truthy(a.and_then(|v| v.speed))),
                ("flashlight", truthy(a.and_then(|v| v.flashlight))),
                ("sliderFactor", truthy(a.and_then(|v| v.slider_factor))),
                ("stamina", truthy(a.and_then(|v| v.stamina))),
                ("rhythm", truthy(a.and_then(|v| v.rhythm))),
                ("color", truthy(a.and_then(|v| v.color))),
                ("reading", truthy(a.and_then(|v| v.reading))),
                (
                    "hitWindow",
                    number(
                        p.and_then(|p| p.engine.hit_windows.great)
                            .filter(|v| *v != 0.0 && !v.is_nan())
                            .map(fix_decimals)
                            .map(fix_decimals),
                    ),
                ),
                ("total", V::F64(fix_decimals(a.map_or(0.0, |v| v.stars)))),
            ],
        );
        for (key, original, converted) in [
            (
                "ar",
                orig.map_or(0.0, |v| v.approach_rate),
                conv.map_or(0.0, |v| v.approach_rate),
            ),
            (
                "cs",
                orig.map_or(0.0, |v| v.circle_size),
                conv.map_or(0.0, |v| v.circle_size),
            ),
            (
                "od",
                orig.map_or(0.0, |v| v.overall_difficulty),
                conv.map_or(0.0, |v| v.overall_difficulty),
            ),
            (
                "hp",
                orig.map_or(0.0, |v| v.drain_rate),
                conv.map_or(0.0, |v| v.drain_rate),
            ),
        ] {
            scalar(out, V::String(key));
            map(
                out,
                &[
                    ("original", V::F64(fix_decimals(original))),
                    ("converted", V::F64(fix_decimals(converted))),
                ],
            );
        }
        scalar(out, V::String("hitWindow"));
        let w = p.map(|p| &p.engine.hit_windows);
        map_present(
            out,
            &[
                ("miss", number(w.and_then(|v| v.miss))),
                ("meh", number(w.and_then(|v| v.meh))),
                ("ok", number(w.and_then(|v| v.ok))),
                ("good", number(w.and_then(|v| v.good))),
                ("great", number(w.and_then(|v| v.great))),
                ("perfect", number(w.and_then(|v| v.perfect))),
            ],
        );
        scalar(out, V::String("bpm"));
        map(
            out,
            &[
                (
                    "realtime",
                    V::F64(
                        fix_decimals_to(r.beatmap.event_status.bpm, 4)
                            .unwrap_or(r.beatmap.event_status.bpm),
                    ),
                ),
                (
                    "common",
                    V::F64(fix_decimals_to(p.map_or(0.0, |p| p.bpm.common), 4).unwrap_or(0.0)),
                ),
                (
                    "min",
                    V::F64(fix_decimals_to(p.map_or(0.0, |p| p.bpm.minimum), 4).unwrap_or(0.0)),
                ),
                (
                    "max",
                    V::F64(fix_decimals_to(p.map_or(0.0, |p| p.bpm.maximum), 4).unwrap_or(0.0)),
                ),
            ],
        );
        scalar(out, V::String("objects"));
        let counts = p.map(|p| p.object_counts);
        let circles = counts.map_or(0, |v| i64::from(v.circles));
        let sliders = counts.map_or(0, |v| i64::from(v.sliders));
        let spinners = counts.map_or(0, |v| i64::from(v.spinners));
        let holds = counts.map_or(0, |v| i64::from(v.holds));
        map(
            out,
            &[
                ("circles", V::I64(circles)),
                ("sliders", V::I64(sliders)),
                ("spinners", V::I64(spinners)),
                ("holds", V::I64(holds)),
                ("total", V::I64(circles + sliders + spinners + holds)),
            ],
        );
        scalar(out, V::String("maxCombo"));
        scalar(out, V::U64(a.map_or(0, |v| u64::from(v.max_combo))));
    });
}

// 1.x omits unsupported components rather than encoding null. Count the
// populated stack-resident entries before writing a definite CBOR map.
fn map_present(out: &mut Vec<u8>, fields: &[(&str, V<'_>)]) {
    let present = fields.iter().filter(|(_, value)| !matches!(value, V::Null));
    head(out, 5, present.clone().count() as u64);
    for (key, value) in present {
        scalar(out, V::String(key));
        scalar(out, *value);
    }
}
