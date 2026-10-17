use super::*;

pub(super) fn empty_tourney() -> Tourney {
    Tourney {
        score_visible: false,
        stars_visible: false,
        ipc_state: 0,
        best_of: 0,
        team: Sides {
            left: String::new(),
            right: String::new(),
        },
        points: Sides { left: 0, right: 0 },
        chat: vec![],
        total_score: Sides {
            left: 0.0,
            right: 0.0,
        },
        clients: vec![],
    }
}
pub(super) fn build_tourney(
    snapshot: &CombinedSnapshot,
    source: ClientType,
    state: &RegularState,
    status: i64,
) -> Result<Option<Tourney>, ApiError> {
    if source == ClientType::Lazer && status == 11 {
        return state
            .lazer_multi_spectating
            .data
            .as_ref()
            .map(|data| build_lazer_tourney(state, data, status))
            .transpose();
    }
    let manager = snapshot
        .manager()
        .instances()
        .values()
        .find(|v| v.is_tournament_manager());
    let Some(manager) = manager else {
        return Ok(Some(empty_tourney()));
    };
    let mut clients: Vec<_> = snapshot
        .manager()
        .instances()
        .values()
        .filter(|v| v.settings().tournament_spectator)
        .collect();
    clients.sort_by(|a, b| {
        a.settings()
            .tournament_ipc_id
            .partial_cmp(&b.settings().tournament_ipc_id)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut mapped = Vec::with_capacity(clients.len());
    for (index, client) in clients.iter().enumerate() {
        mapped.push(build_stable_client(
            client,
            index,
            clients.len(),
            snapshot.runtime_settings().enable_mania_key_hit_errors,
            snapshot.runtime_settings().enable_hit_offsets,
        )?);
    }
    let tourney = &manager.state().regular.tourney;
    let score = &tourney.scoreboard;
    let chat = tourney
        .messages
        .iter()
        .map(|m| {
            let team = mapped.iter().find(|c| c.user.name == m.name).map_or_else(
                || {
                    if m.name == "BanchoBot" {
                        "bot"
                    } else {
                        "unknown"
                    }
                },
                |c| match c.team {
                    TourneyTeam::Left => "left",
                    TourneyTeam::Right => "right",
                },
            );
            TourneyChatMessage::Stable(StableTourneyChatMessage {
                team: team.to_owned(),
                name: m.name.clone(),
                message: m.content.clone(),
                timestamp: m.time.clone(),
            })
        })
        .collect();
    Ok(Some(Tourney {
        score_visible: score.score_visible,
        stars_visible: score.stars_visible,
        ipc_state: score.ipc_state,
        best_of: score.best_of,
        team: Sides {
            left: score.first_team_name.clone(),
            right: score.second_team_name.clone(),
        },
        points: Sides {
            left: score.left_stars,
            right: score.right_stars,
        },
        chat,
        total_score: Sides {
            left: score.first_team_score as f64,
            right: score.second_team_score as f64,
        },
        clients: mapped,
    }))
}
pub(super) fn build_stable_client(
    instance: &ManagerInstanceSnapshot,
    index: usize,
    len: usize,
    enable_key_hits: bool,
    enable_hit_offsets: bool,
) -> Result<TourneyClient, ApiError> {
    let bundle = instance.state();
    let state = bundle.regular.as_ref();
    let status = bundle.precise.status;
    let user = state.tourney.user.as_ref();
    let mods = select_mods(state, status);

    Ok(TourneyClient {
        ipc_id: instance
            .settings()
            .tournament_ipc_id
            .ok_or_else(|| ApiError::serialization("tournament spectator ipcId is unavailable"))?,
        team: if (index as f64) < len as f64 / 2.0 {
            TourneyTeam::Left
        } else {
            TourneyTeam::Right
        },
        settings: TourneyClientSettings {
            mania: TourneyManiaSettings {
                scroll_speed: i64_value(&state.settings.values, &["mania", "scrollSpeed"]),
            },
        },
        user: TourneyUser {
            id: user.map_or(0, |value| value.id),
            name: user.map_or_else(String::new, |value| value.name.clone()),
            country: user.map_or_else(String::new, |value| value.country.clone()),
            accuracy: user.map_or(0.0, |value| value.accuracy),
            ranked_score: user.map_or(0.0, |value| value.ranked_score),
            play_count: user.map_or(0, |value| value.play_count),
            global_rank: 0,
            total_pp: user.map_or(0.0, |value| value.pp),
        },
        beatmap: TourneyBeatmap {
            stats: build_stats(state, state.beatmap.parsed.as_deref()),
        },
        play: build_play(
            &state.gameplay,
            state.beatmap.attributes.as_ref(),
            mods,
            &bundle.precise.hit_errors,
            enable_key_hits,
            bundle.precise.active_catcher(),
            enable_hit_offsets.then(|| bundle.precise.hit_offsets()),
        ),
    })
}
pub(super) fn build_lazer_tourney(
    state: &RegularState,
    data: &crate::model::memory::LazerSpectator,
    status: i64,
) -> Result<Tourney, ApiError> {
    let mut clients = Vec::with_capacity(data.spectating_clients.len());
    for (i, c) in data.spectating_clients.iter().enumerate() {
        clients.push(build_lazer_client(state, c, i, status)?);
    }
    let sum = |team| {
        data.spectating_clients
            .iter()
            .filter(|client| client.team == team)
            .map(|client| client.score.as_ref().map_or(0.0, |score| score.score))
            .sum()
    };
    Ok(Tourney {
        score_visible: status == 11,
        stars_visible: false,
        ipc_state: status,
        best_of: 0,
        team: Sides {
            left: String::new(),
            right: String::new(),
        },
        points: Sides { left: 0, right: 0 },
        chat: data
            .chat
            .iter()
            .map(|m| {
                TourneyChatMessage::Lazer(LazerTourneyChatMessage {
                    team: "none".to_owned(),
                    timestamp: m.time.clone(),
                    name: m.name.clone(),
                    message: m.content.clone(),
                })
            })
            .collect(),
        total_score: Sides {
            left: sum(crate::model::states::MultiplayerTeamType::Red),
            right: sum(crate::model::states::MultiplayerTeamType::Blue),
        },
        clients,
    })
}
pub(super) fn lazer_wire_team(team: crate::model::states::MultiplayerTeamType) -> TourneyTeam {
    match team {
        crate::model::states::MultiplayerTeamType::Red => TourneyTeam::Left,
        crate::model::states::MultiplayerTeamType::Blue
        | crate::model::states::MultiplayerTeamType::None => TourneyTeam::Right,
    }
}

pub(super) fn build_lazer_client(
    state: &RegularState,
    client: &LazerSpectatorEntry,
    index: usize,
    status: i64,
) -> Result<TourneyClient, ApiError> {
    let score = client
        .score
        .as_ref()
        .ok_or_else(|| ApiError::serialization("lazer spectator score is unavailable"))?;
    let mods = match status {
        2 => &score.mods,
        7 => {
            &client
                .result_screen
                .as_ref()
                .ok_or_else(|| ApiError::serialization("lazer spectator result is unavailable"))?
                .mods
        }
        _ => &state.global.menu_mods,
    };
    let hits = to_legacy_hits(score.mode as i32, &score.statistics);
    let grade = calculate_grade(
        true,
        &score.mods.array,
        score.mode as i32,
        score.accuracy,
        &score.statistics,
    );
    let play = Play {
        failed: false,
        player_name: score.player_name.clone(),
        mode: number_name(Some(score.mode), ruleset_name(score.mode)),
        score: score.score,
        accuracy: score.accuracy,
        health_bar: HealthBar {
            // Preserve upstream operation order: `/ 2` can differ by one ULP.
            normal: (score.player_hp / 200.0) * 100.0,
            smooth: (score.player_hp_smooth / 200.0) * 100.0,
        },
        hits: PlayHits::Lazer(LazerHits {
            h0: hits.n0,
            h50: hits.n50,
            h100: hits.n100,
            h300: hits.n300,
            geki: hits.geki,
            katu: hits.katu,
            slider_end_hits: hits.slider_end_hits,
            small_tick_hits: hits.small_tick_hits,
            large_tick_hits: hits.large_tick_hits,
            slider_breaks: 0,
        }),
        hit_error_array: Default::default(),
        press_hit_errors: Vec::new(),
        release_hit_errors: Vec::new(),
        key_hit_errors: None,
        catcher: None,
        hit_offsets: None,
        combo: Combo {
            current: score.combo,
            max: score.max_combo,
        },
        mods: mods.clone(),
        rank: Rank {
            current: grade.clone(),
            max_this_play: grade,
        },
        pp: Pp {
            current: fix_decimals(score.pp.unwrap_or(0.0)),
            fc: 0.0,
            max_achieved: 0.0,
            max_achievable: 0.0,
            detailed: DetailedPp {
                current: detailed(Default::default(), 0.0),
                fc: detailed(Default::default(), 0.0),
            },
        },
        unstable_rate: 0.0,
    };
    let team = lazer_wire_team(client.team);

    Ok(TourneyClient {
        ipc_id: index as f64,
        team,
        settings: TourneyClientSettings {
            mania: TourneyManiaSettings { scroll_speed: 0 },
        },
        user: TourneyUser {
            id: client.user.id,
            name: client.user.name.clone(),
            country: source_country_name(client.user.country_code).to_owned(),
            accuracy: client.user.accuracy,
            ranked_score: client.user.ranked_score,
            play_count: client.user.play_count,
            global_rank: client.user.rank,
            total_pp: client.user.performance_points,
        },
        beatmap: TourneyBeatmap {
            stats: zero_stats(hit_window(state.beatmap.parsed.as_deref())),
        },
        play,
    })
}
pub(super) fn zero_stats(hit_window: HitWindow) -> Stats {
    Stats {
        stars: Stars {
            live: 0.0,
            aim: Some(0.0),
            speed: Some(0.0),
            flashlight: Some(0.0),
            slider_factor: Some(0.0),
            stamina: Some(0.0),
            rhythm: Some(0.0),
            color: Some(0.0),
            reading: Some(0.0),
            hit_window: Some(0.0),
            total: 0.0,
        },
        ar: Ar {
            original: 0.0,
            converted: 0.0,
        },
        cs: Cs {
            original: 0.0,
            converted: 0.0,
        },
        od: Od {
            original: 0.0,
            converted: 0.0,
        },
        hit_window,
        hp: Hp {
            original: 0.0,
            converted: 0.0,
        },
        bpm: Bpm {
            realtime: 0.0,
            common: 0.0,
            min: 0.0,
            max: 0.0,
        },
        objects: Objects {
            circles: 0,
            sliders: 0,
            spinners: 0,
            holds: 0,
            total: 0,
        },
        max_combo: 0,
    }
}
