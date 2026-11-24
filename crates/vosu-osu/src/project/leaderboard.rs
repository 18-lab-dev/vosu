//! Whole-array leaderboard projection, preserving the 1.x order.
use super::play::{array, counts, field, map};
use super::{OsuPointer as P, ProjectionSink};
use vosu_osu_state::StateBundleSnapshot;

pub(super) fn project(s: &StateBundleSnapshot, sink: &mut dyn ProjectionSink) {
    sink.put_cbor(P::OsuLeaderboard, &mut |b| {
        array(b, s.regular.gameplay.leaderboard_scores.len());
        for p in &s.regular.gameplay.leaderboard_scores {
            map(b, 11);
            field(b, "isFailed", &!p.is_passing);
            field(b, "position", &p.position);
            field(b, "team", &p.team);
            field(b, "id", &p.user_id);
            field(b, "name", p.name.as_str());
            field(b, "score", &p.score);
            field(b, "accuracy", &p.accuracy);
            super::play::text(b, "statistics");
            map(b, 6);
            counts(b, &p.statistics);
            super::play::text(b, "combo");
            map(b, 2);
            field(b, "current", &p.combo);
            field(b, "max", &p.max_combo);
            field(b, "mods", &p.mods);
            field(b, "rank", "");
        }
    });
}
