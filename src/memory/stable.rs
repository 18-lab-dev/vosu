use crate::calculation::{js_round_number, net_date_binary_to_date};
use chrono::SecondsFormat;
use indexmap::IndexMap;
use md5::{Digest, Md5};
use serde_json::{Value, json};

use super::{MemoryReader, ReaderSynchronization, SynchronizedMemoryReader};
use crate::model::memory::{
    AudioVelocityBase, Gameplay, Global, GlobalPrecise, HitErrors, KeyOverlay, Leaderboard, Menu,
    MenuChecksum, MenuFull, Mp3Length, RankedPlay, ResultScreen, Settings, StableReplayFrame,
    StableReplayFrameBatch, Tourney, TourneyChat, TourneyUser, User,
};
use crate::model::states::{
    CalculateMods, HitErrorArray, KeyOverlayButton, LeaderboardPlayer, Mod, Statistics,
    TourneyManagerChatItem,
};
use crate::process::{
    MemoryError, MemoryRead, Pattern, ProcessHandle, ReadRequest, ReadStatus, checked_address_add,
};

mod helpers;
mod models;
mod readers;
mod settings;
mod signatures;
#[cfg(test)]
mod tests;

pub use models::StableMemory;
pub use signatures::StablePatterns;

use helpers::*;
use settings::*;
use signatures::{CHAT_PATTERN, LEADER_START, MANIA_PATTERN};
#[cfg(test)]
use signatures::{PATTERNS, shifted};
