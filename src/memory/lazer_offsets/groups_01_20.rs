use super::LazerOffsetsError;
use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup1 {
    #[serde(rename = "osuLogo")]
    pub osu_logo: usize,
    #[serde(rename = "ScreenStack")]
    pub screen_stack: Option<usize>,
    #[serde(rename = "<ScreenStack>k__BackingField")]
    pub screen_stack_k_backing_field: usize,
    #[serde(rename = "channelManager")]
    pub channel_manager: usize,
    #[serde(rename = "<frameworkConfig>k__BackingField")]
    pub framework_config_k_backing_field: usize,
    #[serde(rename = "chatOverlay")]
    pub chat_overlay: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup1 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.osu_logo == 0 {
            return Err(LazerOffsetsError::Zero("osu.Game.OsuGame.osuLogo"));
        }
        if self.screen_stack == Some(0) {
            return Err(LazerOffsetsError::Zero("osu.Game.OsuGame.ScreenStack"));
        }
        if self.screen_stack_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.OsuGame.<ScreenStack>k__BackingField",
            ));
        }
        if self.channel_manager == 0 {
            return Err(LazerOffsetsError::Zero("osu.Game.OsuGame.channelManager"));
        }
        if self.framework_config_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.OsuGame.<frameworkConfig>k__BackingField",
            ));
        }
        if self.chat_overlay == 0 {
            return Err(LazerOffsetsError::Zero("osu.Game.OsuGame.chatOverlay"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup2 {
    #[serde(rename = "<Host>k__BackingField")]
    pub host_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup2 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.host_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Framework.Game.<Host>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup3 {
    #[serde(rename = "<API>k__BackingField")]
    pub api_k_backing_field: usize,
    #[serde(rename = "<SpectatorClient>k__BackingField")]
    pub spectator_client_k_backing_field: usize,
    #[serde(rename = "<ScoreManager>k__BackingField")]
    pub score_manager_k_backing_field: usize,
    #[serde(rename = "<MultiplayerClient>k__BackingField")]
    pub multiplayer_client_k_backing_field: usize,
    #[serde(rename = "<LeaderboardManager>k__BackingField")]
    pub leaderboard_manager_k_backing_field: usize,
    #[serde(rename = "<SessionStatics>k__BackingField")]
    pub session_statics_k_backing_field: usize,
    #[serde(rename = "beatmapClock")]
    pub beatmap_clock: usize,
    #[serde(rename = "<Storage>k__BackingField")]
    pub storage_k_backing_field: usize,
    #[serde(rename = "<Beatmap>k__BackingField")]
    pub beatmap_k_backing_field: usize,
    #[serde(rename = "<SkinManager>k__BackingField")]
    pub skin_manager_k_backing_field: usize,
    #[serde(rename = "<VersionHash>k__BackingField")]
    pub version_hash_k_backing_field: usize,
    #[serde(rename = "<LocalConfig>k__BackingField")]
    pub local_config_k_backing_field: usize,
    #[serde(rename = "rulesetConfigCache")]
    pub ruleset_config_cache: usize,
    #[serde(rename = "realm")]
    pub realm: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup3 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.api_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.OsuGameBase.<API>k__BackingField",
            ));
        }
        if self.spectator_client_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.OsuGameBase.<SpectatorClient>k__BackingField",
            ));
        }
        if self.score_manager_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.OsuGameBase.<ScoreManager>k__BackingField",
            ));
        }
        if self.multiplayer_client_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.OsuGameBase.<MultiplayerClient>k__BackingField",
            ));
        }
        if self.leaderboard_manager_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.OsuGameBase.<LeaderboardManager>k__BackingField",
            ));
        }
        if self.session_statics_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.OsuGameBase.<SessionStatics>k__BackingField",
            ));
        }
        if self.beatmap_clock == 0 {
            return Err(LazerOffsetsError::Zero("osu.Game.OsuGameBase.beatmapClock"));
        }
        if self.storage_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.OsuGameBase.<Storage>k__BackingField",
            ));
        }
        if self.beatmap_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.OsuGameBase.<Beatmap>k__BackingField",
            ));
        }
        if self.skin_manager_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.OsuGameBase.<SkinManager>k__BackingField",
            ));
        }
        if self.version_hash_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.OsuGameBase.<VersionHash>k__BackingField",
            ));
        }
        if self.local_config_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.OsuGameBase.<LocalConfig>k__BackingField",
            ));
        }
        if self.ruleset_config_cache == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.OsuGameBase.rulesetConfigCache",
            ));
        }
        if self.realm == 0 {
            return Err(LazerOffsetsError::Zero("osu.Game.OsuGameBase.realm"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup4 {
    #[serde(rename = "<game>k__BackingField")]
    pub game_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup4 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.game_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Select.SongSelect.<game>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup5 {
    #[serde(rename = "<game>k__BackingField")]
    pub game_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup5 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.game_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Select.SoloSongSelect.<game>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup6 {
    #[serde(rename = "<game>k__BackingField")]
    pub game_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup6 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.game_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.SelectV2.SoloSongSelect.<game>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup7 {
    #[serde(rename = "<api>k__BackingField")]
    pub api_k_backing_field: usize,
    #[serde(rename = "<spectatorClient>k__BackingField")]
    pub spectator_client_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup7 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.api_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Play.SubmittingPlayer.<api>k__BackingField",
            ));
        }
        if self.spectator_client_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Play.SubmittingPlayer.<spectatorClient>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup8 {
    #[serde(rename = "osuLogo")]
    pub osu_logo: usize,
    #[serde(rename = "<leaderboardManager>k__BackingField")]
    pub leaderboard_manager_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup8 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.osu_logo == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Play.PlayerLoader.osuLogo",
            ));
        }
        if self.leaderboard_manager_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Play.PlayerLoader.<leaderboardManager>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup9 {
    #[serde(rename = "score")]
    pub score: usize,
    #[serde(rename = "<SpectatorClient>k__BackingField")]
    pub spectator_client_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup9 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.score == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Play.SpectatorPlayer.score",
            ));
        }
        if self.spectator_client_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Play.SpectatorPlayer.<SpectatorClient>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup10 {
    #[serde(rename = "finalClockSource")]
    pub final_clock_source: usize,
    #[serde(rename = "decoupledTrack")]
    pub decoupled_track: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup10 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.final_clock_source == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.FramedBeatmapClock.finalClockSource",
            ));
        }
        if self.decoupled_track == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.FramedBeatmapClock.decoupledTrack",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup11 {
    #[serde(rename = "<Source>k__BackingField")]
    pub source_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup11 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.source_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Framework.Timing.DecouplingFramedClock.<Source>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup12 {
    #[serde(rename = "length")]
    pub length: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup12 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.length == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Framework.Audio.Track.Track.length",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup13 {
    #[serde(rename = "<CurrentTime>k__BackingField")]
    pub current_time_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup13 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.current_time_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Framework.Timing.FramedClock.<CurrentTime>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup14 {
    #[serde(rename = "<logo>k__BackingField")]
    pub logo_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup14 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.logo_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.OsuScreen.<logo>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup15 {
    #[serde(rename = "<api>k__BackingField")]
    pub api_k_backing_field: usize,
    #[serde(rename = "SelectedScore")]
    pub selected_score: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup15 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.api_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Ranking.SoloResultsScreen.<api>k__BackingField",
            ));
        }
        if self.selected_score == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Ranking.SoloResultsScreen.SelectedScore",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup16 {
    #[serde(rename = "<realm>k__BackingField")]
    pub realm_k_backing_field: usize,
    #[serde(rename = "<api>k__BackingField")]
    pub api_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup16 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.realm_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Edit.Editor.<realm>k__BackingField",
            ));
        }
        if self.api_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Edit.Editor.<api>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup17 {
    #[serde(rename = "<API>k__BackingField")]
    pub api_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup17 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.api_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.OnlinePlay.OnlinePlayScreen.<API>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup18 {
    #[serde(rename = "<client>k__BackingField")]
    pub client_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup18 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.client_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.OnlinePlay.Multiplayer.Multiplayer.<client>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup19 {
    #[serde(rename = "RoomID")]
    pub room_id: usize,
    #[serde(rename = "<ChannelID>k__BackingField")]
    pub channel_id_k_backing_field: usize,
    #[serde(rename = "<MatchState>k__BackingField")]
    pub match_state_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup19 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.room_id == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MultiplayerRoom.RoomID",
            ));
        }
        if self.channel_id_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MultiplayerRoom.<ChannelID>k__BackingField",
            ));
        }
        if self.match_state_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MultiplayerRoom.<MatchState>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup20 {
    #[serde(rename = "<spectatorClient>k__BackingField")]
    pub spectator_client_k_backing_field: usize,
    #[serde(rename = "gameplayStates")]
    pub gameplay_states: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup20 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.spectator_client_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Spectate.SpectatorScreen.<spectatorClient>k__BackingField",
            ));
        }
        if self.gameplay_states == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Spectate.SpectatorScreen.gameplayStates",
            ));
        }
        Ok(())
    }
}
