//! Batched reads for `global` on a non-player screen; see [`MenuGlobal`].
use super::*;

/// What `global` and its nested `global_precise` read on a non-player screen, read ahead in
/// one native batch per pointer level by [`LazerMemory::prefetch_menu_global`] together with
/// the chains `mp3_length`, `user`, and the ranked-play reader walk later in the capture.
///
/// Each entry holds exactly what its sequential read returns; a failed or undecodable
/// prefetch leaves every entry empty and the sequential reads run as before. Entries live for
/// one capture: `synchronize_capture_config` and `set_resolved_state` clear them.
#[derive(Default)]
pub(super) struct MenuGlobal {
    /// For a non-null multiplayer client: whether `is_connected` is set, and the room.
    pub(super) multi: Option<(bool, usize)>,
    pub(super) chat_status: Option<i64>,
    pub(super) base_path: Option<String>,
    /// The beatmap clock and its decoupled clock's current time (`clock_current_time`).
    pub(super) beatmap_time: Option<(usize, f64)>,
}

impl LazerMemory {
    /// `beatmap_hash` is the current beatmap's MD5 string object, when the batched gamemode
    /// and screen read carried its head for `menu`; its value rides along for `menu`'s
    /// checksum comparison.
    pub(super) fn prefetch_menu_global(&mut self, beatmap_hash: Option<usize>) {
        let prefetched = self.menu_global_batched(beatmap_hash).unwrap_or_default();
        self.menu_global = prefetched.global;
        if prefetched.track_length.is_some() {
            self.captured_track_length = prefetched.track_length;
        }
        if let (Some(hash), Some((_, _, carried))) =
            (prefetched.beatmap_hash, &mut self.captured_beatmap)
        {
            *carried = Some(hash);
        }
        self.captured_room = prefetched.room;
    }

    fn menu_global_batched(&self, beatmap_hash: Option<usize>) -> Option<MenuPrefetch> {
        if self.process.pointer_size() != 8 {
            return None;
        }
        let client = self.root(GameRoot::MultiplayerClient).ok()?;
        let chat = self.root(GameRoot::ChatOverlay).ok()?;
        let storage = self.root(GameRoot::Storage).ok()?;
        let beatmap_clock = self.root(GameRoot::BeatmapClock).ok()?;
        let api = self.api().ok()?;
        let decoupled_offset = self
            .offsets
            .osu_game_beatmaps_framed_beatmap_clock
            .decoupled_track;

        // Level one: fields of the game roots.
        let mut level = [[0u8; 8]; 6];
        let [
            connected,
            room,
            state,
            wrapped,
            beatmap_decoupled,
            user_state,
        ] = &mut level;
        let mut hash_head = [0u8; STRING_HEAD];
        let mut stage = Stage::<7>::new();
        let hash_len = match beatmap_hash {
            Some(hash) if hash != 0 => {
                let (address, len) = self.string_head_span(hash)?;
                stage.push(address, &mut hash_head[..len]);
                len
            }
            _ => 0,
        };
        if client != 0 {
            stage.push(
                add(
                    client,
                    self.offsets
                        .osu_game_online_multiplayer_online_multiplayer_client
                        .is_connected_k_backing_field,
                )
                .ok()?,
                connected,
            );
            stage.push(
                add(
                    client,
                    self.offsets
                        .osu_game_online_multiplayer_multiplayer_client
                        .room,
                )
                .ok()?,
                room,
            );
        }
        stage.push(
            add(chat, self.offsets.osu_game_overlays_chat_overlay.state).ok()?,
            state,
        );
        stage.push(
            add(
                storage,
                self.offsets
                    .osu_game_io_wrapped_storage
                    .underlying_storage_k_backing_field,
            )
            .ok()?,
            wrapped,
        );
        stage.push(
            add(beatmap_clock, decoupled_offset).ok()?,
            beatmap_decoupled,
        );
        if api != 0 {
            stage.push(
                add(
                    api,
                    self.offsets.osu_game_online_api_apiaccess.local_user_state,
                )
                .ok()?,
                user_state,
            );
        }
        stage.read(&self.process)?;
        let [
            connected,
            room,
            state,
            wrapped,
            beatmap_decoupled,
            user_state,
        ] = pointers(level)?;

        // Level two: values behind them.
        let (mut connected_value, mut chat_status, mut base_path) = ([0u8; 1], [0u8; 4], [0u8; 8]);
        let (mut time, mut track, mut user_bindable, mut ranked_state) =
            ([0u8; 8], [0u8; 8], [0u8; 8], [0u8; 8]);
        let (mut room_id, mut room_users) = ([0u8; 8], [0u8; 8]);
        let room_offsets = &self.offsets.osu_game_online_multiplayer_multiplayer_room;
        let mut stage = Stage::<9>::new();
        if connected != 0 {
            stage.push(add(connected, 0x40).ok()?, &mut connected_value);
        }
        stage.push(add(state, 0x40).ok()?, &mut chat_status);
        stage.push(
            add(
                wrapped,
                self.offsets
                    .osu_framework_platform_storage
                    .base_path_k_backing_field,
            )
            .ok()?,
            &mut base_path,
        );
        // DecouplingFramedClock.<CurrentTime>k__BackingField, as in `clock_current_time`, and
        // the beatmap track `mp3_length` reads through the same chain.
        if beatmap_decoupled != 0 {
            stage.push(add(beatmap_decoupled, 0x20).ok()?, &mut time);
            stage.push(
                add(
                    beatmap_decoupled,
                    self.offsets
                        .osu_framework_timing_decoupling_framed_clock
                        .source_k_backing_field,
                )
                .ok()?,
                &mut track,
            );
        }
        if user_state != 0 {
            stage.push(
                add(
                    user_state,
                    self.offsets.osu_game_online_api_local_user_state.local_user,
                )
                .ok()?,
                &mut user_bindable,
            );
        }
        let ranked_room = client != 0 && room != 0;
        if ranked_room {
            stage.push(
                add(room, room_offsets.match_state_k_backing_field).ok()?,
                &mut ranked_state,
            );
            // `room`'s first reads.
            stage.push(add(room, room_offsets.room_id).ok()?, &mut room_id);
            if let Some(users) = room_offsets.users_k_backing_field {
                stage.push(add(room, users).ok()?, &mut room_users);
            }
        }
        stage.read(&self.process)?;
        let base_path = le_pointer(&base_path)?;
        let track = le_pointer(&track)?;
        let user_bindable = le_pointer(&user_bindable)?;
        let ranked_state = if ranked_room {
            le_pointer(&ranked_state)?
        } else {
            0
        };

        // Level three: the base-path string, the track length, the local user,
        // `ranked_play`'s first reads, and `room`'s users list header.
        let (mut track_length, mut user) = ([0u8; 8], [0u8; 8]);
        let mut head = [0u8; STRING_HEAD];
        let mut ranked_fields = RankedState::default();
        let room_users = if ranked_room && room_offsets.users_k_backing_field.is_some() {
            le_pointer(&room_users)?
        } else {
            0
        };
        let mut users_header = [0u8; 12];
        let mut stage = Stage::<11>::new();
        if ranked_state != 0 {
            ranked_fields.push(&self.offsets, ranked_state, &mut stage)?;
        }
        if room_users != 0 {
            stage.push(add(room_users, 8).ok()?, &mut users_header);
        }
        if track != 0 {
            stage.push(
                add(track, self.offsets.osu_framework_audio_track_track.length).ok()?,
                &mut track_length,
            );
        }
        if user_bindable != 0 {
            stage.push(add(user_bindable, 0x20).ok()?, &mut user);
        }
        let head_len = if base_path == 0 {
            0
        } else {
            let (address, len) = self.string_head_span(base_path)?;
            stage.push(address, &mut head[..len]);
            len
        };
        stage.read(&self.process)?;
        let base_path = if base_path == 0 {
            String::new()
        } else {
            self.string_from_head(base_path, &head[..head_len])?
        };
        let user = if user_bindable == 0 {
            0
        } else {
            le_pointer(&user)?
        };
        // Chains later readers share, as the player path carries them.
        self.capture_multiplayer_room
            .store(if client == 0 { 0 } else { room }, Ordering::Relaxed);
        if beatmap_decoupled != 0 {
            self.capture_track.store(track, Ordering::Relaxed);
        }
        if ranked_room {
            self.capture_ranked_state
                .store(ranked_state, Ordering::Relaxed);
        }
        if user_state != 0 {
            self.capture_user.store(user, Ordering::Relaxed);
        }
        Some(MenuPrefetch {
            global: MenuGlobal {
                multi: (client != 0).then_some((connected != 0 && connected_value[0] == 1, room)),
                chat_status: Some(le_i32(&chat_status) as i64),
                base_path: Some(base_path),
                beatmap_time: (beatmap_decoupled != 0).then(|| (beatmap_clock, le_f64(&time))),
            },
            track_length: (track != 0).then(|| le_f64(&track_length)),
            // An undecodable string is left to `menu`'s own read.
            beatmap_hash: match beatmap_hash {
                Some(0) => Some(String::new()),
                Some(hash) => self.string_from_head(hash, &hash_head[..hash_len]),
                None => None,
            },
            room: ranked_room.then_some(RoomHead {
                room,
                room_id,
                users: room_offsets
                    .users_k_backing_field
                    .map(|_| room_users.to_le_bytes()),
                users_header: (room_users != 0).then_some(users_header),
                ranked: (ranked_state != 0).then_some((ranked_state, ranked_fields)),
            }),
        })
    }
}

/// What [`LazerMemory::menu_global_batched`] reads for `global` and for later readers.
#[derive(Default)]
struct MenuPrefetch {
    global: MenuGlobal,
    track_length: Option<f64>,
    beatmap_hash: Option<String>,
    room: Option<RoomHead>,
}
