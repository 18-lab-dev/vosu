use super::LazerOffsetsError;
use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup39 {
    #[serde(rename = "<OnlineID>k__BackingField")]
    pub online_id_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup39 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.online_id_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Rulesets.RulesetInfo.<OnlineID>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup40 {
    #[serde(rename = "RestartCount")]
    pub restart_count: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup40 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.restart_count == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Play.SoloPlayer.RestartCount",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup41 {
    #[serde(rename = "Combo")]
    pub combo: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup41 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.combo == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Rulesets.Osu.Scoring.OsuScoreProcessor.Combo",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup42 {
    #[serde(rename = "hitEvents")]
    pub hit_events: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup42 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.hit_events == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Rulesets.Scoring.ScoreProcessor.hitEvents",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup43 {
    #[serde(rename = "PlayMode")]
    pub play_mode: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup43 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.play_mode == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.API.Requests.Responses.APIMe.PlayMode",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup44 {
    #[serde(rename = "<Id>k__BackingField")]
    pub id_k_backing_field: usize,
    #[serde(rename = "<Username>k__BackingField")]
    pub username_k_backing_field: usize,
    #[serde(rename = "countryCodeString")]
    pub country_code_string: usize,
    pub statistics: usize,
    #[serde(rename = "MatchmakingStatistics")]
    pub matchmaking_statistics: usize,
    #[serde(rename = "AvatarUrl", default)]
    pub avatar_url: Option<usize>,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup44 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.id_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.API.Requests.Responses.APIUser.<Id>k__BackingField",
            ));
        }
        if self.username_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.API.Requests.Responses.APIUser.<Username>k__BackingField",
            ));
        }
        if self.country_code_string == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.API.Requests.Responses.APIUser.countryCodeString",
            ));
        }
        if self.statistics == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.API.Requests.Responses.APIUser.statistics",
            ));
        }
        if self.matchmaking_statistics == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.API.Requests.Responses.APIUser.MatchmakingStatistics",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup45 {
    #[serde(rename = "<Rating>k__BackingField")]
    pub rating_k_backing_field: usize,
    #[serde(rename = "<Rank>k__BackingField")]
    pub rank_k_backing_field: usize,
    #[serde(rename = "<Plays>k__BackingField")]
    pub plays_k_backing_field: usize,
    #[serde(rename = "<FirstPlacements>k__BackingField")]
    pub first_placements_k_backing_field: usize,
    #[serde(rename = "<IsRatingProvisional>k__BackingField")]
    pub is_rating_provisional_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup45 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.rating_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.API.Requests.Responses.APIUserMatchmakingStatistics.<Rating>k__BackingField",
            ));
        }
        if self.rank_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.API.Requests.Responses.APIUserMatchmakingStatistics.<Rank>k__BackingField",
            ));
        }
        if self.plays_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.API.Requests.Responses.APIUserMatchmakingStatistics.<Plays>k__BackingField",
            ));
        }
        if self.first_placements_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.API.Requests.Responses.APIUserMatchmakingStatistics.<FirstPlacements>k__BackingField",
            ));
        }
        if self.is_rating_provisional_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.API.Requests.Responses.APIUserMatchmakingStatistics.<IsRatingProvisional>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup46 {
    #[serde(rename = "joinedChannels")]
    pub joined_channels: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup46 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.joined_channels == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Chat.ChannelManager.joinedChannels",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup47 {
    #[serde(rename = "Id")]
    pub id: usize,
    #[serde(rename = "Messages")]
    pub messages: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup47 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.id == 0 {
            return Err(LazerOffsetsError::Zero("osu.Game.Online.Chat.Channel.Id"));
        }
        if self.messages == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Chat.Channel.Messages",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup48 {
    #[serde(rename = "Timestamp")]
    pub timestamp: usize,
    #[serde(rename = "Content")]
    pub content: usize,
    #[serde(rename = "Sender")]
    pub sender: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup48 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.timestamp == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Chat.Message.Timestamp",
            ));
        }
        if self.content == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Chat.Message.Content",
            ));
        }
        if self.sender == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Chat.Message.Sender",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup49 {
    #[serde(rename = "RankedScore")]
    pub ranked_score: usize,
    #[serde(rename = "GlobalRank")]
    pub global_rank: usize,
    #[serde(rename = "PlayCount")]
    pub play_count: usize,
    #[serde(rename = "Accuracy")]
    pub accuracy: usize,
    #[serde(rename = "Level")]
    pub level: usize,
    #[serde(rename = "PP")]
    pub pp: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup49 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.ranked_score == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Users.UserStatistics.RankedScore",
            ));
        }
        if self.global_rank == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Users.UserStatistics.GlobalRank",
            ));
        }
        if self.play_count == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Users.UserStatistics.PlayCount",
            ));
        }
        if self.accuracy == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Users.UserStatistics.Accuracy",
            ));
        }
        if self.level == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Users.UserStatistics.Level",
            ));
        }
        if self.pp == 0 {
            return Err(LazerOffsetsError::Zero("osu.Game.Users.UserStatistics.PP"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup50 {
    #[serde(rename = "Health")]
    pub health: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup50 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.health == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Rulesets.Osu.Scoring.OsuHealthProcessor.Health",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup51 {
    #[serde(rename = "InputCountController")]
    pub input_count_controller: usize,
    #[serde(rename = "<ShowHud>k__BackingField")]
    pub show_hud_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup51 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.input_count_controller == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Play.HUDOverlay.InputCountController",
            ));
        }
        if self.show_hud_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Play.HUDOverlay.<ShowHud>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup52 {
    #[serde(rename = "triggers")]
    pub triggers: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup52 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.triggers == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Play.HUD.InputCountController.triggers",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup53 {
    #[serde(rename = "<ReplayScore>k__BackingField")]
    pub replay_score_k_backing_field: usize,
    #[serde(default, rename = "IsPaused")]
    pub is_paused: Option<usize>,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup53 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.replay_score_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Rulesets.UI.DrawableRuleset.<ReplayScore>k__BackingField",
            ));
        }
        if self.is_paused == Some(0) {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Rulesets.UI.DrawableRuleset.IsPaused",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup54 {
    #[serde(rename = "<OnlineID>k__BackingField")]
    pub online_id_k_backing_field: usize,
    #[serde(rename = "<MD5Hash>k__BackingField")]
    pub md5_hash_k_backing_field: usize,
    #[serde(rename = "<StatusInt>k__BackingField")]
    pub status_int_k_backing_field: usize,
    #[serde(rename = "<Metadata>k__BackingField")]
    pub metadata_k_backing_field: usize,
    #[serde(rename = "<Difficulty>k__BackingField")]
    pub difficulty_k_backing_field: usize,
    #[serde(rename = "<DifficultyName>k__BackingField")]
    pub difficulty_name_k_backing_field: usize,
    #[serde(rename = "<TotalObjectCount>k__BackingField")]
    pub total_object_count_k_backing_field: usize,
    #[serde(rename = "<Hash>k__BackingField")]
    pub hash_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup54 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.online_id_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapInfo.<OnlineID>k__BackingField",
            ));
        }
        if self.md5_hash_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapInfo.<MD5Hash>k__BackingField",
            ));
        }
        if self.status_int_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapInfo.<StatusInt>k__BackingField",
            ));
        }
        if self.metadata_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapInfo.<Metadata>k__BackingField",
            ));
        }
        if self.difficulty_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapInfo.<Difficulty>k__BackingField",
            ));
        }
        if self.difficulty_name_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapInfo.<DifficultyName>k__BackingField",
            ));
        }
        if self.total_object_count_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapInfo.<TotalObjectCount>k__BackingField",
            ));
        }
        if self.hash_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapInfo.<Hash>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup55 {
    #[serde(rename = "<OnlineID>k__BackingField")]
    pub online_id_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup55 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.online_id_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapSetInfo.<OnlineID>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup56 {
    #[serde(rename = "<Title>k__BackingField")]
    pub title_k_backing_field: usize,
    #[serde(rename = "<TitleUnicode>k__BackingField")]
    pub title_unicode_k_backing_field: usize,
    #[serde(rename = "<Artist>k__BackingField")]
    pub artist_k_backing_field: usize,
    #[serde(rename = "<ArtistUnicode>k__BackingField")]
    pub artist_unicode_k_backing_field: usize,
    #[serde(rename = "<Author>k__BackingField")]
    pub author_k_backing_field: usize,
    #[serde(rename = "<Source>k__BackingField")]
    pub source_k_backing_field: usize,
    #[serde(rename = "<Tags>k__BackingField")]
    pub tags_k_backing_field: usize,
    #[serde(rename = "<UserTags>k__BackingField")]
    pub user_tags_k_backing_field: usize,
    #[serde(rename = "<PreviewTime>k__BackingField")]
    pub preview_time_k_backing_field: usize,
    #[serde(rename = "<AudioFile>k__BackingField")]
    pub audio_file_k_backing_field: usize,
    #[serde(rename = "<BackgroundFile>k__BackingField")]
    pub background_file_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup56 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.title_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapMetadata.<Title>k__BackingField",
            ));
        }
        if self.title_unicode_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapMetadata.<TitleUnicode>k__BackingField",
            ));
        }
        if self.artist_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapMetadata.<Artist>k__BackingField",
            ));
        }
        if self.artist_unicode_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapMetadata.<ArtistUnicode>k__BackingField",
            ));
        }
        if self.author_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapMetadata.<Author>k__BackingField",
            ));
        }
        if self.source_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapMetadata.<Source>k__BackingField",
            ));
        }
        if self.tags_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapMetadata.<Tags>k__BackingField",
            ));
        }
        if self.user_tags_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapMetadata.<UserTags>k__BackingField",
            ));
        }
        if self.preview_time_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapMetadata.<PreviewTime>k__BackingField",
            ));
        }
        if self.audio_file_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapMetadata.<AudioFile>k__BackingField",
            ));
        }
        if self.background_file_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapMetadata.<BackgroundFile>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup57 {
    #[serde(rename = "<DrainRate>k__BackingField")]
    pub drain_rate_k_backing_field: usize,
    #[serde(rename = "<CircleSize>k__BackingField")]
    pub circle_size_k_backing_field: usize,
    #[serde(rename = "<OverallDifficulty>k__BackingField")]
    pub overall_difficulty_k_backing_field: usize,
    #[serde(rename = "<ApproachRate>k__BackingField")]
    pub approach_rate_k_backing_field: usize,
    #[serde(rename = "<SliderMultiplier>k__BackingField")]
    pub slider_multiplier_k_backing_field: usize,
    #[serde(rename = "<SliderTickRate>k__BackingField")]
    pub slider_tick_rate_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup57 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.drain_rate_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapDifficulty.<DrainRate>k__BackingField",
            ));
        }
        if self.circle_size_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapDifficulty.<CircleSize>k__BackingField",
            ));
        }
        if self.overall_difficulty_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapDifficulty.<OverallDifficulty>k__BackingField",
            ));
        }
        if self.approach_rate_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapDifficulty.<ApproachRate>k__BackingField",
            ));
        }
        if self.slider_multiplier_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapDifficulty.<SliderMultiplier>k__BackingField",
            ));
        }
        if self.slider_tick_rate_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.BeatmapDifficulty.<SliderTickRate>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup58 {
    #[serde(rename = "UserID")]
    pub user_id: usize,
    #[serde(rename = "<State>k__BackingField")]
    pub state_k_backing_field: usize,
    #[serde(rename = "<BeatmapAvailability>k__BackingField")]
    pub beatmap_availability_k_backing_field: usize,
    #[serde(rename = "<Mods>k__BackingField")]
    pub mods_k_backing_field: usize,
    #[serde(rename = "<MatchState>k__BackingField")]
    pub match_state_k_backing_field: usize,
    #[serde(rename = "RulesetId")]
    pub ruleset_id: usize,
    #[serde(rename = "BeatmapId")]
    pub beatmap_id: usize,
    #[serde(rename = "<User>k__BackingField")]
    pub user_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup58 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.user_id == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MultiplayerRoomUser.UserID",
            ));
        }
        if self.state_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MultiplayerRoomUser.<State>k__BackingField",
            ));
        }
        if self.beatmap_availability_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MultiplayerRoomUser.<BeatmapAvailability>k__BackingField",
            ));
        }
        if self.mods_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MultiplayerRoomUser.<Mods>k__BackingField",
            ));
        }
        if self.match_state_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MultiplayerRoomUser.<MatchState>k__BackingField",
            ));
        }
        if self.ruleset_id == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MultiplayerRoomUser.RulesetId",
            ));
        }
        if self.beatmap_id == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MultiplayerRoomUser.BeatmapId",
            ));
        }
        if self.user_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MultiplayerRoomUser.<User>k__BackingField",
            ));
        }
        Ok(())
    }
}
