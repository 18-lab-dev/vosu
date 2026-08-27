use std::{
    io::{self, Write},
    path::{Component, PathBuf},
};

use bytes::Bytes;
use serde::Serialize;
use serde_json::Value;
use serde_json::ser::{Formatter, Serializer};

use crate::api::{ApiError, ApiSchema, CombinedSnapshot, ManagerInstanceSnapshot};
use crate::calculation::beatmap::ParsedBeatmap;
use crate::calculation::{calculate_grade, fix_decimals, fix_decimals_to, to_legacy_hits};
use crate::model::api::v2::*;
use crate::model::enums::ClientType;
use crate::model::memory::{LazerSpectatorEntry, User};
use crate::model::states::{CalculateMods, HitErrorArray, LeaderboardPlayer, SettingsObject};
use crate::state::{GameplayState, RegularState};

pub fn build(snapshot: &CombinedSnapshot) -> Result<Response, ApiError> {
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
        beatmap: build_beatmap(
            state,
            parsed,
            source,
            current_mode,
            focused.precise.play_time,
        ),
        play: build_play(
            &state.gameplay,
            state.beatmap.attributes.as_ref(),
            current_mods,
            &focused.precise.hit_errors,
        ),
        keypresses: crate::api::precise::build_keypresses(&focused.precise.keypresses),
        leaderboard: state
            .gameplay
            .leaderboard_scores
            .iter()
            .map(|player| build_leaderboard(player, source, state.gameplay.mode))
            .collect(),
        performance: build_performance(state)?,
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
    })
}

pub fn encode(snapshot: &CombinedSnapshot) -> Result<Bytes, ApiError> {
    snapshot.encode_unfiltered(ApiSchema::V2, |snapshot| {
        encode_dto_with_capacity(&build(snapshot)?, 4 * 1024)
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
    encode_dto_with_capacity(value, 0)
}

fn encode_dto_with_capacity<T: Serialize>(value: &T, capacity: usize) -> Result<Bytes, ApiError> {
    let mut bytes = Vec::with_capacity(capacity);
    let mut serializer = Serializer::with_formatter(&mut bytes, JsFormatter);
    value
        .serialize(&mut serializer)
        .map_err(|error| ApiError::serialization(error.to_string()))?;
    Ok(Bytes::from(bytes))
}

pub(crate) struct JsFormatter;

impl Formatter for JsFormatter {
    fn write_f32<W>(&mut self, writer: &mut W, value: f32) -> io::Result<()>
    where
        W: ?Sized + Write,
    {
        crate::calculation::write_js_json_number(writer, f64::from(value))
    }

    fn write_f64<W>(&mut self, writer: &mut W, value: f64) -> io::Result<()>
    where
        W: ?Sized + Write,
    {
        crate::calculation::write_js_json_number(writer, value)
    }
}

mod builders;
pub(crate) mod names;
mod tourney;

use builders::*;
use names::*;
use tourney::*;

#[cfg(test)]
mod tests;
