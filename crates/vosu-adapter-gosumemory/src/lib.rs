//! gosuMemory projection over the permanent authorized surface (no game-state bridge).
mod helpers;
pub mod model;
mod number;
mod plugin;
pub use plugin::vosu_plugin_vtable;
pub mod provider;
mod tourney;
pub mod view;
mod wire;
use bytes::Bytes;
use model::*;
use view::View;
use vosu_osu_model::{source::OverlayButton, states::Statistics, to_legacy_hits};

pub const MANIFEST: &str = include_str!("../manifest.toml");
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    NotReady,
    Decode(String),
    Serialization(String),
    Surface(vosu_surface::state::SurfaceError),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotReady => f.write_str("state is not ready"),
            Self::Decode(e) | Self::Serialization(e) => f.write_str(e),
            Self::Surface(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for Error {}

/// Owned DTO for cold consumers/legacy filter projection. Encoding borrows the view instead.
pub fn build(v: &View) -> Result<Response, Error> {
    build_with(v)
}
fn build_with<'a, V: From<&'a [f64]>>(v: &'a View) -> Result<Response<V>, Error> {
    let b = &v.beatmap;
    let s = &b.stats;
    let p = &v.play;
    let h = legacy_hits(p);
    let paths = v.paths.current.as_ref();
    let pp = |key: &str| v.performance.accuracy.get(key).copied().unwrap_or(0.0);
    let graph = &v.performance.graph;
    let r = &v.result;
    Ok(Response {
        client: v.client.clone(),
        settings: Settings {
            show_interface: v.interface_visible,
            folders: Folders {
                game: game_folder(v).into(),
                skin: v.paths.skin_folder.clone().unwrap_or_default(),
                songs: songs_folder(v).into(),
            },
        },
        menu: Menu {
            main_menu: MainMenu {
                bass_density: v.bass_density,
            },
            state: v.state,
            game_mode: b.selection_mode,
            is_chat_enabled: i64::from(v.chat_status != 0),
            bm: Beatmap {
                time: BeatmapTimings {
                    first_obj: b.time.first_hit_object.unwrap_or(0),
                    current: b.time.live,
                    full: b.time.last_object,
                    mp3: b.time.mp3_length,
                },
                id: b.id,
                set: b.set,
                md5: b.checksum.clone(),
                ranked_status: b.status.number,
                metadata: BeatmapMetadata {
                    artist: b.artist.clone(),
                    artist_original: b.artist_unicode.clone(),
                    title: b.title.clone(),
                    title_original: b.title_unicode.clone(),
                    mapper: b.mapper.clone(),
                    difficulty: b.version.clone(),
                },
                stats: BeatmapStats {
                    ar: s.ar.converted,
                    cs: s.cs.converted,
                    od: s.od.converted,
                    hp: s.hp.converted,
                    sr: s.stars.live,
                    bpm: Bpm {
                        realtime: s.bpm.realtime,
                        common: s.bpm.common,
                        min: s.bpm.min,
                        max: s.bpm.max,
                    },
                    circles: s.objects.circles,
                    sliders: s.objects.sliders,
                    spinners: s.objects.spinners,
                    holds: s.objects.holds,
                    max_combo: s.max_combo,
                    full_sr: s.stars.total,
                    memory_ar: s.ar.original,
                    memory_cs: s.cs.original,
                    memory_od: s.od.original,
                    memory_hp: s.hp.original,
                },
                path: BeatmapPaths {
                    full: helpers::joined_path(
                        paths
                            .and_then(|p| p.beatmap_folder.as_deref())
                            .unwrap_or(""),
                        paths.and_then(|p| p.background.as_deref()).unwrap_or(""),
                    ),
                    folder: paths.and_then(|p| p.beatmap_folder.clone()),
                    file: paths.and_then(|p| p.beatmap_file.clone()),
                    bg: paths.and_then(|p| p.background.clone()),
                    audio: paths.and_then(|p| p.audio.clone()),
                },
            },
            mods: mods(&p.mods),
            pp: PerformancePoints {
                acc_90: pp("90"),
                acc_91: pp("91"),
                acc_92: pp("92"),
                acc_93: pp("93"),
                acc_94: pp("94"),
                acc_95: pp("95"),
                acc_96: pp("96"),
                acc_97: pp("97"),
                acc_98: pp("98"),
                acc_99: pp("99"),
                acc_100: pp("100"),
                strains: V::from(graph.primary.as_ref()),
                strains_all: BeatmapStrains {
                    series: graph
                        .series
                        .iter()
                        .map(|s| BeatmapStrainSeries {
                            name: s.name.clone(),
                            data: V::from(s.data.as_ref()),
                        })
                        .collect(),
                    xaxis: V::from(graph.xaxis.as_ref()),
                },
            },
        },
        gameplay: Gameplay {
            game_mode: p.mode.number,
            name: p.player_name.clone(),
            score: p.score,
            accuracy: p.accuracy,
            combo: Combo {
                current: p.combo.current,
                max: p.combo.max,
            },
            hp: Health {
                normal: p.health.normal,
                smooth: p.health.smooth,
            },
            hits: GameplayHits {
                h300: h.n300,
                geki: h.geki,
                h100: h.n100,
                katu: h.katu,
                h50: h.n50,
                h0: h.n0,
                slider_end_hits: h.slider_end_hits,
                small_tick_hits: h.small_tick_hits,
                large_tick_hits: h.large_tick_hits,
                slider_breaks: p.statistics.slider_breaks,
                grade: GameplayGrade {
                    current: p.rank.current.clone(),
                    max_this_play: p.rank.max_this_play.clone(),
                },
                unstable_rate: p.unstable_rate,
                hit_error_array: V::from(p.hit_errors.as_ref()),
            },
            pp: GameplayPerformancePoints {
                current: p.pp.current,
                fc: p.pp.fc,
                max_this_play: p.pp.max_achievable,
            },
            key_overlay: KeyOverlay {
                k1: key(v.keys.as_ref().and_then(|k| k.k1)),
                k2: key(v.keys.as_ref().and_then(|k| k.k2)),
                m1: key(v.keys.as_ref().and_then(|k| k.m1)),
                m2: key(v.keys.as_ref().and_then(|k| k.m2)),
            },
            leaderboard: Leaderboard {
                has_leaderboard: !v.leaderboard.is_empty(),
                is_visible: v.leaderboard_state.visible,
                ourplayer: player(&v.leaderboard_state.player),
                slots: v.leaderboard.iter().map(slot).collect(),
            },
            is_replay_ui_hidden: !v.replay_ui_visible,
        },
        results_screen: ResultsScreen {
            mode: p.mode.number,
            name: r.name.clone(),
            score: r.score,
            accuracy: r.accuracy,
            max_combo: r.max_combo,
            mods: mods(&r.mods),
            h300: r.statistics.great,
            geki: r.statistics.perfect,
            h100: r.statistics.ok,
            katu: r.statistics.good,
            h50: r.statistics.meh,
            h0: r.statistics.miss,
            grade: r.rank.clone(),
            created_at: r.created_at.clone(),
        },
        user_profile: profile(&v.profile),
        tourney: tourney::build(v)?,
    })
}
pub fn encode(v: &View) -> Result<Bytes, Error> {
    wire::encode(v).map(Bytes::from)
}
fn mods(m: &view::Mods) -> SelectedMods {
    SelectedMods {
        num: m.number,
        text: m.name.clone(),
    }
}
fn key(k: Option<OverlayButton>) -> KeyOverlayButton {
    KeyOverlayButton {
        is_pressed: k.is_some_and(|k| k.is_pressed),
        count: k.map_or(0, |k| k.count),
    }
}
fn game_folder(v: &View) -> &str {
    v.paths
        .stable
        .as_ref()
        .and_then(|p| p.game.as_deref())
        .or_else(|| v.paths.lazer.as_ref().and_then(|p| p.data.as_deref()))
        .unwrap_or("")
}
fn songs_folder(v: &View) -> &str {
    v.paths
        .stable
        .as_ref()
        .and_then(|p| p.songs.as_deref())
        .or_else(|| v.paths.lazer.as_ref().and_then(|p| p.files.as_deref()))
        .unwrap_or("")
}
fn legacy_hits(p: &view::Play) -> vosu_osu_model::LegacyHitsExtended {
    to_legacy_hits(
        p.mode.number as i32,
        &Statistics {
            miss: p.judgements.miss,
            meh: p.judgements.meh,
            ok: p.judgements.ok,
            good: p.judgements.good,
            great: p.judgements.great,
            perfect: p.judgements.perfect,
            slider_tail_hit: p.statistics.slider_end_hits,
            small_tick_hit: p.statistics.small_tick_hit,
            large_tick_hit: p.statistics.large_tick_hit,
            small_tick_miss: p.statistics.small_tick_miss,
            large_tick_miss: p.statistics.large_tick_miss,
            large_bonus: p.statistics.large_bonus,
            ..vosu_osu_model::empty_statistics()
        },
    )
}
fn player(p: &view::Player) -> LeaderboardPlayer {
    player_fields(
        &p.name,
        p.score,
        p.combo,
        p.max_combo,
        &p.mods.name,
        &p.judgements,
        p.team,
        p.position,
        p.is_passing,
    )
}
fn slot(p: &view::Slot) -> LeaderboardPlayer {
    player_fields(
        p.name.as_deref().unwrap_or(""),
        p.score,
        p.combo.current,
        p.combo.max,
        &p.mods.name,
        &p.statistics,
        p.team,
        p.position,
        !p.is_failed,
    )
}
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors the two distinct public leaderboard source records"
)]
fn player_fields(
    name: &str,
    score: f64,
    combo: i64,
    max_combo: i64,
    mods: &str,
    h: &vosu_osu_model::source::Judgements,
    team: i64,
    position: i64,
    passing: bool,
) -> LeaderboardPlayer {
    LeaderboardPlayer {
        name: name.into(),
        score,
        combo,
        max_combo,
        mods: mods.into(),
        h300: h.great,
        geki: h.perfect,
        h100: h.ok,
        katu: h.good,
        h50: h.meh,
        h0: h.miss,
        team,
        position,
        is_passing: i64::from(passing),
    }
}
fn profile(p: &view::Profile) -> UserProfile {
    UserProfile {
        raw_login_status: p.user_status.number,
        name: p.name.clone(),
        accuracy: p.accuracy,
        ranked_score: p.ranked_score,
        id: p.id,
        level: p.level,
        play_count: p.play_count,
        play_mode: p.mode.number,
        rank: p.global_rank,
        country_code: p.country_code.number,
        performance_points: p.pp,
        raw_bancho_status: p.bancho_status.number,
        background_colour: p.background_colour.clone(),
    }
}
#[cfg(test)]
mod tests;
