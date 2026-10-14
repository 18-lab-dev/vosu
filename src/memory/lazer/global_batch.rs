//! Batched pieces of [`MemoryReader::global`] and [`MemoryReader::menu`]; each returns
//! `None` to defer to the sequential decoder (see `batch.rs`).
use super::resolver::list_shape;
use super::*;

/// What `global` and its nested `global_precise` derive for a player screen.
pub(super) struct PlayerGlobal {
    pub(super) spectator: bool,
    pub(super) replay: bool,
    pub(super) show_interface: bool,
    pub(super) is_multi_spectating: bool,
    pub(super) chat_status: i64,
    pub(super) base_path: String,
    pub(super) time: f64,
    pub(super) paused: Option<bool>,
    /// The current beatmap's managed MD5 string, when the player path read it.
    pub(super) beatmap_hash: Option<String>,
    /// The beatmap track length, when the player path read it.
    pub(super) track_length: Option<f64>,
    /// The room's id, users list, and ranked-play state fields, for `room` and `ranked_play`.
    pub(super) room_head: Option<RoomHead>,
}

/// What `menu` needs for its checksum comparison, read by the batched `global` from the same
/// chain `current_beatmap` walks.
#[derive(Clone, Copy)]
pub(super) struct BeatmapHead {
    pub(super) info: usize,
    /// The beatmap set read with the info, as `current_beatmap` reads the pair.
    pub(super) set: usize,
    pub(super) status: i32,
    /// The MD5 string object (`0`: null).
    pub(super) hash: usize,
}

impl LazerMemory {
    /// `menu(previous)` when the current beatmap's hash still equals `previous`: the beatmap and
    /// ruleset chains share batches. A changed beatmap defers to the full sequential read.
    pub(super) fn menu_checksum_batched(&self, previous: &str) -> Option<MenuChecksum> {
        if self.process.pointer_size() != 8 || usize::BITS != 64 {
            return None;
        }
        let o = &self
            .offsets
            .osu_game_beatmaps_working_beatmap_cache_beatmap_manager_working_beatmap;
        if o.beatmap_info.checked_add(8) != Some(o.beatmap_set_info) {
            return None;
        }
        let beatmap = self.root(GameRoot::Beatmap).ok()?;
        let ruleset = self.root(GameRoot::Ruleset).ok()?;
        let [working, ruleset_info] = pointers(read_fixed(
            &self.process,
            [
                Some(add(beatmap, 0x20).ok()?),
                Some(add(ruleset, 0x20).ok()?),
            ],
        )?)?;

        let (mut beatmap_pair, mut gamemode) = ([0u8; 16], [0u8; 4]);
        let mut stage = Stage::<2>::new();
        stage.push(add(working, o.beatmap_info).ok()?, &mut beatmap_pair);
        stage.push(
            add(
                ruleset_info,
                self.offsets
                    .osu_game_rulesets_ruleset_info
                    .online_id_k_backing_field,
            )
            .ok()?,
            &mut gamemode,
        );
        stage.read(&self.process)?;
        let info = le_pointer(&beatmap_pair)?;
        if info == 0 {
            return None;
        }

        let b = &self.offsets.osu_game_beatmaps_beatmap_info;
        let (mut status, mut hash) = ([0u8; 4], [0u8; 8]);
        let mut stage = Stage::<2>::new();
        stage.push(add(info, b.status_int_k_backing_field).ok()?, &mut status);
        stage.push(add(info, b.md5_hash_k_backing_field).ok()?, &mut hash);
        stage.read(&self.process)?;
        let hash = self.string_prefetched(le_pointer(&hash)?)?;
        (hash == previous).then(|| MenuChecksum {
            gamemode: le_i32(&gamemode) as i64,
            ranked_status: lazer_ranked_status(le_i32(&status)),
        })
    }

    /// `(read_gamemode(), get_current_screen())` with the two pointer chains sharing batches.
    /// The current beatmap's head (`menu` reads it next) rides along when every link of its
    /// chain is non-null; otherwise `menu` reads it itself.
    pub(super) fn gamemode_and_screen_batched(&self) -> Option<(i64, usize, Option<BeatmapHead>)> {
        if self.process.pointer_size() != 8 || usize::BITS != 64 {
            return None;
        }
        let ruleset = self.root(GameRoot::Ruleset).ok()?;
        let stack = self.root(GameRoot::ScreenStack).ok()?;
        let beatmap = self.root(GameRoot::Beatmap).ok()?;
        let working_offsets = &self
            .offsets
            .osu_game_beatmaps_working_beatmap_cache_beatmap_manager_working_beatmap;
        // `current_beatmap` reads the info/set pair together only in this layout.
        let beatmap = (beatmap != 0
            && working_offsets.beatmap_info.checked_add(8)
                == Some(working_offsets.beatmap_set_info))
        .then_some(beatmap);

        let (mut info, mut list, mut working) = ([0u8; 8], [0u8; 8], [0u8; 8]);
        let mut stage = Stage::<3>::new();
        stage.push(add(ruleset, 0x20).ok()?, &mut info);
        stage.push(
            add(stack, self.offsets.osu_framework_screens_screen_stack.stack).ok()?,
            &mut list,
        );
        if let Some(beatmap) = beatmap {
            stage.push(add(beatmap, 0x20).ok()?, &mut working);
        }
        stage.read(&self.process)?;
        let [info, list, working] = pointers([info, list, working])?;
        if list == 0 {
            return None;
        }

        let mut gamemode = [0u8; 4];
        let mut header = [0u8; 12];
        let mut pair = [0u8; 16];
        let mut stage = Stage::<3>::new();
        stage.push(
            add(
                info,
                self.offsets
                    .osu_game_rulesets_ruleset_info
                    .online_id_k_backing_field,
            )
            .ok()?,
            &mut gamemode,
        );
        stage.push(add(list, 8).ok()?, &mut header);
        let working = (beatmap.is_some() && working != 0).then_some(working);
        if let Some(working) = working {
            stage.push(add(working, working_offsets.beatmap_info).ok()?, &mut pair);
        }
        stage.read(&self.process)?;
        let marker = le_i32(&header[8..]);
        let value = u64::from_le_bytes(header[..8].try_into().unwrap());
        let (size, items) = list_shape(list, marker, value).ok()?;
        // The set pointer is read with the info, as `current_beatmap` does; `menu` starts
        // from the pair when the beatmap changed.
        let beatmap_pair = match working {
            Some(_) => Some((le_pointer(&pair[..8])?, le_pointer(&pair[8..])?)),
            None => None,
        };

        let b = &self.offsets.osu_game_beatmaps_beatmap_info;
        let mut slots = [0u8; BATCHED_LIST_BYTES];
        let len = size.checked_mul(8).filter(|len| *len <= slots.len())?;
        let (mut status, mut hash) = ([0u8; 4], [0u8; 8]);
        let mut stage = Stage::<3>::new();
        if len > 0 {
            stage.push(items, &mut slots[..len]);
        }
        if let Some((info, _)) = beatmap_pair.filter(|(info, _)| *info != 0) {
            stage.push(add(info, b.status_int_k_backing_field).ok()?, &mut status);
            stage.push(add(info, b.md5_hash_k_backing_field).ok()?, &mut hash);
        }
        stage.read(&self.process)?;
        let mut screen = 0;
        for slot in slots[..len].chunks_exact(8) {
            screen = le_pointer(slot)?;
        }
        let head = match beatmap_pair {
            None => None,
            Some((0, _)) => Some(BeatmapHead {
                info: 0,
                set: 0,
                status: 0,
                hash: 0,
            }),
            Some((info, set)) => Some(BeatmapHead {
                info,
                set,
                status: le_i32(&status),
                hash: le_pointer(&hash)?,
            }),
        };
        Some((le_i32(&gamemode) as i64, screen, head))
    }

    /// The reads `global` and its nested `global_precise` make for a player screen after
    /// classifying it, one native batch per pointer level. Independent chains share levels;
    /// optional links follow the sequential decision trees (gameplay clock, else beatmap
    /// clock; drawable-ruleset pause bindable, else the clock container's). Shapes needing the
    /// framed-clock source path defer.
    /// `beatmap_hash` is the current beatmap's MD5 string object read by
    /// [`Self::gamemode_and_screen_batched`], decoded here for `menu` when present.
    pub(super) fn player_global_batched(
        &self,
        player: usize,
        beatmap_hash: Option<usize>,
    ) -> Option<PlayerGlobal> {
        if self.process.pointer_size() != 8 {
            return None;
        }
        let screen = self.current_screen;
        let play = &self.offsets.osu_game_screens_play_player;
        let clock_group = self
            .offsets
            .osu_game_screens_play_gameplay_clock_container
            .as_ref();
        let container_offset = play
            .gameplay_clock_container_k_backing_field
            .filter(|offset| *offset != 0);
        let clock_offset = clock_group
            .map(|group| group.gameplay_clock)
            .filter(|offset| *offset != 0);
        let container_pause_offset = clock_group
            .map(|group| group.is_paused)
            .filter(|offset| *offset != 0);
        let drawable_pause_offset = self
            .offsets
            .osu_game_rulesets_ui_drawable_ruleset
            .is_paused
            .filter(|offset| *offset != 0);

        let spectator = self.check_spectator(screen).ok()?;
        let drawable = self
            .screen_ptr(player, play.drawable_ruleset_k_backing_field)
            .ok()?;
        let hud = self
            .screen_ptr(player, play.hudoverlay_k_backing_field)
            .ok()?;
        let container = match container_offset {
            Some(offset) => self.screen_ptr(player, offset).ok()?,
            None => 0,
        };
        let client = self.root(GameRoot::MultiplayerClient).ok()?;
        let chat = self.root(GameRoot::ChatOverlay).ok()?;
        let storage = self.root(GameRoot::Storage).ok()?;
        let beatmap_clock = self.root(GameRoot::BeatmapClock).ok()?;
        let api = self.api().ok()?;
        let decoupled_offset = self
            .offsets
            .osu_game_beatmaps_framed_beatmap_clock
            .decoupled_track;

        // Level one: fields of the screen's children and of the game roots.
        let mut level = [[0u8; 8]; 10];
        let [
            replay_score,
            show,
            connected,
            room,
            state,
            wrapped,
            clock,
            drawable_pause,
            container_pause,
            beatmap_decoupled,
        ] = &mut level;
        // The local user chain `user` walks next, sharing these levels, and the beatmap hash
        // string `menu` compares.
        let mut user_state = [0u8; 8];
        let mut hash_head = [0u8; STRING_HEAD];
        let mut stage = Stage::<12>::new();
        let hash_len = match beatmap_hash {
            Some(hash) if hash != 0 => {
                let (address, len) = self.string_head_span(hash)?;
                stage.push(address, &mut hash_head[..len]);
                len
            }
            _ => 0,
        };
        if api != 0 {
            stage.push(
                add(
                    api,
                    self.offsets.osu_game_online_api_apiaccess.local_user_state,
                )
                .ok()?,
                &mut user_state,
            );
        }
        if drawable != 0 {
            stage.push(
                add(
                    drawable,
                    self.offsets
                        .osu_game_rulesets_ui_drawable_ruleset
                        .replay_score_k_backing_field,
                )
                .ok()?,
                replay_score,
            );
        }
        stage.push(
            add(
                hud,
                self.offsets
                    .osu_game_screens_play_hudoverlay
                    .show_hud_k_backing_field,
            )
            .ok()?,
            show,
        );
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
        if let (Some(offset), true) = (clock_offset, container != 0) {
            stage.push(add(container, offset).ok()?, clock);
        }
        if let (Some(offset), true) = (drawable_pause_offset, drawable != 0) {
            stage.push(add(drawable, offset).ok()?, drawable_pause);
        }
        if let (Some(offset), true) = (container_pause_offset, container != 0) {
            stage.push(add(container, offset).ok()?, container_pause);
        }
        stage.push(
            add(beatmap_clock, decoupled_offset).ok()?,
            beatmap_decoupled,
        );
        stage.read(&self.process)?;
        let [
            replay_score,
            show,
            connected,
            room,
            state,
            wrapped,
            clock,
            drawable_pause,
            container_pause,
            beatmap_decoupled,
        ] = pointers(level)?;
        let user_state = le_pointer(&user_state)?;
        // `multiplayer_room()` for the rest of this capture.
        self.capture_multiplayer_room
            .store(if client == 0 { 0 } else { room }, Ordering::Relaxed);

        // Level two: values behind them.
        let mut bytes = [[0u8; 8]; 8];
        let [
            show_value,
            connected_value,
            chat_status,
            base_path,
            gameplay_decoupled,
            drawable_paused,
            container_paused,
            beatmap_time,
        ] = &mut bytes;
        let (mut track, mut user_bindable, mut ranked_state) = ([0u8; 8], [0u8; 8], [0u8; 8]);
        let (mut room_id, mut room_users) = ([0u8; 8], [0u8; 8]);
        let room_offsets = &self.offsets.osu_game_online_multiplayer_multiplayer_room;
        let mut stage = Stage::<13>::new();
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
        stage.push(add(show, 0x40).ok()?, &mut show_value[..1]);
        if connected != 0 {
            stage.push(add(connected, 0x40).ok()?, &mut connected_value[..1]);
        }
        stage.push(add(state, 0x40).ok()?, &mut chat_status[..4]);
        stage.push(
            add(
                wrapped,
                self.offsets
                    .osu_framework_platform_storage
                    .base_path_k_backing_field,
            )
            .ok()?,
            base_path,
        );
        if clock != 0 {
            stage.push(add(clock, decoupled_offset).ok()?, gameplay_decoupled);
        }
        if drawable_pause != 0 {
            stage.push(add(drawable_pause, 0x40).ok()?, &mut drawable_paused[..1]);
        }
        if container_pause != 0 {
            stage.push(add(container_pause, 0x40).ok()?, &mut container_paused[..1]);
        }
        // DecouplingFramedClock.<CurrentTime>k__BackingField, as in `clock_current_time`.
        if beatmap_decoupled != 0 {
            stage.push(add(beatmap_decoupled, 0x20).ok()?, beatmap_time);
            // The beatmap track, which `mp3_length` reads through the same chain.
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
        stage.read(&self.process)?;
        let track = if beatmap_decoupled != 0 {
            let track = le_pointer(&track)?;
            self.capture_track.store(track, Ordering::Relaxed);
            track
        } else {
            0
        };
        let ranked_state = if ranked_room {
            let ranked_state = le_pointer(&ranked_state)?;
            self.capture_ranked_state
                .store(ranked_state, Ordering::Relaxed);
            ranked_state
        } else {
            0
        };
        let user_bindable = le_pointer(&user_bindable)?;
        let base_path = le_pointer(&bytes[3])?;
        let gameplay_decoupled = le_pointer(&bytes[4])?;

        // Level three: the base-path string and the gameplay clock's time.
        let mut gameplay_time = [0u8; 8];
        let mut head = [0u8; STRING_HEAD];
        let mut user = [0u8; 8];
        let mut track_length = [0u8; 8];
        let mut ranked_fields = RankedState::default();
        // `room`'s users list header, as `list_info` reads it.
        let room_users = if ranked_room && room_offsets.users_k_backing_field.is_some() {
            le_pointer(&room_users)?
        } else {
            0
        };
        let mut users_header = [0u8; 12];
        let mut stage = Stage::<12>::new();
        // `ranked_play`'s first reads.
        if ranked_state != 0 {
            ranked_fields.push(&self.offsets, ranked_state, &mut stage)?;
        }
        if room_users != 0 {
            stage.push(add(room_users, 8).ok()?, &mut users_header);
        }
        // `mp3_length`'s value read for the rest of this capture.
        if track != 0 {
            stage.push(
                add(track, self.offsets.osu_framework_audio_track_track.length).ok()?,
                &mut track_length,
            );
        }
        if user_bindable != 0 {
            stage.push(add(user_bindable, 0x20).ok()?, &mut user);
        }
        if gameplay_decoupled != 0 {
            stage.push(add(gameplay_decoupled, 0x20).ok()?, &mut gameplay_time);
        }
        let head_len = if base_path == 0 {
            0
        } else {
            let (address, len) = self.string_head_span(base_path)?;
            stage.push(address, &mut head[..len]);
            len
        };
        stage.read(&self.process)?;
        // `user()`'s pointer for the rest of this capture: a null bindable reads as no user.
        if user_state != 0 {
            let user = if user_bindable == 0 {
                0
            } else {
                le_pointer(&user)?
            };
            self.capture_user.store(user, Ordering::Relaxed);
        }
        let base_path = if base_path == 0 {
            String::new()
        } else {
            self.string_from_head(base_path, &head[..head_len])?
        };

        // `current_time_for_player`: the gameplay clock when present, else the beatmap clock;
        // a clock without a decoupled source needs the framed-source path, which defers.
        let time = if container_offset.is_some() && clock_offset.is_some() && clock != 0 {
            if gameplay_decoupled == 0 {
                return None;
            }
            le_f64(&gameplay_time)
        } else {
            if beatmap_decoupled == 0 {
                return None;
            }
            le_f64(&bytes[7])
        };
        // `gameplay_paused_for_player` with `player_loading` false, as `global_precise` sets.
        let paused = if drawable_pause_offset.is_some() && drawable != 0 && drawable_pause != 0 {
            Some(bytes[5][0] == 1)
        } else if container_offset.is_some()
            && container_pause_offset.is_some()
            && container != 0
            && container_pause != 0
        {
            Some(bytes[6][0] == 1)
        } else {
            None
        };
        let multi_room = if client == 0 || connected == 0 || bytes[1][0] != 1 {
            0
        } else {
            room
        };
        let is_multi_spectating = multi_room != 0 && self.check_multi_spectator(screen).ok()?;
        let replay = drawable != 0
            && replay_score != 0
            && self
                .screen_ptr(
                    screen,
                    self.offsets
                        .osu_game_screens_play_spectator_player
                        .spectator_client_k_backing_field,
                )
                .ok()?
                != self.spectator_client().ok()?;
        Some(PlayerGlobal {
            spectator,
            replay,
            show_interface: bytes[0][0] == 1,
            is_multi_spectating,
            chat_status: le_i32(&bytes[2]) as i64,
            base_path,
            time,
            paused,
            beatmap_hash: match beatmap_hash {
                Some(0) => Some(String::new()),
                Some(hash) => Some(self.string_from_head(hash, &hash_head[..hash_len])?),
                None => None,
            },
            track_length: (track != 0).then(|| le_f64(&track_length)),
            room_head: ranked_room.then_some(RoomHead {
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
