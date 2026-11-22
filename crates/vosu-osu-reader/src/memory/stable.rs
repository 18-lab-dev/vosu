use chrono::SecondsFormat;
use indexmap::IndexMap;
use md5::{Digest, Md5};
use serde_json::{Value, json};
use vosu_osu_model::{js_round_number, net_date_binary_to_date};

use super::{MemoryReader, ReaderSynchronization, SynchronizedMemoryReader};
use crate::process::{Pattern, ProcessHandle, checked_address_add};
use vosu_osu_model::memory::{
    AudioVelocityBase, Gameplay, Global, GlobalPrecise, HitErrors, KeyOverlay, Leaderboard, Menu,
    MenuChecksum, MenuFull, Mp3Length, RankedPlay, ResultScreen, Settings, StableReplayFrame,
    StableReplayFrameBatch, Tourney, TourneyChat, TourneyUser, User,
};
use vosu_osu_model::states::{
    CalculateMods, HitErrorArray, KeyOverlayButton, LeaderboardPlayer, Mod, Statistics,
    TourneyManagerChatItem,
};
use vosu_sys::process::{MemoryError, MemoryRead, ReadRequest, ReadStatus};

mod batched;
mod helpers;
mod models;
mod readers;
mod settings;
mod signatures;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_batch;

pub use models::StableMemory;
pub use signatures::StablePatterns;

use helpers::*;
use settings::*;
use signatures::{CHAT_PATTERN, LEADER_START, MANIA_PATTERN};
#[cfg(test)]
use signatures::{PATTERNS, shifted};
