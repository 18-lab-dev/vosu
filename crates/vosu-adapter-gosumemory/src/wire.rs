//! Direct writer: histories and strings stay borrowed from the immutable view.
use std::io;
use vosu_adapter_support::json::{Wire, With, wire_object};
use crate::{Error, View, game_folder, songs_folder, legacy_hits, helpers::joined_path};
use crate::view;

pub(super) fn encode(v: &View) -> Result<Vec<u8>, Error> {
    // Tournament failure is evaluated before emitting any response bytes.
    crate::tourney::check(v)?;
    let mut out = Vec::with_capacity(4096 + 24 * (v.play.hit_errors.len() + v.performance.graph.primary.len() + v.performance.graph.xaxis.len()));
    document(&mut out, v).map_err(|e| Error::Serialization(e.to_string()))?;
    Ok(out)
}
struct Mods<'a>(&'a view::Mods);
impl Wire for Mods<'_> {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(out { "num": self.0.number, "str": self.0.name }); Ok(())
    }
}
fn document(out: &mut Vec<u8>, v: &View) -> io::Result<()> {
    let b = &v.beatmap; let s = &b.stats; let p = &v.play; let h = legacy_hits(p);
    let r = &v.result; let g = &v.performance.graph; let paths = v.paths.current.as_ref();
    let pp = |key: &str| v.performance.accuracy.get(key).copied().unwrap_or(0.0);
    let key = |button: Option<vosu_osu_model::source::OverlayButton>| With(move |out: &mut Vec<u8>| {
        wire_object!(out { "isPressed": button.is_some_and(|b| b.is_pressed), "count": button.map_or(0, |b| b.count) }); Ok(())
    });
    wire_object!(@open out first);
    wire_object!(@members out first {
        "client": v.client,
        "settings": { "showInterface": v.interface_visible, "folders": {
            "game": game_folder(v), "skin": v.paths.skin_folder.as_deref().unwrap_or(""), "songs": songs_folder(v),
        } },
        "menu": With(|out: &mut Vec<u8>| {
            wire_object!(@open out first);
            wire_object!(@members out first {
                "mainMenu": { "bassDensity": v.bass_density }, "state": v.state,
                "gameMode"?: b.selection_mode, "isChatEnabled": i64::from(v.chat_status != 0),
                "bm": {
                    "time": { "firstObj": b.time.first_hit_object.unwrap_or(0), "current": b.time.live, "full": b.time.last_object, "mp3"?: b.time.mp3_length },
                    "id"?: b.id, "set"?: b.set, "md5"?: b.checksum.as_ref(), "rankedStatus"?: b.status.number,
                    "metadata": { "artist"?: b.artist.as_ref(), "artistOriginal"?: b.artist_unicode.as_ref(), "title"?: b.title.as_ref(), "titleOriginal"?: b.title_unicode.as_ref(), "mapper"?: b.mapper.as_ref(), "difficulty"?: b.version.as_ref() },
                    "stats": With(|out: &mut Vec<u8>| {
                        wire_object!(out {
                            "AR": s.ar.converted, "CS": s.cs.converted, "OD": s.od.converted, "HP": s.hp.converted, "SR": s.stars.live,
                            "BPM": { "realtime": s.bpm.realtime, "common": s.bpm.common, "min": s.bpm.min, "max": s.bpm.max },
                            "circles": s.objects.circles, "sliders": s.objects.sliders, "spinners": s.objects.spinners, "holds": s.objects.holds,
                            "maxCombo": s.max_combo, "fullSR": s.stars.total, "memoryAR": s.ar.original, "memoryCS": s.cs.original, "memoryOD": s.od.original, "memoryHP": s.hp.original,
                        }); Ok(())
                    }),
                    "path": {
                        "full": joined_path(paths.and_then(|p| p.beatmap_folder.as_deref()).unwrap_or(""), paths.and_then(|p| p.background.as_deref()).unwrap_or("")),
                        "folder"?: paths.and_then(|p| p.beatmap_folder.as_ref()), "file"?: paths.and_then(|p| p.beatmap_file.as_ref()),
                        "bg"?: paths.and_then(|p| p.background.as_ref()), "audio"?: paths.and_then(|p| p.audio.as_ref()),
                    },
                }, "mods": Mods(&p.mods),
            });
            wire_object!(@members out first {
                "pp": {
                    "90": pp("90"), "91": pp("91"), "92": pp("92"), "93": pp("93"), "94": pp("94"), "95": pp("95"),
                    "96": pp("96"), "97": pp("97"), "98": pp("98"), "99": pp("99"), "100": pp("100"),
                    "strains": g.primary.as_ref(), "strainsAll": {
                        "series": With(|out: &mut Vec<u8>| {
                            out.push(b'[');
                            for (i, series) in g.series.iter().enumerate() {
                                if i != 0 { out.push(b','); }
                                wire_object!(out { "name": series.name, "data": series.data.as_ref() });
                            }
                            out.push(b']'); Ok(())
                        }), "xaxis": g.xaxis.as_ref(),
                    },
                },
            });
            wire_object!(@close out first); Ok(())
        }),
    });
    wire_object!(@members out first {
        "gameplay": With(|out: &mut Vec<u8>| {
            wire_object!(out {
                "gameMode": p.mode.number, "name": p.player_name, "score": p.score, "accuracy": p.accuracy,
                "combo": { "current": p.combo.current, "max": p.combo.max }, "hp": { "normal": p.health.normal, "smooth": p.health.smooth },
                "hits": {
                    "0": h.n0, "50": h.n50, "100": h.n100, "300": h.n300, "geki": h.geki, "katu": h.katu,
                    "sliderEndHits": h.slider_end_hits, "smallTickHits": h.small_tick_hits, "largeTickHits": h.large_tick_hits,
                    "sliderBreaks": p.statistics.slider_breaks, "grade": { "current": p.rank.current, "maxThisPlay": p.rank.max_this_play },
                    "unstableRate": p.unstable_rate, "hitErrorArray": p.hit_errors.as_ref(),
                },
                "pp": { "current": p.pp.current, "fc": p.pp.fc, "maxThisPlay": p.pp.max_achievable },
                "keyOverlay": { "k1": key(v.keys.as_ref().and_then(|k| k.k1)), "k2": key(v.keys.as_ref().and_then(|k| k.k2)), "m1": key(v.keys.as_ref().and_then(|k| k.m1)), "m2": key(v.keys.as_ref().and_then(|k| k.m2)) },
                "leaderboard": {
                    "hasLeaderboard": !v.leaderboard.is_empty(), "isVisible": v.leaderboard_state.visible,
                    "ourplayer": With(|out: &mut Vec<u8>| {
                        let p = &v.leaderboard_state.player;
                        player(out, &p.name, p.score, p.combo, p.max_combo, &p.mods.name, &p.judgements, p.team, p.position, p.is_passing)
                    }),
                    "slots": With(|out: &mut Vec<u8>| {
                        out.push(b'[');
                        for (i, p) in v.leaderboard.iter().enumerate() {
                            if i != 0 { out.push(b','); }
                            player(out, p.name.as_deref().unwrap_or(""), p.score, p.combo.current, p.combo.max, &p.mods.name, &p.statistics, p.team, p.position, !p.is_failed)?;
                        }
                        out.push(b']'); Ok(())
                    }),
                }, "_isReplayUiHidden": !v.replay_ui_visible,
            }); Ok(())
        }),
        "resultsScreen": {
            "0": r.statistics.miss, "50": r.statistics.meh, "100": r.statistics.ok, "300": r.statistics.great,
            "mode": p.mode.number, "name": r.name, "score": r.score, "accuracy": r.accuracy, "maxCombo": r.max_combo,
            "mods": Mods(&r.mods), "geki": r.statistics.perfect, "katu": r.statistics.good, "grade": r.rank, "createdAt": r.created_at,
        },
        "userProfile": With(|out: &mut Vec<u8>| {
            let u = &v.profile;
            wire_object!(out {
                "rawLoginStatus"?: u.user_status.number, "name"?: u.name.as_ref(), "accuracy"?: u.accuracy,
                "rankedScore"?: u.ranked_score, "id"?: u.id, "level"?: u.level, "playCount"?: u.play_count,
                "playMode"?: u.mode.number, "rank"?: u.global_rank, "countryCode"?: u.country_code.number,
                "performancePoints"?: u.pp, "rawBanchoStatus"?: u.bancho_status.number, "backgroundColour"?: u.background_colour.as_ref(),
            }); Ok(())
        }),
        "tourney"?: crate::tourney::source(v).map(|source| With(move |out: &mut Vec<u8>| crate::tourney::write(out, v, source))),
    });
    wire_object!(@close out first); Ok(())
}
#[expect(clippy::too_many_arguments, reason = "writes two differently shaped upstream leaderboard source records without allocating")]
fn player(out: &mut Vec<u8>, name: &str, score: f64, combo: i64, max_combo: i64, mods: &str, h: &vosu_osu_model::source::Judgements, team: i64, position: i64, passing: bool) -> io::Result<()> {
    wire_object!(out { "name": name, "score": score, "combo": combo, "maxCombo": max_combo, "mods": mods,
        "h300": h.great, "geki": h.perfect, "h100": h.ok, "katu": h.good, "h50": h.meh, "h0": h.miss,
        "team": team, "position": position, "isPassing": i64::from(passing),
    }); Ok(())
}
