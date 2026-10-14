//! Batched decoders for the multiplayer room and ranked-play state; each returns `None` to
//! defer to the sequential decoder (see `batch.rs`).
use super::resolver::list_shape;
use super::*;

/// Rooms with more members than this take the sequential decoder.
const BATCHED_ROOM_USERS: usize = 16;
const RANKED_ENTRY_STRIDE: usize = 0x18;
/// Ranked-play user entries are read from the user pointer (+0) through the id (+0x10..+0x14).
const RANKED_ENTRY_SPAN: usize = 0x14;
const MAX_RANKED_USERS: usize = 128;
/// Ranked-play entries prefetched with the array length, within the length field's page.
const RANKED_PREFETCH_ENTRIES: usize = 16;
const PAGE: usize = 4096;

fn nullable_i32(bytes: &[u8; 8]) -> Option<i32> {
    (bytes[0] != 0).then(|| le_i32(&bytes[4..]))
}

/// A multiplayer room's id and users list, and its ranked-play state's fields, read by the
/// batched `global` paths with the room this capture (see `global_batch.rs`); `room` and
/// `ranked_play` start from them instead of reading them again.
#[derive(Clone, Copy)]
pub(super) struct RoomHead {
    pub(super) room: usize,
    pub(super) room_id: [u8; 8],
    /// Absent when the offsets have no users field.
    pub(super) users: Option<[u8; 8]>,
    /// The users list's header (`list_info`'s read at +8) when the list is non-null.
    pub(super) users_header: Option<[u8; 12]>,
    /// The non-null match state and its fields.
    pub(super) ranked: Option<(usize, RankedState)>,
}

/// The ranked-play room state fields `ranked_play` reads first.
#[derive(Clone, Copy, Default)]
pub(super) struct RankedState {
    stage: [u8; 4],
    round: [u8; 4],
    stars: [u8; 8],
    damage: [u8; 8],
    dict: [u8; 8],
    active: [u8; 8],
    winning: [u8; 8],
}

impl RankedState {
    pub(super) fn push<'a, const N: usize>(
        &'a mut self,
        offsets: &LazerOffsets,
        state: usize,
        stage: &mut Stage<'a, N>,
    ) -> Option<()> {
        let o = &offsets.osu_game_online_multiplayer_match_types_ranked_play_ranked_play_room_state;
        stage.push(add(state, o.stage_k_backing_field).ok()?, &mut self.stage);
        stage.push(
            add(state, o.current_round_k_backing_field).ok()?,
            &mut self.round,
        );
        stage.push(
            add(state, o.star_rating_k_backing_field).ok()?,
            &mut self.stars,
        );
        stage.push(
            add(state, o.damage_multiplier_k_backing_field).ok()?,
            &mut self.damage,
        );
        stage.push(add(state, o.users_k_backing_field).ok()?, &mut self.dict);
        stage.push(
            add(state, o.active_user_id_k_backing_field).ok()?,
            &mut self.active,
        );
        stage.push(
            add(state, o.winning_user_id_k_backing_field).ok()?,
            &mut self.winning,
        );
        Some(())
    }
}

impl LazerMemory {
    /// [`MemoryReader::ranked_play`] for a non-null room.
    // Direct comparisons intentionally treat NaN as neither below nor above the valid range.
    #[allow(clippy::manual_range_contains)]
    pub(super) fn ranked_play_batched(&self, room: usize) -> Option<ReadStatus<RankedPlay>> {
        if self.process.pointer_size() != 8 {
            return None;
        }
        // The batched `global` reads this capture's room state pointer along with the room.
        let cached = self.capture_ranked_state.load(Ordering::Relaxed);
        let state = if cached != UNCACHED_POINTER {
            cached
        } else {
            let [state] = pointers(read_fixed(
                &self.process,
                [Some(
                    add(
                        room,
                        self.offsets
                            .osu_game_online_multiplayer_multiplayer_room
                            .match_state_k_backing_field,
                    )
                    .ok()?,
                )],
            )?)?;
            state
        };
        if state == 0 {
            return Some(ReadStatus::NotReady);
        }
        let carried = self
            .captured_room
            .filter(|head| head.room == room)
            .and_then(|head| head.ranked)
            .filter(|(carried, _)| *carried == state)
            .map(|(_, fields)| fields);
        let fields = match carried {
            Some(fields) => fields,
            None => {
                let mut fields = RankedState::default();
                let mut stage = Stage::<7>::new();
                fields.push(&self.offsets, state, &mut stage)?;
                stage.read(&self.process)?;
                fields
            }
        };
        let RankedState {
            stage: stage_value,
            round,
            stars,
            damage,
            dict,
            active,
            winning,
        } = fields;
        let Some(stage_enum) = RankedPlayStage::from_value(le_i32(&stage_value)) else {
            return Some(ReadStatus::NotReady);
        };
        let round = le_i32(&round) as i64;
        let stars = le_f64(&stars);
        let damage = le_f64(&damage);
        if !(1..=50).contains(&round) || stars < 0.0 || stars > 20.0 || damage < 0.0 {
            return Some(ReadStatus::NotReady);
        }
        let dict = le_pointer(&dict)?;
        if dict == 0 {
            return Some(ReadStatus::NotReady);
        }
        let [entries] = pointers(read_fixed(&self.process, [Some(add(dict, 0x10).ok()?)])?)?;
        if entries == 0 {
            return Some(ReadStatus::NotReady);
        }

        // The array length at +8 and leading entries at +0x10, clamped to the length's page.
        let length_address = add(entries, 8).ok()?;
        let page_end = (add(length_address, 3).ok()? | (PAGE - 1)).checked_add(1)?;
        let mut bytes = [0u8; 8 + MAX_RANKED_USERS * RANKED_ENTRY_STRIDE];
        let head_len =
            (8 + RANKED_PREFETCH_ENTRIES * RANKED_ENTRY_STRIDE).min(page_end - length_address);
        self.process
            .read_exact(length_address, &mut bytes[..head_len])
            .ok()?;
        let count = le_i32(&bytes);
        if !(0..=MAX_RANKED_USERS as i32).contains(&count) {
            return Some(ReadStatus::NotReady);
        }
        let count = count as usize;
        let needed = 8 + count
            .checked_sub(1)
            .map_or(0, |last| last * RANKED_ENTRY_STRIDE + RANKED_ENTRY_SPAN);
        if needed > head_len {
            self.process
                .read_exact(
                    add(length_address, head_len).ok()?,
                    &mut bytes[head_len..needed],
                )
                .ok()?;
        }

        let mut members = [(0usize, 0i64); BATCHED_ROOM_USERS];
        let mut member_count = 0;
        for entry in bytes[8..needed].chunks(RANKED_ENTRY_STRIDE) {
            if le_i32(&entry[8..]) == 0 {
                continue;
            }
            let user = le_pointer(entry)?;
            if user == 0 || member_count == BATCHED_ROOM_USERS {
                return None;
            }
            members[member_count] = (user, le_i32(&entry[0x10..]) as i64);
            member_count += 1;
        }
        let uo = &self
            .offsets
            .osu_game_online_multiplayer_match_types_ranked_play_ranked_play_user_info;
        let mut fields = [[0u8; 8]; BATCHED_ROOM_USERS * 4];
        {
            let mut stage = Stage::<{ BATCHED_ROOM_USERS * 4 }>::new();
            for ((user, _), fields) in members[..member_count]
                .iter()
                .zip(fields.chunks_exact_mut(4))
            {
                let [rating, life, rounds, damage] = fields else {
                    unreachable!()
                };
                stage.push(
                    add(*user, uo.rating_k_backing_field).ok()?,
                    &mut rating[..4],
                );
                stage.push(add(*user, uo.life_k_backing_field).ok()?, &mut life[..4]);
                stage.push(
                    add(*user, uo.rounds_won_k_backing_field).ok()?,
                    &mut rounds[..4],
                );
                stage.push(
                    add(*user, uo.damage_multiplier_k_backing_field).ok()?,
                    damage,
                );
            }
            stage.read(&self.process)?;
        }
        let users = members[..member_count]
            .iter()
            .zip(fields.chunks_exact(4))
            .map(|((_, id), fields)| RankedPlayUserInfo {
                id: *id,
                rating: le_i32(&fields[0]) as f64,
                life: le_i32(&fields[1]) as f64,
                rounds_won: le_i32(&fields[2]) as i64,
                damage_multiplier: le_f64(&fields[3]),
            })
            .collect();
        let user_id = |bytes: &[u8; 8]| {
            nullable_i32(bytes)
                .map(i64::from)
                .filter(|id| *id != 0)
                .unwrap_or(-1)
        };
        Some(ReadStatus::Ready(RankedPlay {
            stage: stage_enum,
            current_round: round,
            star_rating: stars,
            damage_multiplier: damage,
            active_user_id: user_id(&active),
            winning_user_id: user_id(&winning),
            users,
        }))
    }

    /// [`MemoryReader::room`] for a non-null room.
    pub(super) fn room_batched(&self, room: usize) -> Option<crate::model::memory::LazerRoom> {
        if self.process.pointer_size() != 8 {
            return None;
        }
        let room_offsets = &self.offsets.osu_game_online_multiplayer_multiplayer_room;
        let users_offset = room_offsets.users_k_backing_field?;
        let carried = self.captured_room.filter(|head| head.room == room);
        let (room_id, users) = match carried.and_then(|head| Some((head.room_id, head.users?))) {
            Some(carried) => carried,
            None => {
                let (mut room_id, mut users) = ([0u8; 8], [0u8; 8]);
                let mut stage = Stage::<2>::new();
                stage.push(add(room, room_offsets.room_id).ok()?, &mut room_id);
                stage.push(add(room, users_offset).ok()?, &mut users);
                stage.read(&self.process)?;
                (room_id, users)
            }
        };
        let users = le_pointer(&users)?;
        if users == 0 {
            return None;
        }
        let members = match carried.and_then(|head| head.users_header) {
            // `list_items_bounded` from the header it would read first.
            Some(header) => {
                let (size, items) = list_shape(
                    users,
                    le_i32(&header[8..]),
                    u64::from_le_bytes(header[..8].try_into().unwrap()),
                )
                .ok()?;
                if size > MAX_ROOM_USERS {
                    return None;
                }
                let mut members = Vec::with_capacity(size);
                if !self.slot_pointers_batched(items, size, 8, &mut members) {
                    return None;
                }
                members
            }
            None => self
                .list_items_bounded(users, false, 8, MAX_ROOM_USERS)
                .ok()?,
        };
        if members.len() > BATCHED_ROOM_USERS {
            return None;
        }
        let user_offsets = &self
            .offsets
            .osu_game_online_multiplayer_multiplayer_room_user;
        let mut fields = [([0u8; 4], [0u8; 8]); BATCHED_ROOM_USERS];
        {
            let mut stage = Stage::<{ BATCHED_ROOM_USERS * 2 }>::new();
            for (current, (id, api_user)) in members.iter().zip(fields.iter_mut()) {
                if *current != 0 {
                    stage.push(add(*current, user_offsets.user_id).ok()?, id);
                    stage.push(
                        add(*current, user_offsets.user_k_backing_field).ok()?,
                        api_user,
                    );
                }
            }
            stage.read(&self.process)?;
        }
        let avatar = self
            .offsets
            .osu_game_online_api_requests_responses_apiuser
            .avatar_url;
        let mut result = Vec::new();
        for (current, (id, api_user)) in members.iter().zip(fields) {
            if *current == 0 {
                continue;
            }
            let api_user = le_pointer(&api_user)?;
            if api_user == 0 {
                continue;
            }
            let (user, avatar_url) = self.read_user_batched(api_user, avatar)?;
            result.push(crate::model::memory::LazerRoomUser {
                id: le_i32(&id) as i64,
                username: user.name,
                country_code: user.country_code,
                avatar_url,
            });
        }
        Some(crate::model::memory::LazerRoom {
            room_id: le_i64(&room_id),
            users: result,
        })
    }
}
