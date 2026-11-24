//! Results-screen fields retain the unrounded 1.x result pp values.
use super::play::{counts, field, map, number_name, text};
use super::{OsuPointer as P, ProjectionSink};
use vosu_osu_state::StateBundleSnapshot;

pub(super) fn project(s: &StateBundleSnapshot, sink: &mut dyn ProjectionSink) {
    sink.put_cbor(P::OsuResult, &mut |b| {
        let r = &s.regular.result_screen;
        let raw = &r.raw;
        map(b, 12);
        field(b, "scoreId", &raw.online_id);
        field(b, "playerName", raw.player_name.as_str());
        text(b, "mode");
        number_name(b, Some(raw.mode));
        field(b, "score", &raw.score);
        field(b, "accuracy", &raw.accuracy);
        field(b, "name", raw.player_name.as_str());
        text(b, "statistics");
        map(b, 10);
        counts(b, &raw.statistics);
        field(b, "sliderEndHits", &raw.statistics.slider_tail_hit);
        field(b, "smallTickHits", &raw.statistics.small_tick_hit);
        field(b, "largeTickHits", &raw.statistics.large_tick_hit);
        // Match 1.x v1's results precedent (src/api/v1.rs:692): slider_breaks: 0.
        field(b, "sliderBreaks", &0_i64);
        field(b, "mods", &raw.mods);
        field(b, "maxCombo", &raw.max_combo);
        field(b, "rank", r.grade.as_str());
        text(b, "pp");
        map(b, 2);
        field(b, "current", &r.pp);
        field(b, "fc", &r.fc_pp);
        field(b, "createdAt", raw.date.as_str());
    });
}
