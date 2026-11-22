use super::LazerOffsetsError;
use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup59 {
    #[serde(rename = "CurrentSkin")]
    pub current_skin: usize,
    #[serde(default, rename = "<DefaultClassicSkin>k__BackingField")]
    pub legacy_skin: usize,
    #[serde(default, rename = "<argonSkin>k__BackingField")]
    pub argon_skin: usize,
    #[serde(default, rename = "<trianglesSkin>k__BackingField")]
    pub triangles_skin: usize,
    #[serde(default, rename = "<retroSkin>k__BackingField")]
    pub retro_skin: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup59 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.current_skin == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Skinning.SkinManager.CurrentSkin",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup60 {
    #[serde(rename = "<Name>k__BackingField")]
    pub name_k_backing_field: usize,
    #[serde(default)]
    pub store: usize,
    #[serde(default, rename = "SkinInfo")]
    pub skin_info: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup60 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.name_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Skinning.Skin.<Name>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup61 {
    #[serde(rename = "<inputConfig>k__BackingField")]
    pub input_config_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup61 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.input_config_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Framework.Platform.GameHost.<inputConfig>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup62 {
    #[serde(rename = "<InputHandlers>k__BackingField")]
    pub input_handlers_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup62 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.input_handlers_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Framework.Configuration.InputConfigManager.<InputHandlers>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup63 {
    #[serde(rename = "<AreaOffset>k__BackingField")]
    pub area_offset_k_backing_field: usize,
    #[serde(rename = "<AreaSize>k__BackingField")]
    pub area_size_k_backing_field: usize,
    #[serde(rename = "<Rotation>k__BackingField")]
    pub rotation_k_backing_field: usize,
    #[serde(rename = "<PressureThreshold>k__BackingField")]
    pub pressure_threshold_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup63 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.area_offset_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Framework.Input.Handlers.Tablet.OpenTabletDriverHandler.<AreaOffset>k__BackingField",
            ));
        }
        if self.area_size_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Framework.Input.Handlers.Tablet.OpenTabletDriverHandler.<AreaSize>k__BackingField",
            ));
        }
        if self.rotation_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Framework.Input.Handlers.Tablet.OpenTabletDriverHandler.<Rotation>k__BackingField",
            ));
        }
        if self.pressure_threshold_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Framework.Input.Handlers.Tablet.OpenTabletDriverHandler.<PressureThreshold>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup64 {
    #[serde(rename = "<UseRelativeMode>k__BackingField")]
    pub use_relative_mode_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup64 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.use_relative_mode_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Framework.Input.Handlers.Mouse.MouseHandler.<UseRelativeMode>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup65 {
    #[serde(rename = "<Stage>k__BackingField")]
    pub stage_k_backing_field: usize,
    #[serde(rename = "<CurrentRound>k__BackingField")]
    pub current_round_k_backing_field: usize,
    #[serde(rename = "<DamageMultiplier>k__BackingField")]
    pub damage_multiplier_k_backing_field: usize,
    #[serde(rename = "<Users>k__BackingField")]
    pub users_k_backing_field: usize,
    #[serde(rename = "<ActiveUserId>k__BackingField")]
    pub active_user_id_k_backing_field: usize,
    #[serde(rename = "<StarRating>k__BackingField")]
    pub star_rating_k_backing_field: usize,
    #[serde(rename = "<WinningUserId>k__BackingField")]
    pub winning_user_id_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup65 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.stage_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MatchTypes.RankedPlay.RankedPlayRoomState.<Stage>k__BackingField",
            ));
        }
        if self.current_round_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MatchTypes.RankedPlay.RankedPlayRoomState.<CurrentRound>k__BackingField",
            ));
        }
        if self.damage_multiplier_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MatchTypes.RankedPlay.RankedPlayRoomState.<DamageMultiplier>k__BackingField",
            ));
        }
        if self.users_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MatchTypes.RankedPlay.RankedPlayRoomState.<Users>k__BackingField",
            ));
        }
        if self.active_user_id_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MatchTypes.RankedPlay.RankedPlayRoomState.<ActiveUserId>k__BackingField",
            ));
        }
        if self.star_rating_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MatchTypes.RankedPlay.RankedPlayRoomState.<StarRating>k__BackingField",
            ));
        }
        if self.winning_user_id_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MatchTypes.RankedPlay.RankedPlayRoomState.<WinningUserId>k__BackingField",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OffsetGroup66 {
    #[serde(rename = "<Rating>k__BackingField")]
    pub rating_k_backing_field: usize,
    #[serde(rename = "<Life>k__BackingField")]
    pub life_k_backing_field: usize,
    #[serde(rename = "<Hand>k__BackingField")]
    pub hand_k_backing_field: usize,
    #[serde(rename = "<RatingAfter>k__BackingField")]
    pub rating_after_k_backing_field: usize,
    #[serde(rename = "DamageInfo")]
    pub damage_info: usize,
    #[serde(rename = "<RoundsWon>k__BackingField")]
    pub rounds_won_k_backing_field: usize,
    #[serde(rename = "<DamageMultiplier>k__BackingField")]
    pub damage_multiplier_k_backing_field: usize,
    #[serde(flatten)]
    pub extras: IndexMap<String, Value>,
}
impl OffsetGroup66 {
    pub(super) fn validate(&self) -> Result<(), LazerOffsetsError> {
        if self.rating_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MatchTypes.RankedPlay.RankedPlayUserInfo.<Rating>k__BackingField",
            ));
        }
        if self.life_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MatchTypes.RankedPlay.RankedPlayUserInfo.<Life>k__BackingField",
            ));
        }
        if self.hand_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MatchTypes.RankedPlay.RankedPlayUserInfo.<Hand>k__BackingField",
            ));
        }
        if self.rating_after_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MatchTypes.RankedPlay.RankedPlayUserInfo.<RatingAfter>k__BackingField",
            ));
        }
        if self.damage_info == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MatchTypes.RankedPlay.RankedPlayUserInfo.DamageInfo",
            ));
        }
        if self.rounds_won_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MatchTypes.RankedPlay.RankedPlayUserInfo.<RoundsWon>k__BackingField",
            ));
        }
        if self.damage_multiplier_k_backing_field == 0 {
            return Err(LazerOffsetsError::Zero(
                "osu.Game.Online.Multiplayer.MatchTypes.RankedPlay.RankedPlayUserInfo.<DamageMultiplier>k__BackingField",
            ));
        }
        Ok(())
    }
}
