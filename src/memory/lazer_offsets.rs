//! Typed osu!lazer offset schema and validation.

use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

mod groups_01_20;
mod groups_21_38;
mod groups_39_58;
mod groups_59_66;

pub use groups_01_20::*;
pub use groups_21_38::*;
pub use groups_39_58::*;
pub use groups_59_66::*;

pub const FALLBACK_GAME_BASE_VTABLE: usize = 7_730_957_910_016;

#[derive(Debug, Error)]
pub enum LazerOffsetsError {
    #[error("invalid offsets JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid osu! version `{0}`; expected canonical x.y.z")]
    InvalidVersion(String),
    #[error("offset `{0}` must be nonzero")]
    Zero(&'static str),
    #[error("embedded offsets asset missing")]
    MissingEmbedded,
}

pub fn validate_version(version: &str) -> Result<(), LazerOffsetsError> {
    let mut parts = version.split('.');
    for _ in 0..3 {
        let part = parts
            .next()
            .ok_or_else(|| LazerOffsetsError::InvalidVersion(version.to_owned()))?;
        if part.is_empty()
            || !part.bytes().all(|byte| byte.is_ascii_digit())
            || (part.len() > 1 && part.starts_with('0'))
            || part.parse::<u64>().is_err()
        {
            return Err(LazerOffsetsError::InvalidVersion(version.to_owned()));
        }
    }
    if parts.next().is_some() {
        return Err(LazerOffsetsError::InvalidVersion(version.to_owned()));
    }
    Ok(())
}

/// Class-field offsets; zero denotes not yet measured, not an object address.
#[derive(Debug, Clone, Deserialize)]
pub struct OsuHitObjectOffsets {
    pub position: usize,
    #[serde(rename = "stackHeight")]
    pub stack_height: usize,
    pub scale: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManiaHitObjectOffsets {
    #[serde(rename = "column")]
    pub column: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct LazerOffsets {
    pub osu_version: String,
    pub game_base_vtable: Option<usize>,
    #[serde(rename = "osu.Game.Rulesets.Mania.Objects.ManiaHitObject", default)]
    pub mania_hit_object: Option<ManiaHitObjectOffsets>,
    #[serde(rename = "osu.Game.OsuGame")]
    pub osu_game_osu_game: OffsetGroup1,
    #[serde(rename = "osu.Framework.Game")]
    pub osu_framework_game: OffsetGroup2,
    #[serde(rename = "osu.Game.OsuGameBase")]
    pub osu_game_osu_game_base: OffsetGroup3,
    #[serde(rename = "osu.Game.Screens.Select.SongSelect")]
    pub osu_game_screens_select_song_select: OffsetGroup4,
    #[serde(rename = "osu.Game.Screens.Select.SoloSongSelect")]
    pub osu_game_screens_select_solo_song_select: Option<OffsetGroup5>,
    #[serde(rename = "osu.Game.Screens.SelectV2.SoloSongSelect")]
    pub osu_game_screens_select_v2_solo_song_select: Option<OffsetGroup6>,
    #[serde(rename = "osu.Game.Screens.Play.SubmittingPlayer")]
    pub osu_game_screens_play_submitting_player: OffsetGroup7,
    #[serde(rename = "osu.Game.Screens.Play.PlayerLoader")]
    pub osu_game_screens_play_player_loader: OffsetGroup8,
    #[serde(rename = "osu.Game.Screens.Play.SpectatorPlayer")]
    pub osu_game_screens_play_spectator_player: OffsetGroup9,
    #[serde(rename = "osu.Game.Beatmaps.FramedBeatmapClock")]
    pub osu_game_beatmaps_framed_beatmap_clock: OffsetGroup10,
    #[serde(rename = "osu.Framework.Timing.DecouplingFramedClock")]
    pub osu_framework_timing_decoupling_framed_clock: OffsetGroup11,
    #[serde(rename = "osu.Framework.Audio.Track.Track")]
    pub osu_framework_audio_track_track: OffsetGroup12,
    #[serde(rename = "osu.Framework.Timing.FramedClock")]
    pub osu_framework_timing_framed_clock: OffsetGroup13,
    #[serde(rename = "osu.Game.Screens.OsuScreen")]
    pub osu_game_screens_osu_screen: OffsetGroup14,
    #[serde(rename = "osu.Game.Screens.Ranking.SoloResultsScreen")]
    pub osu_game_screens_ranking_solo_results_screen: OffsetGroup15,
    #[serde(rename = "osu.Game.Screens.Edit.Editor")]
    pub osu_game_screens_edit_editor: OffsetGroup16,
    #[serde(rename = "osu.Game.Screens.OnlinePlay.OnlinePlayScreen")]
    pub osu_game_screens_online_play_online_play_screen: OffsetGroup17,
    #[serde(rename = "osu.Game.Screens.OnlinePlay.Multiplayer.Multiplayer")]
    pub osu_game_screens_online_play_multiplayer_multiplayer: OffsetGroup18,
    #[serde(rename = "osu.Game.Online.Multiplayer.MultiplayerRoom")]
    pub osu_game_online_multiplayer_multiplayer_room: OffsetGroup19,
    #[serde(rename = "osu.Game.Screens.Spectate.SpectatorScreen")]
    pub osu_game_screens_spectate_spectator_screen: OffsetGroup20,
    #[serde(rename = "osu.Game.Screens.OnlinePlay.Multiplayer.Spectate.MultiSpectatorScreen")]
    pub osu_game_screens_online_play_multiplayer_spectate_multi_spectator_screen: OffsetGroup21,
    #[serde(rename = "osu.Game.Online.Multiplayer.OnlineMultiplayerClient")]
    pub osu_game_online_multiplayer_online_multiplayer_client: OffsetGroup22,
    #[serde(rename = "osu.Game.Online.Multiplayer.MultiplayerClient")]
    pub osu_game_online_multiplayer_multiplayer_client: OffsetGroup23,
    #[serde(rename = "osu.Game.Overlays.ChatOverlay")]
    pub osu_game_overlays_chat_overlay: OffsetGroup24,
    #[serde(rename = "osu.Game.Screens.Menu.OsuLogo")]
    pub osu_game_screens_menu_osu_logo: OffsetGroup25,
    #[serde(rename = "osu.Game.Screens.Menu.LogoVisualisation")]
    pub osu_game_screens_menu_logo_visualisation: OffsetGroup26,
    #[serde(rename = "osu.Game.Screens.Play.Player")]
    pub osu_game_screens_play_player: OffsetGroup27,
    #[serde(default, rename = "osu.Game.Screens.Play.GameplayClockContainer")]
    pub osu_game_screens_play_gameplay_clock_container: Option<OffsetGroupGameplayClockContainer>,
    #[serde(rename = "osu.Framework.Screens.ScreenStack")]
    pub osu_framework_screens_screen_stack: OffsetGroup28,
    #[serde(rename = "osu.Game.Rulesets.RulesetConfigCache")]
    pub osu_game_rulesets_ruleset_config_cache: OffsetGroup29,
    #[serde(rename = "osu.Game.Online.Chat.ExternalLinkOpener")]
    pub osu_game_online_chat_external_link_opener: OffsetGroup30,
    #[serde(rename = "osu.Game.Online.API.APIAccess")]
    pub osu_game_online_api_apiaccess: OffsetGroup31,
    #[serde(rename = "osu.Game.Online.API.LocalUserState")]
    pub osu_game_online_api_local_user_state: OffsetGroup32,
    #[serde(rename = "osu.Desktop.OsuGameDesktop")]
    pub osu_desktop_osu_game_desktop: OffsetGroup33,
    #[serde(rename = "osu.Game.Scoring.ScoreInfo")]
    pub osu_game_scoring_score_info: OffsetGroup34,
    #[serde(rename = "osu.Framework.Platform.Storage")]
    pub osu_framework_platform_storage: OffsetGroup35,
    #[serde(rename = "osu.Game.IO.WrappedStorage")]
    pub osu_game_io_wrapped_storage: OffsetGroup36,
    #[serde(rename = "osu.Game.Beatmaps.WorkingBeatmapCache+BeatmapManagerWorkingBeatmap")]
    pub osu_game_beatmaps_working_beatmap_cache_beatmap_manager_working_beatmap: OffsetGroup37,
    #[serde(rename = "osu.Game.Models.RealmUser")]
    pub osu_game_models_realm_user: OffsetGroup38,
    #[serde(rename = "osu.Game.Rulesets.RulesetInfo")]
    pub osu_game_rulesets_ruleset_info: OffsetGroup39,
    #[serde(rename = "osu.Game.Screens.Play.SoloPlayer")]
    pub osu_game_screens_play_solo_player: OffsetGroup40,
    #[serde(rename = "osu.Game.Rulesets.Osu.Scoring.OsuScoreProcessor")]
    pub osu_game_rulesets_osu_scoring_osu_score_processor: OffsetGroup41,
    #[serde(rename = "osu.Game.Rulesets.Scoring.ScoreProcessor")]
    pub osu_game_rulesets_scoring_score_processor: OffsetGroup42,
    #[serde(rename = "osu.Game.Online.API.Requests.Responses.APIMe")]
    pub osu_game_online_api_requests_responses_apime: OffsetGroup43,
    #[serde(rename = "osu.Game.Online.API.Requests.Responses.APIUser")]
    pub osu_game_online_api_requests_responses_apiuser: OffsetGroup44,
    #[serde(rename = "osu.Game.Online.API.Requests.Responses.APIUserMatchmakingStatistics")]
    pub osu_game_online_api_requests_responses_apiuser_matchmaking_statistics: OffsetGroup45,
    #[serde(rename = "osu.Game.Online.Chat.ChannelManager")]
    pub osu_game_online_chat_channel_manager: OffsetGroup46,
    #[serde(rename = "osu.Game.Online.Chat.Channel")]
    pub osu_game_online_chat_channel: OffsetGroup47,
    #[serde(rename = "osu.Game.Online.Chat.Message")]
    pub osu_game_online_chat_message: OffsetGroup48,
    #[serde(rename = "osu.Game.Users.UserStatistics")]
    pub osu_game_users_user_statistics: OffsetGroup49,
    #[serde(rename = "osu.Game.Rulesets.Osu.Scoring.OsuHealthProcessor")]
    pub osu_game_rulesets_osu_scoring_osu_health_processor: OffsetGroup50,
    #[serde(rename = "osu.Game.Screens.Play.HUDOverlay")]
    pub osu_game_screens_play_hudoverlay: OffsetGroup51,
    #[serde(rename = "osu.Game.Screens.Play.HUD.InputCountController")]
    pub osu_game_screens_play_hud_input_count_controller: OffsetGroup52,
    #[serde(rename = "osu.Game.Rulesets.UI.DrawableRuleset")]
    pub osu_game_rulesets_ui_drawable_ruleset: OffsetGroup53,
    #[serde(rename = "osu.Game.Beatmaps.BeatmapInfo")]
    pub osu_game_beatmaps_beatmap_info: OffsetGroup54,
    #[serde(rename = "osu.Game.Beatmaps.BeatmapSetInfo")]
    pub osu_game_beatmaps_beatmap_set_info: OffsetGroup55,
    #[serde(rename = "osu.Game.Beatmaps.BeatmapMetadata")]
    pub osu_game_beatmaps_beatmap_metadata: OffsetGroup56,
    #[serde(rename = "osu.Game.Beatmaps.BeatmapDifficulty")]
    pub osu_game_beatmaps_beatmap_difficulty: OffsetGroup57,
    #[serde(rename = "osu.Game.Online.Multiplayer.MultiplayerRoomUser")]
    pub osu_game_online_multiplayer_multiplayer_room_user: OffsetGroup58,
    #[serde(rename = "osu.Game.Skinning.SkinManager")]
    pub osu_game_skinning_skin_manager: OffsetGroup59,
    #[serde(rename = "osu.Game.Skinning.Skin")]
    pub osu_game_skinning_skin: OffsetGroup60,
    #[serde(rename = "osu.Framework.Platform.GameHost")]
    pub osu_framework_platform_game_host: OffsetGroup61,
    #[serde(rename = "osu.Framework.Configuration.InputConfigManager")]
    pub osu_framework_configuration_input_config_manager: OffsetGroup62,
    #[serde(rename = "osu.Framework.Input.Handlers.Tablet.OpenTabletDriverHandler")]
    pub osu_framework_input_handlers_tablet_open_tablet_driver_handler: OffsetGroup63,
    #[serde(rename = "osu.Framework.Input.Handlers.Mouse.MouseHandler")]
    pub osu_framework_input_handlers_mouse_mouse_handler: OffsetGroup64,
    #[serde(rename = "osu.Game.Online.Multiplayer.MatchTypes.RankedPlay.RankedPlayRoomState")]
    pub osu_game_online_multiplayer_match_types_ranked_play_ranked_play_room_state: OffsetGroup65,
    #[serde(rename = "osu.Game.Online.Multiplayer.MatchTypes.RankedPlay.RankedPlayUserInfo")]
    pub osu_game_online_multiplayer_match_types_ranked_play_ranked_play_user_info: OffsetGroup66,
    #[serde(default, rename = "osu.Game.Rulesets.Catch.UI.DrawableCatchRuleset")]
    pub catch_ruleset: CatchRulesetOffsets,
    #[serde(default, rename = "osu.Game.Rulesets.Catch.UI.CatchPlayfield")]
    pub catch_playfield: CatchPlayfieldOffsets,
    #[serde(default, rename = "osu.Game.Rulesets.Catch.UI.Catcher")]
    pub catcher: CatcherOffsets,
    #[serde(default, rename = "osu.Framework.Graphics.Drawable")]
    pub drawable: DrawableOffsets,
    #[serde(rename = "osu.Game.Rulesets.Osu.Objects.OsuHitObject", default)]
    pub osu_hit_object: Option<OsuHitObjectOffsets>,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CatchRulesetOffsets {
    pub playfield: usize,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CatchPlayfieldOffsets {
    #[serde(rename = "<Catcher>k__BackingField")]
    pub catcher: usize,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CatcherOffsets {
    #[serde(rename = "<Dashing>k__BackingField")]
    pub dashing: usize,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DrawableOffsets {
    pub x: usize,
}

impl LazerOffsets {
    pub fn from_json(bytes: &[u8]) -> Result<Self, LazerOffsetsError> {
        let value: Self = serde_json::from_slice(bytes)?;
        value.validate()?;
        Ok(value)
    }
    pub fn embedded() -> Result<Self, LazerOffsetsError> {
        let asset = crate::assets::DaemonAssets::get("offsets.json")
            .ok_or(LazerOffsetsError::MissingEmbedded)?;
        Self::from_json(asset.data.as_ref())
    }
    pub fn effective_game_base_vtable(&self) -> usize {
        self.game_base_vtable.unwrap_or(FALLBACK_GAME_BASE_VTABLE)
    }
    pub fn validate(&self) -> Result<(), LazerOffsetsError> {
        validate_version(&self.osu_version)?;
        if self.game_base_vtable == Some(0) {
            return Err(LazerOffsetsError::Zero("GameBaseVtable"));
        }
        self.osu_game_osu_game.validate()?;
        self.osu_framework_game.validate()?;
        self.osu_game_osu_game_base.validate()?;
        self.osu_game_screens_select_song_select.validate()?;
        if let Some(group) = &self.osu_game_screens_select_solo_song_select {
            group.validate()?;
        }
        if let Some(group) = &self.osu_game_screens_select_v2_solo_song_select {
            group.validate()?;
        }
        self.osu_game_screens_play_submitting_player.validate()?;
        self.osu_game_screens_play_player_loader.validate()?;
        self.osu_game_screens_play_spectator_player.validate()?;
        self.osu_game_beatmaps_framed_beatmap_clock.validate()?;
        self.osu_framework_timing_decoupling_framed_clock
            .validate()?;
        self.osu_framework_audio_track_track.validate()?;
        self.osu_framework_timing_framed_clock.validate()?;
        self.osu_game_screens_osu_screen.validate()?;
        self.osu_game_screens_ranking_solo_results_screen
            .validate()?;
        self.osu_game_screens_edit_editor.validate()?;
        self.osu_game_screens_online_play_online_play_screen
            .validate()?;
        self.osu_game_screens_online_play_multiplayer_multiplayer
            .validate()?;
        self.osu_game_online_multiplayer_multiplayer_room
            .validate()?;
        self.osu_game_screens_spectate_spectator_screen.validate()?;
        self.osu_game_screens_online_play_multiplayer_spectate_multi_spectator_screen
            .validate()?;
        self.osu_game_online_multiplayer_online_multiplayer_client
            .validate()?;
        self.osu_game_online_multiplayer_multiplayer_client
            .validate()?;
        self.osu_game_overlays_chat_overlay.validate()?;
        self.osu_game_screens_menu_osu_logo.validate()?;
        self.osu_game_screens_menu_logo_visualisation.validate()?;
        self.osu_game_screens_play_player.validate()?;
        if let Some(group) = &self.osu_game_screens_play_gameplay_clock_container {
            group.validate()?;
        }
        self.osu_framework_screens_screen_stack.validate()?;
        self.osu_game_rulesets_ruleset_config_cache.validate()?;
        self.osu_game_online_chat_external_link_opener.validate()?;
        self.osu_game_online_api_apiaccess.validate()?;
        self.osu_game_online_api_local_user_state.validate()?;
        self.osu_desktop_osu_game_desktop.validate()?;
        self.osu_game_scoring_score_info.validate()?;
        self.osu_framework_platform_storage.validate()?;
        self.osu_game_io_wrapped_storage.validate()?;
        self.osu_game_beatmaps_working_beatmap_cache_beatmap_manager_working_beatmap
            .validate()?;
        self.osu_game_models_realm_user.validate()?;
        self.osu_game_rulesets_ruleset_info.validate()?;
        self.osu_game_screens_play_solo_player.validate()?;
        self.osu_game_rulesets_osu_scoring_osu_score_processor
            .validate()?;
        self.osu_game_rulesets_scoring_score_processor.validate()?;
        self.osu_game_online_api_requests_responses_apime
            .validate()?;
        self.osu_game_online_api_requests_responses_apiuser
            .validate()?;
        self.osu_game_online_api_requests_responses_apiuser_matchmaking_statistics
            .validate()?;
        self.osu_game_online_chat_channel_manager.validate()?;
        self.osu_game_online_chat_channel.validate()?;
        self.osu_game_online_chat_message.validate()?;
        self.osu_game_users_user_statistics.validate()?;
        self.osu_game_rulesets_osu_scoring_osu_health_processor
            .validate()?;
        self.osu_game_screens_play_hudoverlay.validate()?;
        self.osu_game_screens_play_hud_input_count_controller
            .validate()?;
        self.osu_game_rulesets_ui_drawable_ruleset.validate()?;
        self.osu_game_beatmaps_beatmap_info.validate()?;
        self.osu_game_beatmaps_beatmap_set_info.validate()?;
        self.osu_game_beatmaps_beatmap_metadata.validate()?;
        self.osu_game_beatmaps_beatmap_difficulty.validate()?;
        self.osu_game_online_multiplayer_multiplayer_room_user
            .validate()?;
        self.osu_game_skinning_skin_manager.validate()?;
        self.osu_game_skinning_skin.validate()?;
        self.osu_framework_platform_game_host.validate()?;
        self.osu_framework_configuration_input_config_manager
            .validate()?;
        self.osu_framework_input_handlers_tablet_open_tablet_driver_handler
            .validate()?;
        self.osu_framework_input_handlers_mouse_mouse_handler
            .validate()?;
        self.osu_game_online_multiplayer_match_types_ranked_play_ranked_play_room_state
            .validate()?;
        self.osu_game_online_multiplayer_match_types_ranked_play_ranked_play_user_info
            .validate()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
