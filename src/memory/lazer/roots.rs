//! Per-capture cache of the game object's long-lived child pointers.
//!
//! These fields hold services and overlays the game creates once (API, clients, configs,
//! screen stack, ...). Like the API and spectator-client caches they replace, values live only
//! for one capture: [`LazerMemory::reset_capture_roots`] runs at every capture boundary. The
//! first use in a capture reads all roots in one native batch; if that batch fails, each root
//! falls back to its own read so errors are those of that field alone.
use super::*;

#[derive(Debug, Clone, Copy)]
pub(super) enum GameRoot {
    Api,
    SpectatorClient,
    MultiplayerClient,
    ScreenStack,
    Ruleset,
    SelectedMods,
    ChatOverlay,
    Storage,
    OsuLogo,
    SkinManager,
    Beatmap,
    BeatmapClock,
    LocalConfig,
    FrameworkConfig,
    RulesetConfigCache,
    ScoreManager,
    LeaderboardManager,
    SessionStatics,
    Realm,
}

pub(super) const GAME_ROOTS: usize = 19;

impl GameRoot {
    const ALL: [Self; GAME_ROOTS] = [
        Self::Api,
        Self::SpectatorClient,
        Self::MultiplayerClient,
        Self::ScreenStack,
        Self::Ruleset,
        Self::SelectedMods,
        Self::ChatOverlay,
        Self::Storage,
        Self::OsuLogo,
        Self::SkinManager,
        Self::Beatmap,
        Self::BeatmapClock,
        Self::LocalConfig,
        Self::FrameworkConfig,
        Self::RulesetConfigCache,
        Self::ScoreManager,
        Self::LeaderboardManager,
        Self::SessionStatics,
        Self::Realm,
    ];

    fn offset(self, offsets: &LazerOffsets) -> usize {
        let base = &offsets.osu_game_osu_game_base;
        let game = &offsets.osu_game_osu_game;
        let desktop = &offsets.osu_desktop_osu_game_desktop;
        match self {
            Self::Api => base.api_k_backing_field,
            Self::SpectatorClient => base.spectator_client_k_backing_field,
            Self::MultiplayerClient => base.multiplayer_client_k_backing_field,
            Self::ScreenStack => game
                .screen_stack
                .unwrap_or(game.screen_stack_k_backing_field),
            Self::Ruleset => desktop.ruleset,
            Self::SelectedMods => desktop.selected_mods,
            Self::ChatOverlay => game.chat_overlay,
            Self::Storage => base.storage_k_backing_field,
            Self::OsuLogo => game.osu_logo,
            Self::SkinManager => base.skin_manager_k_backing_field,
            Self::Beatmap => base.beatmap_k_backing_field,
            Self::BeatmapClock => base.beatmap_clock,
            Self::LocalConfig => base.local_config_k_backing_field,
            Self::FrameworkConfig => game.framework_config_k_backing_field,
            Self::RulesetConfigCache => base.ruleset_config_cache,
            Self::ScoreManager => base.score_manager_k_backing_field,
            Self::LeaderboardManager => base.leaderboard_manager_k_backing_field,
            Self::SessionStatics => base.session_statics_k_backing_field,
            Self::Realm => base.realm,
        }
    }
}

/// The root span read with or without the vtable pointer at +0; spans beyond it read roots one
/// by one.
const ROOT_SPAN_LIMIT: usize = 4096;

impl LazerMemory {
    /// Reads `game_base + root offset` as a pointer, at most once per capture.
    pub(super) fn root(&self, root: GameRoot) -> Result<usize, MemoryError> {
        let slot = &self.capture_roots[root as usize];
        let cached = slot.load(Ordering::Relaxed);
        if cached != UNCACHED_POINTER {
            return Ok(cached);
        }
        let base = self.game_base()?;
        // Validating the base may have loaded every root in the same batch.
        let cached = slot.load(Ordering::Relaxed);
        if cached != UNCACHED_POINTER {
            return Ok(cached);
        }
        if !self.capture_roots_loaded.swap(true, Ordering::Relaxed) && self.load_roots(base) {
            return Ok(slot.load(Ordering::Relaxed));
        }
        let value = self.ptr(add(base, root.offset(&self.offsets))?)?;
        slot.store(value, Ordering::Relaxed);
        Ok(value)
    }

    /// Reads every root from one contiguous span of the game object; one remote range costs
    /// far less than a scatter entry per field. The span lies inside the game object, so it is
    /// readable whenever the fields are.
    fn load_roots(&self, base: usize) -> bool {
        let Some((first, len)) = self.root_span() else {
            return false;
        };
        let mut span = [0u8; ROOT_SPAN_LIMIT];
        let Some(span) = span.get_mut(..len) else {
            return false;
        };
        let Ok(address) = add(base, first) else {
            return false;
        };
        self.process.pointer_size() == 8
            && self.process.read_exact(address, span).is_ok()
            && self.store_roots(span, first)
    }

    /// The game vtable check of `game_base` (`read_i64(vtable)`), reading the root span in the
    /// same native batch when this capture loads roots together. `None` leaves the check to
    /// its own read.
    pub(super) fn vtable_check_with_roots(&self, base: usize, vtable: usize) -> Option<bool> {
        if self.capture_roots_loaded.load(Ordering::Relaxed) || self.process.pointer_size() != 8 {
            return None;
        }
        let (first, len) = self.root_span()?;
        let mut span = [0u8; ROOT_SPAN_LIMIT];
        let span = span.get_mut(..len)?;
        let mut value = [0u8; 8];
        self.process
            .read_many(&mut [
                ReadRequest::new(vtable, &mut value),
                ReadRequest::new(add(base, first).ok()?, span),
            ])
            .ok()?;
        let valid = le_i64(&value) as usize == self.offsets.effective_game_base_vtable();
        if valid && self.store_roots(span, first) {
            self.capture_roots_loaded.store(true, Ordering::Relaxed);
        }
        Some(valid)
    }

    /// `(first offset, length)` of the span covering every root; `None` when untrusted offsets
    /// overflow, which leaves each root to its own checked read.
    fn root_span(&self) -> Option<(usize, usize)> {
        let offsets = GameRoot::ALL.map(|root| root.offset(&self.offsets));
        let first = offsets.iter().copied().min()?;
        let end = offsets.iter().copied().max()?.checked_add(8)?;
        Some((first, end - first))
    }

    fn store_roots(&self, span: &[u8], span_offset: usize) -> bool {
        let mut values = [0; GAME_ROOTS];
        for (value, root) in values.iter_mut().zip(GameRoot::ALL) {
            let Some(pointer) = le_pointer(&span[root.offset(&self.offsets) - span_offset..])
            else {
                return false;
            };
            *value = pointer;
        }
        for (slot, value) in self.capture_roots.iter().zip(values) {
            slot.store(value, Ordering::Relaxed);
        }
        true
    }

    /// Starts a new capture: roots, the player classification, the screen snapshot, and the
    /// chains `global` shares with later readers are read again. Only regular captures, which
    /// use most of them, load roots and the screen span together; a precise cycle reads just
    /// the few fields it needs.
    pub(super) fn reset_capture_roots(&self, load_together: bool) {
        for slot in &self.capture_roots {
            slot.store(UNCACHED_POINTER, Ordering::Relaxed);
        }
        self.capture_roots_loaded
            .store(!load_together, Ordering::Relaxed);
        self.capture_player_screen.store(0, Ordering::Relaxed);
        self.capture_screen.store(0, Ordering::Relaxed);
        self.capture_screen_pending
            .store(load_together, Ordering::Relaxed);
        self.capture_multiplayer_room
            .store(UNCACHED_POINTER, Ordering::Relaxed);
        self.capture_track
            .store(UNCACHED_POINTER, Ordering::Relaxed);
        self.capture_user.store(UNCACHED_POINTER, Ordering::Relaxed);
        self.capture_ranked_state
            .store(UNCACHED_POINTER, Ordering::Relaxed);
    }

    /// Reads a pointer field of a screen object. Fields of the current screen come from one
    /// per-capture read of its field span (see [`screen_span`]); other reads are direct.
    pub(super) fn screen_ptr(&self, screen: usize, offset: usize) -> Result<usize, MemoryError> {
        if let Some(bytes) = self.screen_bytes::<8>(screen, offset)
            && let Some(pointer) = le_pointer(&bytes)
        {
            return Ok(pointer);
        }
        self.ptr(add(screen, offset)?)
    }

    /// [`Self::screen_ptr`] for an `i32` field.
    pub(super) fn screen_i32(&self, screen: usize, offset: usize) -> Result<i64, MemoryError> {
        if let Some(bytes) = self.screen_bytes::<4>(screen, offset) {
            return Ok(le_i32(&bytes) as i64);
        }
        self.i32(add(screen, offset)?)
    }

    fn screen_bytes<const N: usize>(&self, screen: usize, offset: usize) -> Option<[u8; N]> {
        if screen == 0 {
            return None;
        }
        if self.capture_screen.load(Ordering::Relaxed) != screen {
            // Precise cycles never load the snapshot; a plain load keeps their misses cheap.
            if screen != self.current_screen || !self.capture_screen_pending.load(Ordering::Relaxed)
            {
                return None;
            }
            self.capture_screen_pending.store(false, Ordering::Relaxed);
            if !self.load_screen(screen) {
                return None;
            }
        }
        let (first, len) = self.screen_span;
        let start = offset.checked_sub(first)?;
        if start.checked_add(N)? > len {
            return None;
        }
        let mut out = [0u8; N];
        for (index, byte) in out.iter_mut().enumerate() {
            let at = start + index;
            let word = self.capture_screen_words[at / 8].load(Ordering::Relaxed);
            *byte = (word >> ((at % 8) * 8)) as u8;
        }
        Some(out)
    }

    /// Reads the current screen's field span in one native read. If the span is unreadable,
    /// every field falls back to its own direct read, so errors are those of that field.
    fn load_screen(&self, screen: usize) -> bool {
        let (first, len) = self.screen_span;
        if len == 0 || self.process.pointer_size() != 8 {
            return false;
        }
        let mut bytes = [0u8; SCREEN_SPAN_WORDS * 8];
        let Ok(address) = add(screen, first) else {
            return false;
        };
        if self.process.read_exact(address, &mut bytes[..len]).is_err() {
            return false;
        }
        for (word, bytes) in self
            .capture_screen_words
            .iter()
            .zip(bytes[..len].chunks_exact(8))
        {
            word.store(
                u64::from_le_bytes(bytes.try_into().unwrap()),
                Ordering::Relaxed,
            );
        }
        self.capture_screen.store(screen, Ordering::Relaxed);
        true
    }

    pub(super) fn api(&self) -> Result<usize, MemoryError> {
        self.root(GameRoot::Api)
    }

    pub(super) fn spectator_client(&self) -> Result<usize, MemoryError> {
        self.root(GameRoot::SpectatorClient)
    }
}

pub(super) const SCREEN_SPAN_WORDS: usize = 64;

/// The 8-byte-aligned span of screen-object fields the regular capture reads: player,
/// submitting/spectator player, results, song select, editor, multiplayer, and loader fields.
/// Spans over [`SCREEN_SPAN_WORDS`] words disable the snapshot.
pub(super) fn screen_span(offsets: &LazerOffsets) -> (usize, usize) {
    let player = &offsets.osu_game_screens_play_player;
    let submitting = &offsets.osu_game_screens_play_submitting_player;
    let spectator = &offsets.osu_game_screens_play_spectator_player;
    let fields = [
        player.score_manager_k_backing_field,
        player.api_k_backing_field,
        player.score_processor_k_backing_field,
        player.health_processor_k_backing_field,
        player.drawable_ruleset_k_backing_field,
        player.hudoverlay_k_backing_field,
        player.score_k_backing_field,
        submitting.api_k_backing_field,
        submitting.spectator_client_k_backing_field,
        spectator.spectator_client_k_backing_field,
        spectator.score,
        offsets.osu_game_screens_play_solo_player.restart_count,
        offsets.osu_game_screens_osu_screen.logo_k_backing_field,
        offsets
            .osu_game_screens_play_player_loader
            .leaderboard_manager_k_backing_field,
        offsets
            .osu_game_screens_ranking_solo_results_screen
            .api_k_backing_field,
        offsets
            .osu_game_screens_ranking_solo_results_screen
            .selected_score,
        offsets.osu_game_screens_edit_editor.api_k_backing_field,
        offsets.osu_game_screens_edit_editor.realm_k_backing_field,
    ];
    // Offsets come from the loaded offsets file; overflow disables the snapshot rather than
    // wrapping, and every field then takes its own checked read.
    let Some(first) = fields.iter().copied().min().map(|first| first & !7) else {
        return (0, 0);
    };
    let Some(end) = fields
        .iter()
        .copied()
        .max()
        .and_then(|last| last.checked_add(8 + 7))
        .map(|end| end & !7)
    else {
        return (0, 0);
    };
    let len = end - first;
    if len > SCREEN_SPAN_WORDS * 8 {
        return (0, 0);
    }
    (first, len)
}
