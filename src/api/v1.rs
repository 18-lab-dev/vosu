#[path = "v1/helpers.rs"]
mod helpers;

#[cfg(test)]
use helpers::join_node_path;
use helpers::joined_path;

use std::cmp::Ordering;
use std::io::{self, Write};

use bytes::Bytes;
use serde::Serialize;
use serde_json::ser::{Formatter, Serializer};

use crate::api::{ApiError, ApiSchema, CombinedSnapshot, ManagerInstanceSnapshot};
use crate::calculation::{
    calculate_grade, fix_decimals, fix_decimals_to, to_legacy_hits, write_js_json_number,
};
use crate::model::api::v1::*;
use crate::model::enums::{ClientType, CountryCodes, GameState};
use crate::model::memory::{LazerSpectatorEntry, Score};
use crate::model::states::{CalculateMods, MultiplayerTeamType, Statistics};
use crate::state::{GameplayState, LeaderboardPlayerState, PreciseState, RegularState};

pub fn build(snapshot: &CombinedSnapshot) -> Result<Response, ApiError> {
    let focused = snapshot.focused().ok_or(ApiError::NotReady)?;
    let regular = &focused.regular;
    let precise = &focused.precise;
    let current_mods = selected_mods(precise.status, regular);
    let parsed = regular.beatmap.parsed.as_deref();
    let progressive = regular.beatmap.attributes.unwrap_or_default();
    let graph = &regular.beatmap.graph;
    let menu = &regular.menu;
    let gameplay = &regular.gameplay;
    let result = &regular.result_screen;
    let legacy_hits = to_legacy_hits(gameplay.mode as i32, &gameplay.statistics);

    let default_original = crate::calculation::beatmap::DifficultyValues::default();
    let default_converted = crate::calculation::beatmap::DifficultyValues::default();
    let (original, converted, counts, full_stars, max_combo, times, bpm) = if let Some(map) = parsed
    {
        (
            &map.engine.original_difficulty,
            &map.engine.converted_difficulty,
            map.object_counts,
            map.engine.difficulty.stars,
            i64::from(map.engine.maximum_score.max_combo),
            map.times,
            map.bpm,
        )
    } else {
        (
            &default_original,
            &default_converted,
            Default::default(),
            0.0,
            0,
            Default::default(),
            Default::default(),
        )
    };

    let pp = |accuracy| {
        fix_decimals(
            regular
                .beatmap
                .pp_acc
                .get(&accuracy)
                .copied()
                .unwrap_or(0.0),
        )
    };
    let key = |index: usize| {
        precise
            .key_overlay
            .get(index)
            .map(|button| KeyOverlayButton {
                is_pressed: button.is_pressed,
                count: button.count,
            })
            .unwrap_or(KeyOverlayButton {
                is_pressed: false,
                count: 0,
            })
    };

    Ok(Response {
        client: match focused.source {
            ClientType::Stable => "stable",
            ClientType::Lazer => "lazer",
        }
        .to_owned(),
        settings: Settings {
            show_interface: regular.global.show_interface,
            folders: Folders {
                game: regular.global.game_folder.clone(),
                skin: regular.global.skin_folder.clone(),
                songs: regular.global.songs_folder.clone(),
            },
        },
        menu: Menu {
            main_menu: MainMenu {
                bass_density: regular.bass_density.density,
            },
            state: precise.status,
            game_mode: menu.gamemode,
            is_chat_enabled: i64::from(regular.global.chat_status != 0),
            bm: Beatmap {
                time: BeatmapTimings {
                    first_obj: i64::from(times.first_object),
                    current: precise.play_time,
                    full: times.full,
                    mp3: menu.mp3_length,
                },
                id: menu.map_id,
                set: menu.set_id,
                md5: menu.checksum.clone(),
                ranked_status: menu.ranked_status,
                metadata: BeatmapMetadata {
                    artist: menu.artist.clone(),
                    artist_original: menu.artist_original.clone(),
                    title: menu.title.clone(),
                    title_original: menu.title_original.clone(),
                    mapper: menu.creator.clone(),
                    difficulty: menu.difficulty.clone(),
                },
                stats: BeatmapStats {
                    ar: fix_decimals(converted.approach_rate),
                    cs: fix_decimals(converted.circle_size),
                    od: fix_decimals(converted.overall_difficulty),
                    hp: fix_decimals(converted.drain_rate),
                    sr: fix_decimals(progressive.stars),
                    bpm: Bpm {
                        realtime: fix_decimals_to(regular.beatmap.event_status.bpm, 4)
                            .map_err(|error| ApiError::serialization(error.to_string()))?,
                        common: fix_decimals_to(bpm.common, 4)
                            .map_err(|error| ApiError::serialization(error.to_string()))?,
                        min: fix_decimals_to(bpm.minimum, 4)
                            .map_err(|error| ApiError::serialization(error.to_string()))?,
                        max: fix_decimals_to(bpm.maximum, 4)
                            .map_err(|error| ApiError::serialization(error.to_string()))?,
                    },
                    circles: i64::from(counts.circles),
                    sliders: i64::from(counts.sliders),
                    spinners: i64::from(counts.spinners),
                    holds: i64::from(counts.holds),
                    max_combo,
                    full_sr: fix_decimals(full_stars),
                    memory_ar: fix_decimals(original.approach_rate),
                    memory_cs: fix_decimals(original.circle_size),
                    memory_od: fix_decimals(original.overall_difficulty),
                    memory_hp: fix_decimals(original.drain_rate),
                },
                path: BeatmapPaths {
                    full: joined_path(
                        menu.folder.as_deref().unwrap_or(""),
                        menu.background_filename.as_deref().unwrap_or(""),
                    ),
                    folder: menu.folder.clone(),
                    file: menu.filename.clone(),
                    bg: menu.background_filename.clone(),
                    audio: menu.audio_filename.clone(),
                },
            },
            mods: current_mods,
            pp: PerformancePoints {
                acc_100: pp(100),
                acc_99: pp(99),
                acc_98: pp(98),
                acc_97: pp(97),
                acc_96: pp(96),
                acc_95: pp(95),
                acc_94: pp(94),
                acc_93: pp(93),
                acc_92: pp(92),
                acc_91: pp(91),
                acc_90: pp(90),
                strains: graph.primary.clone(),
                strains_all: BeatmapStrains {
                    series: graph
                        .series
                        .iter()
                        .map(|series| BeatmapStrainSeries {
                            name: series.name.clone(),
                            data: series.data.clone(),
                        })
                        .collect(),
                    xaxis: graph.x_axis.clone(),
                },
            },
        },
        gameplay: Gameplay {
            game_mode: gameplay.mode,
            name: gameplay.player_name.clone(),
            score: gameplay.score,
            accuracy: gameplay.accuracy,
            combo: Combo {
                current: gameplay.combo,
                max: gameplay.max_combo,
            },
            hp: Health {
                normal: gameplay.player_hp,
                smooth: gameplay.player_hp_smooth,
            },
            hits: GameplayHits {
                h300: legacy_hits.n300,
                geki: legacy_hits.geki,
                h100: legacy_hits.n100,
                katu: legacy_hits.katu,
                h50: legacy_hits.n50,
                h0: legacy_hits.n0,
                slider_end_hits: legacy_hits.slider_end_hits,
                small_tick_hits: legacy_hits.small_tick_hits,
                large_tick_hits: legacy_hits.large_tick_hits,
                slider_breaks: gameplay.slider_breaks,
                grade: GameplayGrade {
                    current: gameplay.grade_current.clone(),
                    max_this_play: gameplay.grade_expected.clone(),
                },
                unstable_rate: gameplay.unstable_rate,
                hit_error_array: precise.hit_errors.as_ref().clone(),
            },
            pp: GameplayPerformancePoints {
                current: fix_decimals(progressive.pp),
                fc: fix_decimals(progressive.fc_pp),
                max_this_play: fix_decimals(progressive.max_achievable),
            },
            key_overlay: KeyOverlay {
                k1: key(0),
                k2: key(1),
                m1: key(2),
                m2: key(3),
            },
            leaderboard: Leaderboard {
                has_leaderboard: !gameplay.leaderboard_scores.is_empty(),
                is_visible: gameplay.leaderboard_visible,
                ourplayer: leaderboard_player_state(&gameplay.leaderboard_player),
                slots: gameplay
                    .leaderboard_scores
                    .iter()
                    .map(leaderboard_player)
                    .collect(),
            },
            is_replay_ui_hidden: false,
        },
        results_screen: ResultsScreen {
            mode: gameplay.mode,
            name: result.raw.player_name.clone(),
            score: result.raw.score,
            accuracy: result.raw.accuracy,
            max_combo: result.raw.max_combo,
            mods: mods(&result.raw.mods),
            h300: result.raw.statistics.great,
            geki: result.raw.statistics.perfect,
            h100: result.raw.statistics.ok,
            katu: result.raw.statistics.good,
            h50: result.raw.statistics.meh,
            h0: result.raw.statistics.miss,
            grade: result.grade.clone(),
            created_at: result.raw.date.clone(),
        },
        user_profile: user_profile(regular),
        tourney: if focused.source == ClientType::Lazer
            && precise.status == i64::from(GameState::Lobby.value())
        {
            build_lazer_tourney(regular, precise.status)?
        } else {
            Some(build_stable_tourney(snapshot))
        },
    })
}

pub fn encode(snapshot: &CombinedSnapshot) -> Result<Bytes, ApiError> {
    snapshot.encode_unfiltered(ApiSchema::V1, |snapshot| {
        let value = build(snapshot)?;
        let mut bytes = Vec::new();
        let mut serializer = Serializer::with_formatter(&mut bytes, JsFormatter);
        value
            .serialize(&mut serializer)
            .map_err(|error| ApiError::serialization(error.to_string()))?;
        Ok(Bytes::from(bytes))
    })
}

struct JsFormatter;

impl Formatter for JsFormatter {
    fn write_f32<W>(&mut self, writer: &mut W, value: f32) -> io::Result<()>
    where
        W: ?Sized + Write,
    {
        write_js_json_number(writer, f64::from(value))
    }

    fn write_f64<W>(&mut self, writer: &mut W, value: f64) -> io::Result<()>
    where
        W: ?Sized + Write,
    {
        write_js_json_number(writer, value)
    }
}

fn selected_mods(status: i64, state: &RegularState) -> SelectedMods {
    if status == i64::from(GameState::Play.value()) {
        mods(&state.gameplay.mods)
    } else if status == i64::from(GameState::ResultScreen.value()) {
        mods(&state.result_screen.raw.mods)
    } else {
        mods(&state.global.menu_mods)
    }
}

fn mods(value: &CalculateMods) -> SelectedMods {
    SelectedMods {
        num: value.number,
        text: value.name.clone(),
    }
}

fn leaderboard_player_state(player: &LeaderboardPlayerState) -> LeaderboardPlayer {
    leaderboard_fields(
        &player.name,
        player.score,
        player.combo,
        player.max_combo,
        &player.mods,
        &player.statistics,
        player.team,
        player.position,
        player.is_passing,
    )
}

fn leaderboard_player(player: &crate::model::states::LeaderboardPlayer) -> LeaderboardPlayer {
    leaderboard_fields(
        &player.name,
        player.score,
        player.combo,
        player.max_combo,
        &player.mods,
        &player.statistics,
        player.team,
        player.position,
        player.is_passing,
    )
}

#[allow(clippy::too_many_arguments)]
fn leaderboard_fields(
    name: &str,
    score: f64,
    combo: i64,
    max_combo: i64,
    mods_value: &CalculateMods,
    statistics: &Statistics,
    team: i64,
    position: i64,
    is_passing: bool,
) -> LeaderboardPlayer {
    LeaderboardPlayer {
        name: name.to_owned(),
        score,
        combo,
        max_combo,
        mods: mods_value.name.clone(),
        h300: statistics.great,
        geki: statistics.perfect,
        h100: statistics.ok,
        katu: statistics.good,
        h50: statistics.meh,
        h0: statistics.miss,
        team,
        position,
        is_passing: i64::from(is_passing),
    }
}

fn user_profile(state: &RegularState) -> UserProfile {
    let Some(user) = state.user.profile.as_ref() else {
        return UserProfile::default();
    };
    UserProfile {
        raw_login_status: Some(user.raw_login_status),
        name: Some(user.name.clone()),
        accuracy: Some(user.accuracy),
        ranked_score: Some(user.ranked_score),
        id: Some(user.id),
        level: Some(user.level),
        play_count: Some(user.play_count),
        play_mode: Some(user.play_mode),
        rank: Some(user.rank),
        country_code: Some(user.country_code),
        performance_points: Some(user.performance_points),
        raw_bancho_status: Some(user.raw_bancho_status),
        background_colour: Some(if user.background_colour < 0 {
            format!("-{:x}", user.background_colour.unsigned_abs())
        } else {
            format!("{:x}", user.background_colour)
        }),
    }
}

fn build_stable_tourney(snapshot: &CombinedSnapshot) -> Tourney {
    let mut manager = None;
    let mut spectators = Vec::new();
    for instance in snapshot.manager().instances().values() {
        if manager.is_none() && instance.is_tournament_manager() {
            manager = Some(instance);
        }
        if instance.settings().tournament_spectator {
            spectators.push(instance);
        }
    }
    for index in 1..spectators.len() {
        let mut cursor = index;
        while cursor > 0
            && compare_ipc_id(spectators[cursor - 1], spectators[cursor]) == Ordering::Greater
        {
            spectators.swap(cursor - 1, cursor);
            cursor -= 1;
        }
    }
    let total = spectators.len();
    let clients: Vec<_> = spectators
        .into_iter()
        .enumerate()
        .map(|(index, instance)| stable_tourney_client(instance, index, total))
        .collect();

    let Some(manager_instance) = manager else {
        return Tourney {
            manager: default_tourney_manager(),
            ipc_clients: Vec::new(),
        };
    };
    let state = &manager_instance.state().regular.tourney;
    let board = &state.scoreboard;
    Tourney {
        manager: TourneyManager {
            ipc_state: board.ipc_state,
            best_of: board.best_of,
            team_name: TourneyName {
                left: board.first_team_name.clone(),
                right: board.second_team_name.clone(),
            },
            stars: TourneyStars {
                left: board.left_stars,
                right: board.right_stars,
            },
            bools: TourneyBools {
                score_visible: board.score_visible,
                stars_visible: board.stars_visible,
            },
            chat: map_chat(&state.messages, &clients),
            gameplay: TourneyManagerGameplay {
                score: TourneyScore {
                    left: board.first_team_score as f64,
                    right: board.second_team_score as f64,
                },
            },
        },
        ipc_clients: clients,
    }
}

fn compare_ipc_id(left: &ManagerInstanceSnapshot, right: &ManagerInstanceSnapshot) -> Ordering {
    match (
        left.settings().tournament_ipc_id,
        right.settings().tournament_ipc_id,
    ) {
        (Some(left), Some(right)) => left.partial_cmp(&right).unwrap_or(Ordering::Equal),
        _ => Ordering::Equal,
    }
}

fn stable_tourney_client(
    instance: &ManagerInstanceSnapshot,
    index: usize,
    total: usize,
) -> TourneyIpcClient {
    let regular = &instance.state().regular;
    let gameplay = &regular.gameplay;
    let precise = &instance.state().precise;
    let user = regular.tourney.user.as_ref();
    TourneyIpcClient {
        team: if index < total.div_ceil(2) {
            "left"
        } else {
            "right"
        }
        .to_owned(),
        spectating: TourneyIpcSpec {
            name: user.map(|value| value.name.clone()).unwrap_or_default(),
            country: user.map(|value| value.country.clone()).unwrap_or_default(),
            user_id: user.map(|value| value.id).unwrap_or(0),
            accuracy: user.map(|value| value.accuracy).unwrap_or(0.0),
            ranked_score: user.map(|value| value.ranked_score).unwrap_or(0.0),
            play_count: user.map(|value| value.play_count).unwrap_or(0),
            global_rank: 0,
            total_pp: user.map(|value| value.pp).unwrap_or(0.0),
        },
        gameplay: tournament_gameplay(gameplay, &selected_mods(precise.status, regular), precise),
    }
}

fn default_tourney_manager() -> TourneyManager {
    TourneyManager {
        ipc_state: 0,
        best_of: 0,
        team_name: TourneyName {
            left: String::new(),
            right: String::new(),
        },
        stars: TourneyStars { left: 0, right: 0 },
        bools: TourneyBools {
            score_visible: false,
            stars_visible: false,
        },
        chat: Vec::new(),
        gameplay: TourneyManagerGameplay {
            score: TourneyScore {
                left: 0.0,
                right: 0.0,
            },
        },
    }
}

fn map_chat(
    messages: &[crate::model::states::TourneyManagerChatItem],
    clients: &[TourneyIpcClient],
) -> Vec<TourneyMessage> {
    messages
        .iter()
        .map(|message| TourneyMessage {
            team: clients
                .iter()
                .find(|client| client.spectating.name == message.name)
                .map(|client| client.team.clone())
                .unwrap_or_else(|| {
                    if message.name == "BanchoBot" {
                        "bot".to_owned()
                    } else {
                        "unknown".to_owned()
                    }
                }),
            time: message.time.clone(),
            name: message.name.clone(),
            message_body: message.content.clone(),
        })
        .collect()
}

fn build_lazer_tourney(state: &RegularState, status: i64) -> Result<Option<Tourney>, ApiError> {
    let Some(data) = state.lazer_multi_spectating.data.as_ref() else {
        return Ok(None);
    };
    let clients: Vec<_> = data
        .spectating_clients
        .iter()
        .map(|client| lazer_tourney_client(state, status, client))
        .collect::<Result<_, _>>()?;
    let left = data
        .spectating_clients
        .iter()
        .filter(|client| client.team == MultiplayerTeamType::Red)
        .map(|client| client.score.as_ref().map_or(0.0, |score| score.score))
        .sum();
    let right = data
        .spectating_clients
        .iter()
        .filter(|client| client.team == MultiplayerTeamType::Blue)
        .map(|client| client.score.as_ref().map_or(0.0, |score| score.score))
        .sum();
    Ok(Some(Tourney {
        manager: TourneyManager {
            ipc_state: status,
            best_of: 0,
            team_name: TourneyName {
                left: String::new(),
                right: String::new(),
            },
            stars: TourneyStars { left: 0, right: 0 },
            bools: TourneyBools {
                score_visible: status == i64::from(GameState::Lobby.value()),
                stars_visible: false,
            },
            chat: map_chat(&data.chat, &clients),
            gameplay: TourneyManagerGameplay {
                score: TourneyScore { left, right },
            },
        },
        ipc_clients: clients,
    }))
}

fn lazer_tourney_client(
    state: &RegularState,
    status: i64,
    client: &LazerSpectatorEntry,
) -> Result<TourneyIpcClient, ApiError> {
    let score = client
        .score
        .as_ref()
        .ok_or_else(|| ApiError::serialization("lazer tournament spectator has no score"))?;
    let selected = if status == i64::from(GameState::Play.value()) {
        mods(&score.mods)
    } else if status == i64::from(GameState::ResultScreen.value()) {
        client
            .result_screen
            .as_ref()
            .map(|result| mods(&result.mods))
            .ok_or_else(|| {
                ApiError::serialization("lazer tournament spectator has no result screen")
            })?
    } else {
        mods(&state.global.menu_mods)
    };
    Ok(TourneyIpcClient {
        team: if client.team == MultiplayerTeamType::Red {
            "left"
        } else {
            "right"
        }
        .to_owned(),
        spectating: TourneyIpcSpec {
            name: client.user.name.clone(),
            country: CountryCodes::from_value(client.user.country_code as i32)
                .map(|country| format!("{country:?}").to_uppercase())
                .unwrap_or_default(),
            user_id: client.user.id,
            accuracy: client.user.accuracy,
            ranked_score: client.user.ranked_score,
            play_count: client.user.play_count,
            global_rank: client.user.rank,
            total_pp: client.user.performance_points,
        },
        gameplay: lazer_gameplay(score, selected),
    })
}

fn tournament_gameplay(
    gameplay: &GameplayState,
    selected: &SelectedMods,
    precise: &PreciseState,
) -> TourneyGameplay {
    TourneyGameplay {
        game_mode: gameplay.mode,
        score: gameplay.score,
        name: gameplay.player_name.clone(),
        accuracy: gameplay.accuracy,
        hits: TourneyHits {
            h300: gameplay.statistics.great,
            geki: gameplay.statistics.perfect,
            h100: gameplay.statistics.ok,
            katu: gameplay.statistics.good,
            h50: gameplay.statistics.meh,
            h0: gameplay.statistics.miss,
            slider_breaks: gameplay.slider_breaks,
            grade: GameplayGrade {
                current: gameplay.grade_current.clone(),
                max_this_play: gameplay.grade_expected.clone(),
            },
            unstable_rate: gameplay.unstable_rate,
            hit_error_array: precise.hit_errors.as_ref().clone(),
        },
        combo: Combo {
            current: gameplay.combo,
            max: gameplay.max_combo,
        },
        mods: selected.clone(),
        hp: Health {
            normal: gameplay.player_hp,
            smooth: gameplay.player_hp_smooth,
        },
    }
}

fn lazer_gameplay(score: &Score, selected: SelectedMods) -> TourneyGameplay {
    let grade = calculate_grade(
        true,
        &score.mods.array,
        score.mode as i32,
        score.accuracy,
        &score.statistics,
    );
    TourneyGameplay {
        game_mode: score.mode,
        score: score.score,
        name: score.player_name.clone(),
        accuracy: score.accuracy,
        hits: TourneyHits {
            h300: score.statistics.great,
            geki: score.statistics.perfect,
            h100: score.statistics.ok,
            katu: score.statistics.good,
            h50: score.statistics.meh,
            h0: score.statistics.miss,
            slider_breaks: 0,
            grade: GameplayGrade {
                current: grade,
                max_this_play: String::new(),
            },
            unstable_rate: 0.0,
            hit_error_array: Default::default(),
        },
        combo: Combo {
            current: score.combo,
            max: score.max_combo,
        },
        mods: selected,
        hp: Health {
            normal: score.player_hp,
            smooth: score.player_hp_smooth,
        },
    }
}

#[cfg(test)]
#[path = "v1/tests.rs"]
mod tests;
