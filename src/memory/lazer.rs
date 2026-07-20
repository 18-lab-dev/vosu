use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, RwLock};

use chrono::{SecondsFormat, Utc};
use indexmap::IndexMap;
use serde_json::{Number, Value};

use crate::calculation::{js_round_number, net_date_binary_to_date};
use crate::memory::lazer_offsets::LazerOffsets;
use crate::memory::{MemoryReader, ReaderSynchronization, SynchronizedMemoryReader};
use crate::model::enums::{GameState, RankedPlayStage};
use crate::model::memory::{
    AudioVelocityBase, Gameplay, Global, GlobalPrecise, HitErrors, KeyOverlay, LazerSpectator,
    LazerSpectatorEntry, Leaderboard, MatchmakingStats, Menu, MenuChecksum, MenuFull, Mp3Length,
    RankedPlay, RankedPlayUserInfo, ResultScreen, Settings, Tourney, TourneyChat, TourneyUser,
    User,
};
use crate::model::states::{
    CalculateMods, HitErrorArray, KeyOverlayButton, LeaderboardPlayer, Mod, MultiplayerTeamType,
    Statistics, TourneyManagerChatItem,
};
use crate::process::{MemoryError, MemoryRead, Pattern, ProcessHandle, ReadRequest, ReadStatus};

const MAX_COLLECTION: usize = 1_000_000;
const MAX_SCORE: f64 = 1_000_000.0;
const GAME_BASE_DELTAS: [usize; 7] = [0x24, 0x28, 0x2c, 0x20, 0x30, 0x1c, 0x34];

mod configuration;
mod data;
mod helpers;
mod models;
mod reader;
mod resolver;
mod scores;
mod screens;

use helpers::*;
use models::*;
pub use models::{LazerMemory, LazerPattern};

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests_behavior;
#[cfg(test)]
mod tests_core;
