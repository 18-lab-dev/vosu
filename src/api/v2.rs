use std::path::{Component, PathBuf};

use bytes::Bytes;
use serde::Serialize;
use serde_json::Value;

use crate::api::fixed::{fix_decimals, fix_decimals_to};
use crate::api::{ApiError, ApiSchema, CombinedSnapshot, ManagerInstanceSnapshot};
use crate::calculation::beatmap::ParsedBeatmap;
use crate::calculation::{calculate_grade, to_legacy_hits};
use crate::model::api::v2::*;
use crate::model::enums::ClientType;
use crate::model::memory::{LazerSpectatorEntry, User};
use crate::model::states::{CalculateMods, HitErrorArray, LeaderboardPlayer, SettingsObject};
use crate::state::{GameplayState, RegularState};

pub fn build(snapshot: &CombinedSnapshot) -> Result<Response, ApiError> {
    build_internal(snapshot, true)
}

fn build_internal<'a, V: From<&'a [f64]>>(
    snapshot: &'a CombinedSnapshot,
    include_graph_data: bool,
) -> Result<Response<V>, ApiError> {
    let focused = snapshot.focused().ok_or(ApiError::NotReady)?;
    let state = focused.regular.as_ref();
    let source = focused.source;
    let status = focused.precise.status;
    let manager_instance = snapshot
        .manager()
        .focused_instance()
        .ok_or(ApiError::NotReady)?;
    let current_mods = select_mods(state, status);
    let current_mode = select_mode(state, status);
    let parsed = state.beatmap.parsed.as_deref();

    Ok(Response {
        game: Game {
            focused: cfg!(target_os = "windows").then(|| snapshot.manager().game_focused()),
            paused: state.global.paused,
        },
        client: client_name(source).to_owned(),
        server: manager_instance
            .settings()
            .devserver_endpoint
            .as_ref()
            .map_or_else(|| "ppy.sh".to_owned(), |v| v.to_string_lossy().into_owned()),
        state: number_name(Some(status), game_state_name(status)),
        session: Session {
            play_time: state.global.game_time as f64,
            play_count: 0,
        },
        ranked_play: state.ranked_play.info.clone(),
        settings: build_settings(state, source),
        profile: build_profile(state.user.profile.as_ref()),
        beatmap: build_beatmap(state, parsed, current_mode, focused.precise.play_time),
        play: build_play(
            &state.gameplay,
            state.beatmap.attributes.as_ref(),
            current_mods,
            &focused.precise.hit_errors,
        ),
        keys: crate::api::precise::build_keys(
            &focused.precise.key_overlay,
            &focused.precise.keypresses,
        ),
        leaderboard: state
            .gameplay
            .leaderboard_scores
            .iter()
            .map(|player| build_leaderboard(player, source, state.gameplay.mode))
            .collect(),
        performance: build_performance(state, include_graph_data)?,
        results_screen: build_results(state),
        folders: Folders {
            game: state.global.game_folder.clone(),
            skin: state.global.skin_folder.clone(),
            songs: state.global.songs_folder.clone(),
            beatmap: state.menu.folder.clone(),
        },
        files: Files {
            beatmap: state.menu.filename.clone(),
            background: state.menu.background_filename.clone(),
            audio: state.menu.audio_filename.clone(),
        },
        direct_path: DirectPath {
            beatmap_file: direct_join(state.menu.folder.as_deref(), state.menu.filename.as_deref()),
            beatmap_background: direct_join(
                state.menu.folder.as_deref(),
                state.menu.background_filename.as_deref(),
            ),
            beatmap_audio: direct_join(
                state.menu.folder.as_deref(),
                state.menu.audio_filename.as_deref(),
            ),
            beatmap_folder: state.menu.folder.clone(),
            skin_folder: state.global.skin_folder.clone(),
        },
        tourney: build_tourney(snapshot, source, state, status)?,
        room: state.room.info.as_ref().map(|room| Room {
            room_id: room.room_id,
            users: room
                .users
                .iter()
                .map(|user| RoomUser {
                    id: user.id,
                    username: user.username.clone(),
                    country_code: user.country_code,
                    avatar_url: user.avatar_url.clone(),
                })
                .collect(),
        }),
    })
}

pub fn encode(snapshot: &CombinedSnapshot) -> Result<Bytes, ApiError> {
    snapshot.encode_unfiltered(ApiSchema::V2, |snapshot| {
        let direct = wire::encode(snapshot);
        #[cfg(debug_assertions)]
        {
            let dto = build_internal::<&[f64]>(snapshot, false)
                .and_then(|response| encode_dto(&response));
            assert_eq!(
                direct.as_deref().map(String::from_utf8_lossy),
                dto.as_deref().map(String::from_utf8_lossy),
                "direct V2 encoding diverged from the DTO contract",
            );
        }
        direct.map(Bytes::from)
    })
}

pub(crate) fn document(snapshot: &CombinedSnapshot) -> Result<Value, ApiError> {
    let mut document = serde_json::to_value(build(snapshot)?)
        .map_err(|error| ApiError::serialization(error.to_string()))?;
    normalize_javascript_numbers(&mut document)?;
    Ok(document)
}

fn normalize_javascript_numbers(value: &mut Value) -> Result<(), ApiError> {
    match value {
        Value::Number(number) if number.is_f64() => {
            let value = number.as_f64().expect("checked f64 JSON number");
            *number = crate::calculation::js_json_number(value)
                .parse()
                .map_err(|error: serde_json::Error| ApiError::serialization(error.to_string()))?;
        }
        Value::Array(values) => {
            for value in values {
                normalize_javascript_numbers(value)?;
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                normalize_javascript_numbers(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn encode_dto<T: Serialize>(value: &T) -> Result<Bytes, ApiError> {
    crate::api::json::encode(value, 0).map(Bytes::from)
}

mod builders;
pub(crate) mod names;
mod tourney;
mod wire;

use builders::*;
use names::*;
use tourney::*;

#[cfg(test)]
mod tests;
