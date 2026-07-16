use super::LazerOffsetsError;
use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup21 {
    #[serde(rename = "<multiplayerClient>k__BackingField")]
    pub multiplayer_client_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup21 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.multiplayer_client_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.OnlinePlay.Multiplayer.Spectate.MultiSpectatorScreen.<multiplayerClient>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup22 {
    #[serde(rename = "<IsConnected>k__BackingField")]
    pub is_connected_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup22 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.is_connected_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.OnlineMultiplayerClient.<IsConnected>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup23 {
    #[serde(rename = "room")]
    pub room: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup23 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.room == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MultiplayerClient.room",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup24 {
    #[serde(rename = "State")]
    pub state: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup24 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.state == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Overlays.ChatOverlay.State",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup25 {
    #[serde(rename = "visualizer")]
    pub visualizer: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup25 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.visualizer == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Menu.OsuLogo.visualizer",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup26 {
    #[serde(rename = "frequencyAmplitudes")]
    pub frequency_amplitudes: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup26 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.frequency_amplitudes == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Menu.LogoVisualisation.frequencyAmplitudes",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup27 {
    #[serde(rename = "<api>k__BackingField")]
    pub api_k_backing_field: usize,
    #[serde(rename = "<scoreManager>k__BackingField")]
    pub score_manager_k_backing_field: usize,
    #[serde(rename = "<Score>k__BackingField")]
    pub score_k_backing_field: usize,
    #[serde(rename = "<ScoreProcessor>k__BackingField")]
    pub score_processor_k_backing_field: usize,
    #[serde(rename = "<HealthProcessor>k__BackingField")]
    pub health_processor_k_backing_field: usize,
    #[serde(rename = "<HUDOverlay>k__BackingField")]
    pub hudoverlay_k_backing_field: usize,
    #[serde(rename = "<DrawableRuleset>k__BackingField")]
    pub drawable_ruleset_k_backing_field: usize,
    #[serde(rename = "dependencies")]
    pub dependencies: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup27 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.api_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Play.Player.<api>k__BackingField",
            ));
        }
        if self.score_manager_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Play.Player.<scoreManager>k__BackingField",
            ));
        }
        if self.score_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Play.Player.<Score>k__BackingField",
            ));
        }
        if self.score_processor_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Play.Player.<ScoreProcessor>k__BackingField",
            ));
        }
        if self.health_processor_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Play.Player.<HealthProcessor>k__BackingField",
            ));
        }
        if self.hudoverlay_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Play.Player.<HUDOverlay>k__BackingField",
            ));
        }
        if self.drawable_ruleset_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Play.Player.<DrawableRuleset>k__BackingField",
            ));
        }
        if self.dependencies == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Screens.Play.Player.dependencies",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup28 {
    #[serde(rename = "stack")]
    pub stack: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup28 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.stack == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Framework.Screens.ScreenStack.stack",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup29 {
    #[serde(rename = "configCache")]
    pub config_cache: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup29 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.config_cache == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Rulesets.RulesetConfigCache.configCache",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup30 {
    #[serde(rename = "<api>k__BackingField")]
    pub api_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup30 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.api_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Chat.ExternalLinkOpener.<api>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup31 {
    #[serde(rename = "localUserState")]
    pub local_user_state: usize,
    #[serde(rename = "game")]
    pub game: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup31 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.local_user_state == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.API.APIAccess.localUserState",
            ));
        }
        if self.game == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.API.APIAccess.game",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup32 {
    #[serde(rename = "localUser")]
    pub local_user: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup32 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.local_user == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.API.LocalUserState.localUser",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup33 {
    #[serde(rename = "AvailableMods")]
    pub available_mods: usize,
    #[serde(rename = "SelectedMods")]
    pub selected_mods: usize,
    #[serde(rename = "Ruleset")]
    pub ruleset: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup33 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.available_mods == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Desktop.OsuGameDesktop.AvailableMods",
            ));
        }
        if self.selected_mods == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Desktop.OsuGameDesktop.SelectedMods",
            ));
        }
        if self.ruleset == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Desktop.OsuGameDesktop.Ruleset",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup34 {
    #[serde(rename = "<OnlineID>k__BackingField")]
    pub online_id_k_backing_field: usize,
    #[serde(rename = "<LegacyOnlineID>k__BackingField")]
    pub legacy_online_id_k_backing_field: usize,
    #[serde(rename = "<ModsJson>k__BackingField")]
    pub mods_json_k_backing_field: usize,
    #[serde(rename = "<HitEvents>k__BackingField")]
    pub hit_events_k_backing_field: usize,
    #[serde(rename = "<RealmUser>k__BackingField")]
    pub realm_user_k_backing_field: usize,
    #[serde(rename = "<TotalScore>k__BackingField")]
    pub total_score_k_backing_field: usize,
    #[serde(rename = "<MaxCombo>k__BackingField")]
    pub max_combo_k_backing_field: usize,
    #[serde(rename = "<Combo>k__BackingField")]
    pub combo_k_backing_field: usize,
    #[serde(rename = "<Passed>k__BackingField")]
    pub passed_k_backing_field: usize,
    #[serde(rename = "<Ruleset>k__BackingField")]
    pub ruleset_k_backing_field: usize,
    #[serde(rename = "<Accuracy>k__BackingField")]
    pub accuracy_k_backing_field: usize,
    #[serde(rename = "<Date>k__BackingField")]
    pub date_k_backing_field: usize,
    #[serde(rename = "statistics")]
    pub statistics: usize,
    #[serde(rename = "maximumStatistics")]
    pub maximum_statistics: usize,
    #[serde(rename = "user")]
    pub user: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup34 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.online_id_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Scoring.ScoreInfo.<OnlineID>k__BackingField",
            ));
        }
        if self.legacy_online_id_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Scoring.ScoreInfo.<LegacyOnlineID>k__BackingField",
            ));
        }
        if self.mods_json_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Scoring.ScoreInfo.<ModsJson>k__BackingField",
            ));
        }
        if self.hit_events_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Scoring.ScoreInfo.<HitEvents>k__BackingField",
            ));
        }
        if self.realm_user_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Scoring.ScoreInfo.<RealmUser>k__BackingField",
            ));
        }
        if self.total_score_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Scoring.ScoreInfo.<TotalScore>k__BackingField",
            ));
        }
        if self.max_combo_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Scoring.ScoreInfo.<MaxCombo>k__BackingField",
            ));
        }
        if self.combo_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Scoring.ScoreInfo.<Combo>k__BackingField",
            ));
        }
        if self.passed_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Scoring.ScoreInfo.<Passed>k__BackingField",
            ));
        }
        if self.ruleset_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Scoring.ScoreInfo.<Ruleset>k__BackingField",
            ));
        }
        if self.accuracy_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Scoring.ScoreInfo.<Accuracy>k__BackingField",
            ));
        }
        if self.date_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Scoring.ScoreInfo.<Date>k__BackingField",
            ));
        }
        if self.statistics == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Scoring.ScoreInfo.statistics",
            ));
        }
        if self.maximum_statistics == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Scoring.ScoreInfo.maximumStatistics",
            ));
        }
        if self.user == 0 {
            return Err(LazerOffsetsError::Zero("osu.Game.Scoring.ScoreInfo.user"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup35 {
    #[serde(rename = "<BasePath>k__BackingField")]
    pub base_path_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup35 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.base_path_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Framework.Platform.Storage.<BasePath>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup36 {
    #[serde(rename = "<UnderlyingStorage>k__BackingField")]
    pub underlying_storage_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup36 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.underlying_storage_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.IO.WrappedStorage.<UnderlyingStorage>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup37 {
    #[serde(rename = "BeatmapInfo")]
    pub beatmap_info: usize,
    #[serde(rename = "BeatmapSetInfo")]
    pub beatmap_set_info: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup37 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.beatmap_info == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.WorkingBeatmapCache+BeatmapManagerWorkingBeatmap.BeatmapInfo",
            ));
        }
        if self.beatmap_set_info == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Beatmaps.WorkingBeatmapCache+BeatmapManagerWorkingBeatmap.BeatmapSetInfo",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup38 {
    #[serde(rename = "<Username>k__BackingField")]
    pub username_k_backing_field: usize,
    #[serde(rename = "<OnlineID>k__BackingField")]
    pub online_id_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup38 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.username_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Models.RealmUser.<Username>k__BackingField",
            ));
        }
        if self.online_id_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Models.RealmUser.<OnlineID>k__BackingField",
            ));
        }
        Ok(())
    }
}
