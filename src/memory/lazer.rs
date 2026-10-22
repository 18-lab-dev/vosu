use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
    RankedPlay, RankedPlayUserInfo, ReleaseCorrection, ResultScreen, Settings, Tourney,
    TourneyChat, TourneyUser, User,
};
use crate::model::states::{
    CalculateMods, HitErrorArray, KeyOverlayButton, LeaderboardPlayer, Mod, MultiplayerTeamType,
    Statistics, TourneyManagerChatItem,
};
use crate::process::{MemoryError, MemoryRead, Pattern, ProcessHandle, ReadRequest, ReadStatus};

const MAX_COLLECTION: usize = 1_000_000;
// Room/client counts and retained chat are far smaller than gameplay histories.
const MAX_ROOM_USERS: usize = 256;
const MAX_CHAT_MESSAGES: usize = 2_048;
const MAX_RETAINED_CHAT_BYTES: usize = 4 * 1024 * 1024;
const MAX_SCORE: f64 = 1_000_000.0;
const GAME_BASE_DELTAS: [usize; 7] = [0x24, 0x28, 0x2c, 0x20, 0x30, 0x1c, 0x34];
const UNCACHED_POINTER: usize = usize::MAX;

mod batch;
mod configuration;
mod data;
mod global_batch;
mod helpers;
mod menu_batch;
mod menu_global;
mod models;
mod presentation;
mod reader;
mod resolver;
mod room_batch;
mod roots;
mod score_batch;
mod scores;
mod screens;
mod skin;
#[cfg(test)]
mod tests_presentation;

use batch::*;
use global_batch::BeatmapHead;
use helpers::*;
use menu_global::MenuGlobal;
use models::*;
pub use models::{LazerMemory, LazerPattern};
use room_batch::{RankedState, RoomHead};
use roots::{GAME_ROOTS, GameRoot, SCREEN_SPAN_WORDS, screen_span};

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests_batch;
#[cfg(test)]
mod tests_behavior;
#[cfg(test)]
mod tests_core;
#[cfg(test)]
mod tests_menu_batch;
