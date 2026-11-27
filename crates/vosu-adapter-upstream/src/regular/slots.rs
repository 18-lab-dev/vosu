//! Single-pass slot table over `CapturedSelection::values` for regular v2.
use crate::{Error, view};
use ciborium::Value;
use vosu_plugin_api::StateEntry;
use vosu_surface::state::CapturedSelection;

#[derive(Clone, Copy)]
#[repr(usize)]
pub(super) enum Slot {
    PlayClock = 0,
    SessionPlayTime,
    SessionServer,
    SessionState,
    RankedPlay,
    Settings,
    Profile,
    Beatmap,
    PlayFailed,
    PlayPlayerName,
    PlayMode,
    PlayScore,
    PlayAccuracyPercent,
    PlayHealthRaw,
    PlayJudgements,
    PlayStatistics,
    PlayHitErrorsAll,
    PlayCombo,
    PlayMods,
    PlayRank,
    PlayPp,
    PlayUnstableRate,
    Leaderboard,
    Performance,
    Result,
    InternalPaths,
    Tourney,
    TourneyAvailability,
    Room,
    SkinName,
}

const SLOT_COUNT: usize = 30;

const SLOT_META: [(&str, &str); SLOT_COUNT] = [
    ("vosu.osu", super::PLAY_CLOCK),
    ("vosu.osu", "/session/playTime"),
    ("vosu.osu", "/session/server"),
    ("vosu.osu", "/session/state"),
    ("vosu.osu", "/rankedPlay"),
    ("vosu.osu", "/settings"),
    ("vosu.osu", "/profile"),
    ("vosu.osu", "/beatmap"),
    ("vosu.osu", "/play/failed"),
    ("vosu.osu", "/play/playerName"),
    ("vosu.osu", "/play/mode"),
    ("vosu.osu", "/play/score"),
    ("vosu.osu", "/play/accuracyPercent"),
    ("vosu.osu", "/play/healthRaw"),
    ("vosu.osu", "/play/judgements"),
    ("vosu.osu", "/play/statistics"),
    ("vosu.osu", "/play/hitErrors/all"),
    ("vosu.osu", "/play/combo"),
    ("vosu.osu", "/play/mods"),
    ("vosu.osu", "/play/rank"),
    ("vosu.osu", "/play/pp"),
    ("vosu.osu", "/play/unstableRate"),
    ("vosu.osu", "/leaderboard"),
    ("vosu.osu", "/performance"),
    ("vosu.osu", "/result"),
    ("vosu.osu", "/internal/paths"),
    ("vosu.osu", "/tourney"),
    ("vosu.osu", "/tourneyAvailability"),
    ("vosu.osu", "/room"),
    ("vosu.skin", "/name"),
];

pub(super) struct RegularSlots<'a> {
    capture: &'a CapturedSelection,
    entries: [Option<&'a StateEntry>; SLOT_COUNT],
    has_osu_prefix: bool,
    has_skin_prefix: bool,
}

impl<'a> RegularSlots<'a> {
    pub(super) fn index(capture: &'a CapturedSelection) -> Self {
        let mut entries = [None; SLOT_COUNT];
        // Fast path: production and benchmark captures preserve `regular::selectors()` order.
        if capture.values.len() == SLOT_COUNT {
            let mut matched = true;
            for (i, entry) in capture.values.iter().enumerate() {
                let (ns, ptr) = SLOT_META[i];
                if entry.ns == ns && entry.pointer == ptr {
                    entries[i] = Some(entry);
                } else {
                    matched = false;
                    break;
                }
            }
            if matched {
                return Self {
                    capture,
                    entries,
                    has_osu_prefix: false,
                    has_skin_prefix: false,
                };
            }
            entries = [None; SLOT_COUNT];
        }
        let mut has_osu_prefix = false;
        let mut has_skin_prefix = false;
        for entry in capture.values.iter() {
            match entry.ns.as_str() {
                "vosu.osu" => match entry.pointer.as_str() {
                    super::PLAY_CLOCK => entries[Slot::PlayClock as usize] = Some(entry),
                    "/session/playTime" => entries[Slot::SessionPlayTime as usize] = Some(entry),
                    "/session/server" => entries[Slot::SessionServer as usize] = Some(entry),
                    "/session/state" => entries[Slot::SessionState as usize] = Some(entry),
                    "/rankedPlay" => entries[Slot::RankedPlay as usize] = Some(entry),
                    "/settings" => entries[Slot::Settings as usize] = Some(entry),
                    "/profile" => entries[Slot::Profile as usize] = Some(entry),
                    "/beatmap" => entries[Slot::Beatmap as usize] = Some(entry),
                    "/play/failed" => entries[Slot::PlayFailed as usize] = Some(entry),
                    "/play/playerName" => entries[Slot::PlayPlayerName as usize] = Some(entry),
                    "/play/mode" => entries[Slot::PlayMode as usize] = Some(entry),
                    "/play/score" => entries[Slot::PlayScore as usize] = Some(entry),
                    "/play/accuracyPercent" => {
                        entries[Slot::PlayAccuracyPercent as usize] = Some(entry);
                    }
                    "/play/healthRaw" => entries[Slot::PlayHealthRaw as usize] = Some(entry),
                    "/play/judgements" => entries[Slot::PlayJudgements as usize] = Some(entry),
                    "/play/statistics" => entries[Slot::PlayStatistics as usize] = Some(entry),
                    "/play/hitErrors/all" => {
                        entries[Slot::PlayHitErrorsAll as usize] = Some(entry);
                    }
                    "/play/combo" => entries[Slot::PlayCombo as usize] = Some(entry),
                    "/play/mods" => entries[Slot::PlayMods as usize] = Some(entry),
                    "/play/rank" => entries[Slot::PlayRank as usize] = Some(entry),
                    "/play/pp" => entries[Slot::PlayPp as usize] = Some(entry),
                    "/play/unstableRate" => entries[Slot::PlayUnstableRate as usize] = Some(entry),
                    "/leaderboard" => entries[Slot::Leaderboard as usize] = Some(entry),
                    "/performance" => entries[Slot::Performance as usize] = Some(entry),
                    "/result" => entries[Slot::Result as usize] = Some(entry),
                    "/internal/paths" => entries[Slot::InternalPaths as usize] = Some(entry),
                    "/tourney" => entries[Slot::Tourney as usize] = Some(entry),
                    "/tourneyAvailability" => {
                        entries[Slot::TourneyAvailability as usize] = Some(entry);
                    }
                    "/room" => entries[Slot::Room as usize] = Some(entry),
                    "" | "/session" | "/play" | "/play/hitErrors" | "/internal" => {
                        has_osu_prefix = true;
                    }
                    _ => {}
                },
                "vosu.skin" => match entry.pointer.as_str() {
                    "/name" => entries[Slot::SkinName as usize] = Some(entry),
                    "" => has_skin_prefix = true,
                    _ => {}
                },
                _ => {}
            }
        }
        Self {
            capture,
            entries,
            has_osu_prefix,
            has_skin_prefix,
        }
    }

    #[inline]
    pub(super) fn get(&self, slot: Slot) -> Result<&'a Value, Error> {
        let index = slot as usize;
        if let Some(entry) = self.entries[index] {
            if let Some(error) = &entry.error {
                let (ns, pointer) = SLOT_META[index];
                return Err(Error::InvalidSource(format!("{ns}{pointer}: {error:?}")));
            }
            return entry.value.as_ref().ok_or(Error::NotReady);
        }
        let (ns, pointer) = SLOT_META[index];
        let may_have_prefix = match slot {
            Slot::SkinName => self.has_skin_prefix,
            _ => self.has_osu_prefix,
        };
        if may_have_prefix {
            view::value(self.capture, ns, pointer)
        } else {
            Err(Error::NotReady)
        }
    }
}
