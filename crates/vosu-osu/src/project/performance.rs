//! Performance table and graph; DiffSink retains whole-value encodings.
use super::play::{array, field, map, text};
use super::{OsuPointer as P, ProjectionSink};
use vosu_osu_model::fix_decimals;
use vosu_osu_state::StateBundleSnapshot;

pub(super) fn project(s: &StateBundleSnapshot, sink: &mut dyn ProjectionSink) {
    sink.put_cbor(P::OsuPerformanceAccuracy, &mut |b| {
        map(b, 11);
        for (key, n) in [
            ("90", 90),
            ("91", 91),
            ("92", 92),
            ("93", 93),
            ("94", 94),
            ("95", 95),
            ("96", 96),
            ("97", 97),
            ("98", 98),
            ("99", 99),
            ("100", 100),
        ] {
            field(
                b,
                key,
                &fix_decimals(s.regular.beatmap.pp_acc.get(&n).copied().unwrap_or(0.0)),
            );
        }
    });
    sink.put_cbor(P::OsuPerformanceGraph, &mut |b| {
        let graph = &s.regular.beatmap.graph;
        map(b, 2);
        text(b, "series");
        array(b, graph.series.len());
        for series in &graph.series {
            map(b, 2);
            field(b, "name", series.name.as_str());
            field(b, "data", &series.data);
        }
        field(b, "xaxis", &graph.x_axis);
    });
}
