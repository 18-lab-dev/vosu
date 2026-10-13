//! Batched decoders for the player's live score; each returns `None` to defer to the
//! sequential decoder (see `batch.rs`).
use super::*;

const MAX_STATISTICS: usize = 64;
const STATISTICS_ENTRY: usize = 0x10;

/// Statistics dictionary shape with `statistics_dict` semantics: `Some(None)` decodes as empty
/// statistics, and `None` is an invalid count that the sequential decoder reports.
fn statistics_shape(
    dict: usize,
    header: &[u8; DICTIONARY_HEADER_LEN],
) -> Option<Option<(usize, usize)>> {
    if dict == 0 {
        return Some(None);
    }
    let entries = le_pointer(header)?;
    if entries == 0 {
        return Some(None);
    }
    let count = usize::try_from(le_i32(&header[DICTIONARY_COUNT..]))
        .ok()
        .filter(|count| *count <= MAX_STATISTICS)?;
    Some(Some((entries, count)))
}

/// Queues a statistics dictionary's entries and returns their length.
fn push_statistics<'a, const N: usize>(
    stage: &mut Stage<'a, N>,
    shape: Option<(usize, usize)>,
    dst: &'a mut [u8; MAX_STATISTICS * STATISTICS_ENTRY],
) -> Option<usize> {
    match shape {
        Some((entries, count)) if count > 0 => {
            let len = count * STATISTICS_ENTRY;
            stage.push(add(entries, 0x10).ok()?, &mut dst[..len]);
            Some(len)
        }
        _ => Some(0),
    }
}

fn statistics_from(entries: &[u8]) -> Statistics {
    let mut statistics = empty_statistics();
    for entry in entries.chunks_exact(STATISTICS_ENTRY) {
        set_stat(
            &mut statistics,
            le_i32(&entry[8..]),
            le_i32(&entry[12..]) as i64,
        );
    }
    statistics
}

impl LazerMemory {
    /// Queues a non-null string's head and returns its span length (0 for null).
    fn push_string_head<'a, const N: usize>(
        &self,
        stage: &mut Stage<'a, N>,
        object: usize,
        dst: &'a mut [u8; STRING_HEAD],
    ) -> Option<usize> {
        if object == 0 {
            return Some(0);
        }
        let (address, len) = self.string_head_span(object)?;
        stage.push(address, &mut dst[..len]);
        Some(len)
    }

    fn string_from_span(
        &self,
        object: usize,
        head: &[u8; STRING_HEAD],
        len: usize,
    ) -> Option<String> {
        if object == 0 {
            return Some(String::new());
        }
        self.string_from_head(object, &head[..len])
    }

    /// [`LazerMemory::gameplay`] after its player check, one native batch per pointer level.
    /// Also returns the `leaderboard_score(score_info, -1)` entry of the same score, whose few
    /// extra fields ride in the same batches.
    pub(super) fn gameplay_batched(&self, player: usize) -> Option<(Gameplay, LeaderboardPlayer)> {
        if self.process.pointer_size() != 8 {
            return None;
        }
        let play = &self.offsets.osu_game_screens_play_player;
        let o = &self.offsets.osu_game_scoring_score_info;

        let hp = self
            .screen_ptr(player, play.health_processor_k_backing_field)
            .ok()?;
        let retries = self
            .screen_i32(
                player,
                self.offsets.osu_game_screens_play_solo_player.restart_count,
            )
            .ok()?;
        let score = self.screen_ptr(player, play.score_k_backing_field).ok()?;
        let processor = self
            .screen_ptr(player, play.score_processor_k_backing_field)
            .ok()?;
        if score == 0 {
            return None;
        }

        let (mut bind, mut info, mut combo_bind) = ([0u8; 8], [0u8; 8], [0u8; 8]);
        let mut stage = Stage::<3>::new();
        stage.push(
            add(
                hp,
                self.offsets
                    .osu_game_rulesets_osu_scoring_osu_health_processor
                    .health,
            )
            .ok()?,
            &mut bind,
        );
        stage.push(add(score, 8).ok()?, &mut info);
        stage.push(
            add(
                processor,
                self.offsets
                    .osu_game_rulesets_osu_scoring_osu_score_processor
                    .combo,
            )
            .ok()?,
            &mut combo_bind,
        );
        stage.read(&self.process)?;
        let [bind, info, combo_bind] = pointers([bind, info, combo_bind])?;
        if info == 0 {
            return None;
        }

        let (mut health, mut combo, mut max_combo) = ([0u8; 8], [0u8; 4], [0u8; 4]);
        let (mut total, mut accuracy) = ([0u8; 8], [0u8; 8]);
        let mut objects = [[0u8; 8]; 5];
        let (mut score_combo, mut passed) = ([0u8; 4], [0u8; 1]);
        let mut stage = Stage::<12>::new();
        stage.push(add(bind, 0x40).ok()?, &mut health);
        stage.push(add(combo_bind, 0x40).ok()?, &mut combo);
        let [statistics, maximum, realm, ruleset, mods] = &mut objects;
        stage.push(add(info, o.statistics).ok()?, statistics);
        stage.push(add(info, o.maximum_statistics).ok()?, maximum);
        stage.push(add(info, o.realm_user_k_backing_field).ok()?, realm);
        stage.push(add(info, o.ruleset_k_backing_field).ok()?, ruleset);
        stage.push(add(info, o.mods_json_k_backing_field).ok()?, mods);
        stage.push(add(info, o.total_score_k_backing_field).ok()?, &mut total);
        stage.push(add(info, o.accuracy_k_backing_field).ok()?, &mut accuracy);
        stage.push(add(info, o.max_combo_k_backing_field).ok()?, &mut max_combo);
        // The personal leaderboard entry's remaining score fields.
        stage.push(add(info, o.combo_k_backing_field).ok()?, &mut score_combo);
        stage.push(add(info, o.passed_k_backing_field).ok()?, &mut passed);
        stage.read(&self.process)?;
        let [statistics, maximum, realm, ruleset, mods] = pointers(objects)?;

        let mut headers = [[0u8; DICTIONARY_HEADER_LEN]; 2];
        let (mut username, mut mode, mut user_id) = ([0u8; 8], [0u8; 4], [0u8; 4]);
        let mut mods_head = [0u8; STRING_HEAD];
        let mut stage = Stage::<6>::new();
        for (header, dict) in headers.iter_mut().zip([statistics, maximum]) {
            if dict != 0 {
                stage.push(add(dict, DICTIONARY_HEADER).ok()?, header);
            }
        }
        stage.push(
            add(
                realm,
                self.offsets
                    .osu_game_models_realm_user
                    .username_k_backing_field,
            )
            .ok()?,
            &mut username,
        );
        stage.push(
            add(
                ruleset,
                self.offsets
                    .osu_game_rulesets_ruleset_info
                    .online_id_k_backing_field,
            )
            .ok()?,
            &mut mode,
        );
        stage.push(
            add(
                realm,
                self.offsets
                    .osu_game_models_realm_user
                    .online_id_k_backing_field,
            )
            .ok()?,
            &mut user_id,
        );
        let mods_len = self.push_string_head(&mut stage, mods, &mut mods_head)?;
        stage.read(&self.process)?;
        let username = le_pointer(&username)?;
        let current_shape = statistics_shape(statistics, &headers[0])?;
        let maximum_shape = statistics_shape(maximum, &headers[1])?;

        let mut entries = [[0u8; MAX_STATISTICS * STATISTICS_ENTRY]; 2];
        let mut username_head = [0u8; STRING_HEAD];
        let mut stage = Stage::<3>::new();
        let [statistics_entries, maximum_entries] = &mut entries;
        let statistics_len = push_statistics(&mut stage, current_shape, statistics_entries)?;
        let maximum_len = push_statistics(&mut stage, maximum_shape, maximum_entries)?;
        let username_len = self.push_string_head(&mut stage, username, &mut username_head)?;
        stage.read(&self.process)?;

        let statistics = statistics_from(&entries[0][..statistics_len]);
        let maximum_statistics = statistics_from(&entries[1][..maximum_len]);
        let username = self.string_from_span(username, &username_head, username_len)?;
        let mods =
            mods_from_json(info, &self.string_from_span(mods, &mods_head, mods_len)?).ok()?;
        let mode = le_i32(&mode) as i64;
        let health = le_f64(&health) * 200.0;
        let raw = js_long(le_i64(&total));
        let score = if self.scoring_display_mode == 1 {
            display_score(mode, raw, object_count(&maximum_statistics))
        } else {
            raw
        };
        let accuracy = le_f64(&accuracy) * 100.0;
        let max_combo = le_i32(&max_combo) as i64;
        // `leaderboard_score(score_info, -1)` of the same score.
        let personal = LeaderboardPlayer {
            user_id: le_i32(&user_id) as i64,
            name: username.clone(),
            score: raw,
            combo: le_i32(&score_combo) as i64,
            accuracy,
            max_combo,
            mods: mods.clone(),
            statistics: statistics.clone(),
            team: 0,
            position: 0,
            is_passing: passed[0] != 0,
        };
        let gameplay = Gameplay {
            failed: health <= 0.0,
            retries,
            player_name: display_username(username),
            mods,
            mode,
            score,
            player_hp_smooth: health,
            player_hp: health,
            accuracy,
            statistics,
            maximum_statistics,
            combo: le_i32(&combo) as i64,
            max_combo,
            pp: None,
        };
        Some((gameplay, personal))
    }

    /// `leaderboard_score(score_info(player), -1)` with one native batch per pointer level.
    pub(super) fn personal_leaderboard_batched(&self, player: usize) -> Option<LeaderboardPlayer> {
        if self.process.pointer_size() != 8 {
            return None;
        }
        let o = &self.offsets.osu_game_scoring_score_info;
        let score = self.current_score(player).ok()?;
        if score == 0 {
            return None;
        }
        let [info] = pointers(read_fixed(&self.process, [Some(add(score, 8).ok()?)])?)?;
        if info == 0 {
            return None;
        }

        let (mut combo, mut max_combo, mut passed) = ([0u8; 4], [0u8; 4], [0u8; 1]);
        let (mut total, mut accuracy) = ([0u8; 8], [0u8; 8]);
        let mut objects = [[0u8; 8]; 3];
        let mut stage = Stage::<8>::new();
        let [realm, statistics, mods] = &mut objects;
        stage.push(add(info, o.realm_user_k_backing_field).ok()?, realm);
        stage.push(add(info, o.statistics).ok()?, statistics);
        stage.push(add(info, o.mods_json_k_backing_field).ok()?, mods);
        stage.push(add(info, o.total_score_k_backing_field).ok()?, &mut total);
        stage.push(add(info, o.combo_k_backing_field).ok()?, &mut combo);
        stage.push(add(info, o.accuracy_k_backing_field).ok()?, &mut accuracy);
        stage.push(add(info, o.max_combo_k_backing_field).ok()?, &mut max_combo);
        stage.push(add(info, o.passed_k_backing_field).ok()?, &mut passed);
        stage.read(&self.process)?;
        let [realm, statistics, mods] = pointers(objects)?;

        let realm_user = &self.offsets.osu_game_models_realm_user;
        let (mut user_id, mut username) = ([0u8; 4], [0u8; 8]);
        let mut header = [0u8; DICTIONARY_HEADER_LEN];
        let mut mods_head = [0u8; STRING_HEAD];
        let mut stage = Stage::<4>::new();
        stage.push(
            add(realm, realm_user.online_id_k_backing_field).ok()?,
            &mut user_id,
        );
        stage.push(
            add(realm, realm_user.username_k_backing_field).ok()?,
            &mut username,
        );
        if statistics != 0 {
            stage.push(add(statistics, DICTIONARY_HEADER).ok()?, &mut header);
        }
        let mods_len = self.push_string_head(&mut stage, mods, &mut mods_head)?;
        stage.read(&self.process)?;
        let username = le_pointer(&username)?;
        let shape = statistics_shape(statistics, &header)?;

        let mut entries = [0u8; MAX_STATISTICS * STATISTICS_ENTRY];
        let mut username_head = [0u8; STRING_HEAD];
        let mut stage = Stage::<2>::new();
        let entries_len = push_statistics(&mut stage, shape, &mut entries)?;
        let username_len = self.push_string_head(&mut stage, username, &mut username_head)?;
        stage.read(&self.process)?;

        Some(LeaderboardPlayer {
            user_id: le_i32(&user_id) as i64,
            name: self.string_from_span(username, &username_head, username_len)?,
            score: js_long(le_i64(&total)),
            combo: le_i32(&combo) as i64,
            accuracy: le_f64(&accuracy) * 100.0,
            max_combo: le_i32(&max_combo) as i64,
            mods: mods_from_json(info, &self.string_from_span(mods, &mods_head, mods_len)?).ok()?,
            statistics: statistics_from(&entries[..entries_len]),
            team: 0,
            position: 0,
            is_passing: passed[0] != 0,
        })
    }
}
