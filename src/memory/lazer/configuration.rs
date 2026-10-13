use super::*;

const ENTRY_STRIDE: usize = 0x18;
/// Config-store entries are read from the value pointer (+0) through the key (+0x10..+0x14).
const STORE_ENTRY_SPAN: usize = 0x14;
/// Ruleset-cache entries are read from the key string (+0) through the config (+8..+0x10).
const CACHE_ENTRY_SPAN: usize = 0x10;
const MAX_CACHE_ENTRIES: usize = 64;
/// Larger dictionaries take the sequential decoder instead of growing the scratch buffer
/// without bound; real stores hold a few hundred entries.
const BATCHED_ENTRY_LIMIT: usize = 4096;
const BATCHED_STRING_BYTES: usize = 16 * 1024;
/// Ruleset short names are managed strings: length at +8, UTF-16 units at +12.
const STRING_LENGTH: usize = 8;
const STRING_CHARS: usize = 12;
const MAX_STRING_UNITS: i32 = 4095;

/// Local config keys and value widths, in [`LazerMemory::read_osu_config_sequential`] order.
/// `BackgroundStoryboard` (10) also feeds `background.video`; one read serves both.
const LOCAL_FIELDS: [(i32, usize); 19] = [
    (34, 4),
    (58, 4),
    (30, 4),
    (40, 4),
    (39, 4),
    (12, 1),
    (88, 1),
    (6, 8),
    (7, 8),
    (22, 8),
    (23, 8),
    (2, 4),
    (3, 4),
    (4, 1),
    (10, 1),
    (19, 1),
    (20, 1),
    (56, 1),
    (54, 1),
];
/// Framework config keys, offsets within the bindable value, and widths. The window sizes
/// read width and height together as one eight-byte span at +4.
const FRAMEWORK_FIELDS: [(i32, usize, usize); 7] = [
    (14, 0, 4),
    (7, 4, 8),
    (11, 4, 8),
    (3, 0, 8),
    (5, 0, 8),
    (4, 0, 8),
    (21, 0, 8),
];
const MANIA_FIELDS: [(i32, usize); 2] = [(0, 8), (1, 4)];
const VALUE_SLOTS: usize = LOCAL_FIELDS.len() + FRAMEWORK_FIELDS.len() + MANIA_FIELDS.len();

/// Settings plus the decoder state the sequential path assigns while reading them.
struct BatchedConfig {
    settings: Settings,
    scoring_display_mode: i32,
    leaderboard_visible: bool,
    replay_settings_overlay: bool,
    /// `skin.name`, or `None` when `settings` would omit it.
    skin: Option<String>,
}

/// Validates a dictionary header exactly as the sequential decoder does and bounds it.
fn dictionary_shape(header: &[u8; DICTIONARY_HEADER_LEN], max: usize) -> Option<(usize, usize)> {
    let entries = le_pointer(header)?;
    let count = le_i32(&header[DICTIONARY_COUNT..]);
    let count = usize::try_from(count).ok().filter(|count| *count <= max)?;
    if (count > 0 && entries == 0) || count > BATCHED_ENTRY_LIMIT {
        return None;
    }
    Some((entries, count))
}

fn entries_len(count: usize, span: usize) -> usize {
    count
        .checked_sub(1)
        .map_or(0, |last| last * ENTRY_STRIDE + span)
}

/// Resolves bindable value addresses with `config_store` semantics: null values are skipped,
/// a later duplicate key replaces an earlier one, and every non-null value must offset safely.
fn store_addresses(
    entries: &[u8],
    keys: impl Iterator<Item = i32>,
    out: &mut [usize],
) -> Option<()> {
    let mut wanted = [0i32; LOCAL_FIELDS.len()];
    let mut count = 0;
    for key in keys {
        wanted[count] = key;
        count += 1;
    }
    let wanted = &wanted[..count];
    let out = &mut out[..count];
    out.fill(0);
    for entry in entries.chunks(ENTRY_STRIDE) {
        let value = le_pointer(entry)?;
        if value == 0 {
            continue;
        }
        // A non-null value is at least 1, so a resolved address is never the 0 sentinel.
        let address = add(value, 0x40).ok()?;
        let key = le_i32(&entry[0x10..]);
        if let Some(slot) = wanted.iter().position(|wanted| *wanted == key) {
            out[slot] = address;
        }
    }
    out.iter().all(|address| *address != 0).then_some(())
}

impl LazerMemory {
    pub(super) fn read_osu_config(&mut self) -> Result<Settings, MemoryError> {
        // `game_base` is the sequential decoder's first fallible step; resolving it once here
        // keeps an unresolved base from scanning twice.
        self.game_base()?;
        self.batched_skin = None;
        let mut scratch = std::mem::take(&mut self.config_scratch);
        let batched = self.read_osu_config_batched(&mut scratch);
        self.config_scratch = scratch;
        // Any failed, missing, or out-of-bounds batched read reruns the sequential decoder,
        // so errors and partially assigned decoder state stay exactly those of its read order.
        let Some(batched) = batched else {
            return self.read_osu_config_sequential();
        };
        self.scoring_display_mode = batched.scoring_display_mode;
        self.leaderboard_visible = batched.leaderboard_visible;
        self.replay_settings_overlay = batched.replay_settings_overlay;
        self.batched_skin = Some(batched.skin);
        Ok(batched.settings)
    }

    /// Decodes the same fields as the sequential path with one native batch per pointer level:
    /// whole dictionary entry arrays instead of two reads per entry, then every value at once.
    fn read_osu_config_batched(&self, scratch: &mut Vec<u8>) -> Option<BatchedConfig> {
        if self.process.pointer_size() != 8 {
            return None;
        }
        let o = &self.offsets;
        let local = self.root(GameRoot::LocalConfig).ok()?;
        let framework = self.root(GameRoot::FrameworkConfig).ok()?;
        let cache = self.root(GameRoot::RulesetConfigCache).ok()?;
        if local == 0 || framework == 0 || cache == 0 {
            return None;
        }
        // The skin name chain shares these levels. As in `settings`, where any failure of that
        // chain only omits `skin.name`, a null link or failed root yields no name; reads at
        // non-null links stay all-or-error with the rest of the batch.
        let link = |object: usize, offset: usize| {
            (object != 0).then(|| object.checked_add(offset)).flatten()
        };
        let manager = self.root(GameRoot::SkinManager).unwrap_or(0);
        let [local_store, framework_store, dict, current] = pointers(read_fixed(
            &self.process,
            [
                Some(add(local, 0x20).ok()?),
                Some(add(framework, 0x20).ok()?),
                Some(add(cache, o.osu_game_rulesets_ruleset_config_cache.config_cache).ok()?),
                link(manager, o.osu_game_skinning_skin_manager.current_skin),
            ],
        )?)?;
        if local_store == 0 || framework_store == 0 || dict == 0 {
            return None;
        }
        let mut headers = [[0u8; DICTIONARY_HEADER_LEN]; 3];
        let mut skin_value = [0u8; 8];
        {
            let mut stage = Stage::<4>::new();
            for (header, store) in headers.iter_mut().zip([local_store, framework_store, dict]) {
                stage.push(add(store, DICTIONARY_HEADER).ok()?, header);
            }
            if let Some(address) = link(current, 0x20) {
                stage.push(address, &mut skin_value);
            }
            stage.read(&self.process)?;
        }
        let skin_value = le_pointer(&skin_value)?;
        let (local_entries, local_count) = dictionary_shape(&headers[0], MAX_COLLECTION)?;
        let (framework_entries, framework_count) = dictionary_shape(&headers[1], MAX_COLLECTION)?;
        let (dict_entries, dict_count) = dictionary_shape(&headers[2], MAX_CACHE_ENTRIES)?;

        let lengths = [
            entries_len(local_count, STORE_ENTRY_SPAN),
            entries_len(framework_count, STORE_ENTRY_SPAN),
            entries_len(dict_count, CACHE_ENTRY_SPAN),
        ];
        scratch.clear();
        scratch.resize(lengths.iter().sum(), 0);
        let (local_bytes, rest) = scratch.split_at_mut(lengths[0]);
        let (framework_bytes, dict_bytes) = rest.split_at_mut(lengths[1]);
        let mut skin_name = [0u8; 8];
        {
            let mut requests = [
                ReadRequest::new(add(local_entries, 0x10).ok()?, local_bytes),
                ReadRequest::new(add(framework_entries, 0x10).ok()?, framework_bytes),
                ReadRequest::new(add(dict_entries, 0x10).ok()?, dict_bytes),
                match link(skin_value, o.osu_game_skinning_skin.name_k_backing_field) {
                    Some(address) => ReadRequest::new(address, &mut skin_name),
                    None => ReadRequest::new(0, &mut []),
                },
            ];
            let mut count = 0;
            for index in 0..requests.len() {
                if !requests[index].dst.is_empty() {
                    requests.swap(count, index);
                    count += 1;
                }
            }
            self.process.read_many(&mut requests[..count]).ok()?;
        }
        let skin_name = le_pointer(&skin_name)?;
        let mut local_addresses = [0; LOCAL_FIELDS.len()];
        store_addresses(
            local_bytes,
            LOCAL_FIELDS.iter().map(|field| field.0),
            &mut local_addresses,
        )?;
        let mut framework_addresses = [0; LOCAL_FIELDS.len()];
        store_addresses(
            framework_bytes,
            FRAMEWORK_FIELDS.iter().map(|field| field.0),
            &mut framework_addresses,
        )?;
        let mut cache_keys = [0; MAX_CACHE_ENTRIES];
        let mut cache_configs = [0; MAX_CACHE_ENTRIES];
        for (index, entry) in dict_bytes.chunks(ENTRY_STRIDE).enumerate() {
            cache_keys[index] = le_pointer(entry)?;
            cache_configs[index] = le_pointer(&entry[8..])?;
        }
        let mania_addresses = match self.find_mania_config(
            &cache_keys[..dict_count],
            &cache_configs[..dict_count],
            scratch,
        )? {
            Some(config) => Some(self.batched_store_addresses(config, &MANIA_FIELDS, scratch)?),
            None => None,
        };

        let mut requested = [(0usize, 0usize); VALUE_SLOTS];
        for (slot, (address, field)) in requested
            .iter_mut()
            .zip(local_addresses.iter().zip(LOCAL_FIELDS))
        {
            *slot = (*address, field.1);
        }
        for (slot, (address, field)) in requested[LOCAL_FIELDS.len()..]
            .iter_mut()
            .zip(framework_addresses.iter().zip(FRAMEWORK_FIELDS))
        {
            if field.1 == 4 {
                // Width and height are separate four-byte reads in the sequential decoder.
                add(*address, 8).ok()?;
            }
            *slot = (add(*address, field.1).ok()?, field.2);
        }
        let mut request_count = LOCAL_FIELDS.len() + FRAMEWORK_FIELDS.len();
        if let Some(addresses) = mania_addresses {
            for (address, field) in addresses.into_iter().zip(MANIA_FIELDS) {
                requested[request_count] = (address, field.1);
                request_count += 1;
            }
        }
        let mut values = [[0u8; 8]; VALUE_SLOTS];
        let mut skin_head = [0u8; STRING_HEAD];
        let skin_span = match skin_name {
            0 => None,
            name => Some(self.string_head_span(name)?),
        };
        {
            let mut pending = values
                .iter_mut()
                .zip(requested)
                .map(|(dst, (address, len))| ReadRequest::new(address, &mut dst[..len]));
            let mut requests: [ReadRequest<'_>; VALUE_SLOTS + 1] = std::array::from_fn(|_| {
                pending
                    .next()
                    .unwrap_or_else(|| ReadRequest::new(0, &mut []))
            });
            if let Some((address, len)) = skin_span {
                requests[request_count] = ReadRequest::new(address, &mut skin_head[..len]);
                request_count += 1;
            }
            self.process
                .read_many(&mut requests[..request_count])
                .ok()?;
        }
        let skin = if manager == 0 || current == 0 || skin_value == 0 {
            None
        } else {
            match skin_span {
                None => Some(String::new()),
                Some((_, len)) => self.string_from_head(skin_name, &skin_head[..len]),
            }
        };

        let (local, rest) = values.split_at(LOCAL_FIELDS.len());
        let (framework, mania) = rest.split_at(FRAMEWORK_FIELDS.len());
        let flag = |bytes: &[u8; 8]| bytes[0] == 1;
        let mut out = IndexMap::with_capacity(32);
        out.insert("client.branch".into(), Value::from(le_i32(&local[0])));
        out.insert("leaderboardType".into(), Value::from(le_i32(&local[2])));
        out.insert("sortType".into(), Value::from(le_i32(&local[3])));
        out.insert("groupType".into(), Value::from(le_i32(&local[4])));
        for (index, name, mul) in [
            (7, "background.dim", 100.0),
            (8, "background.blur", 100.0),
            (9, "audio.offset.universal", 1.0),
            (10, "audio.volume.masterInactive", 100.0),
        ] {
            out.insert(
                name.into(),
                Value::from(js_fixed2(le_f64(&local[index]) * mul)),
            );
        }
        for (index, name) in [(11, "cursor.menuSize"), (12, "cursor.size")] {
            let value = f32::from_le_bytes(local[index][..4].try_into().unwrap());
            out.insert(name.into(), Value::from(js_fixed2(value as f64)));
        }
        for (index, name) in [
            (13, "cursor.autoSize"),
            (14, "background.storyboard"),
            (15, "mouse.disableButtons"),
            (16, "mouse.disableWheel"),
            (14, "background.video"),
        ] {
            out.insert(name.into(), Value::from(flag(&local[index])));
        }
        let beatmap_hitsounds = flag(&local[17]);
        out.insert(
            "audio.ignoreBeatmapSounds".into(),
            Value::from(!beatmap_hitsounds),
        );
        out.insert(
            "audio.useSkinSamples".into(),
            Value::from(!beatmap_hitsounds),
        );
        out.insert(
            "skin.ignoreBeatmapSkins".into(),
            Value::from(!flag(&local[18])),
        );
        out.insert(
            "resolution.fullscreen".into(),
            Value::from(le_i32(&framework[0]) == 2),
        );
        out.insert(
            "resolution.width".into(),
            Value::from(le_i32(&framework[1])),
        );
        out.insert(
            "resolution.height".into(),
            Value::from(le_i32(&framework[1][4..])),
        );
        out.insert(
            "resolution.widthFullscreen".into(),
            Value::from(le_i32(&framework[2])),
        );
        out.insert(
            "resolution.heightFullscreen".into(),
            Value::from(le_i32(&framework[2][4..])),
        );
        for (index, name) in [
            (3, "audio.volume.master"),
            (4, "audio.volume.music"),
            (5, "audio.volume.effect"),
        ] {
            out.insert(
                name.into(),
                Value::from(js_fixed2(le_f64(&framework[index]) * 100.0)),
            );
        }
        out.insert(
            "mouse.sensitivity".into(),
            Value::from(le_f64(&framework[6])),
        );
        if mania_addresses.is_some() {
            out.insert("mania.scrollSpeed".into(), Value::from(le_f64(&mania[0])));
            out.insert(
                "mania.scrollDirection".into(),
                Value::from(le_i32(&mania[1])),
            );
        }
        Some(BatchedConfig {
            settings: out,
            scoring_display_mode: le_i32(&local[1]),
            leaderboard_visible: flag(&local[5]),
            replay_settings_overlay: flag(&local[6]),
            skin,
        })
    }

    /// Finds the first ruleset-cache entry named `mania` with a non-null config, validating
    /// every earlier key string as `string_at` would. `None` defers to the sequential decoder.
    fn find_mania_config(
        &self,
        keys: &[usize],
        configs: &[usize],
        scratch: &mut Vec<u8>,
    ) -> Option<Option<usize>> {
        if keys.is_empty() {
            return Some(None);
        }
        let mut length_addresses = [None; MAX_CACHE_ENTRIES];
        for (address, key) in length_addresses.iter_mut().zip(keys) {
            if *key != 0 {
                *address = Some(add(*key, STRING_LENGTH).ok()?);
            }
        }
        let lengths = read_fixed::<MAX_CACHE_ENTRIES, 4>(&self.process, length_addresses)?
            .map(i32::from_le_bytes);
        let mut spans = [(0usize, 0usize); MAX_CACHE_ENTRIES];
        let mut total = 0usize;
        for (index, key) in keys.iter().enumerate() {
            if *key != 0 && (1..=MAX_STRING_UNITS).contains(&lengths[index]) {
                let len = lengths[index] as usize * 2;
                spans[index] = (total, len);
                total += len;
            }
        }
        if total > BATCHED_STRING_BYTES {
            return None;
        }
        scratch.clear();
        scratch.resize(total, 0);
        {
            let mut destinations = scratch.as_mut_slice();
            let mut requests: [ReadRequest<'_>; MAX_CACHE_ENTRIES] =
                std::array::from_fn(|_| ReadRequest::new(0, &mut []));
            let mut count = 0;
            for (index, key) in keys.iter().enumerate() {
                let len = spans[index].1;
                if len == 0 {
                    continue;
                }
                let (dst, rest) = std::mem::take(&mut destinations).split_at_mut(len);
                destinations = rest;
                requests[count] = ReadRequest::new(add(*key, STRING_CHARS).ok()?, dst);
                count += 1;
            }
            self.process.read_many(&mut requests[..count]).ok()?;
        }
        for (index, key) in keys.iter().enumerate() {
            if *key == 0 {
                continue;
            }
            let length = lengths[index];
            if !(0..=MAX_STRING_UNITS).contains(&length) {
                return None;
            }
            let (start, len) = spans[index];
            let units = scratch[start..start + len]
                .chunks_exact(2)
                .map(|unit| u16::from_le_bytes([unit[0], unit[1]]));
            let mut expected = "mania".chars();
            let mut equal = true;
            for decoded in char::decode_utf16(units) {
                let decoded = decoded.ok()?;
                equal = equal && expected.next() == Some(decoded);
            }
            if equal && expected.next().is_none() && configs[index] != 0 {
                return Some(Some(configs[index]));
            }
        }
        Some(None)
    }

    /// Batched `config_store` lookup of the given keys for one config object.
    fn batched_store_addresses<const N: usize>(
        &self,
        config: usize,
        fields: &[(i32, usize); N],
        scratch: &mut Vec<u8>,
    ) -> Option<[usize; N]> {
        let [store] = pointers(read_fixed(&self.process, [Some(add(config, 0x20).ok()?)])?)?;
        if store == 0 {
            return None;
        }
        let [header] = read_fixed::<1, DICTIONARY_HEADER_LEN>(
            &self.process,
            [Some(add(store, DICTIONARY_HEADER).ok()?)],
        )?;
        let (entries, count) = dictionary_shape(&header, MAX_COLLECTION)?;
        scratch.clear();
        scratch.resize(entries_len(count, STORE_ENTRY_SPAN), 0);
        if count > 0 {
            self.process
                .read_exact(add(entries, 0x10).ok()?, scratch)
                .ok()?;
        }
        let mut addresses = [0; LOCAL_FIELDS.len()];
        store_addresses(scratch, fields.iter().map(|field| field.0), &mut addresses)?;
        Some(std::array::from_fn(|index| addresses[index]))
    }

    pub(super) fn read_osu_config_sequential(&mut self) -> Result<Settings, MemoryError> {
        let mut out = IndexMap::new();
        let local = self.root(GameRoot::LocalConfig)?;
        let values = self.config_store(local)?;
        let address = |map: &IndexMap<i32, usize>, key: i32, context: &'static str| {
            map.get(&key)
                .copied()
                .ok_or_else(|| corrupt(context, key as usize, "missing required key"))
        };
        let i32v = |this: &Self, key: i32| {
            this.process
                .read_i32(address(&values, key, "local config")?)
        };
        let f64v = |this: &Self, key: i32| {
            this.process
                .read_f64(address(&values, key, "local config")?)
        };
        let f32v = |this: &Self, key: i32| {
            this.process
                .read_f32(address(&values, key, "local config")?)
        };
        let boolv = |this: &Self, key: i32| {
            Ok::<_, MemoryError>(
                this.process
                    .read_u8(address(&values, key, "local config")?)?
                    == 1,
            )
        };
        out.insert("client.branch".into(), Value::from(i32v(self, 34)?));
        self.scoring_display_mode = i32v(self, 58)?;
        out.insert("leaderboardType".into(), Value::from(i32v(self, 30)?));
        out.insert("sortType".into(), Value::from(i32v(self, 40)?));
        out.insert("groupType".into(), Value::from(i32v(self, 39)?));
        self.leaderboard_visible = boolv(self, 12)?;
        self.replay_settings_overlay = boolv(self, 88)?;
        for (key, name, mul) in [
            (6, "background.dim", 100.0),
            (7, "background.blur", 100.0),
            (22, "audio.offset.universal", 1.0),
            (23, "audio.volume.masterInactive", 100.0),
        ] {
            out.insert(name.into(), Value::from(js_fixed2(f64v(self, key)? * mul)));
        }
        for (key, name) in [(2, "cursor.menuSize"), (3, "cursor.size")] {
            out.insert(name.into(), Value::from(js_fixed2(f32v(self, key)? as f64)));
        }
        for (key, name) in [
            (4, "cursor.autoSize"),
            (10, "background.storyboard"),
            (19, "mouse.disableButtons"),
            (20, "mouse.disableWheel"),
        ] {
            out.insert(name.into(), Value::from(boolv(self, key)?));
        }
        out.insert("background.video".into(), Value::from(boolv(self, 10)?));
        let beatmap_hitsounds = boolv(self, 56)?;
        out.insert(
            "audio.ignoreBeatmapSounds".into(),
            Value::from(!beatmap_hitsounds),
        );
        out.insert(
            "audio.useSkinSamples".into(),
            Value::from(!beatmap_hitsounds),
        );
        out.insert(
            "skin.ignoreBeatmapSkins".into(),
            Value::from(!boolv(self, 54)?),
        );
        let framework = self.root(GameRoot::FrameworkConfig)?;
        let fv = self.config_store(framework)?;
        let fa = |key| address(&fv, key, "framework config");
        out.insert(
            "resolution.fullscreen".into(),
            Value::from(self.process.read_i32(fa(14)?)? == 2),
        );
        let windowed = fa(7)?;
        out.insert(
            "resolution.width".into(),
            Value::from(self.process.read_i32(add(windowed, 4)?)?),
        );
        out.insert(
            "resolution.height".into(),
            Value::from(self.process.read_i32(add(windowed, 8)?)?),
        );
        let fullscreen = fa(11)?;
        out.insert(
            "resolution.widthFullscreen".into(),
            Value::from(self.process.read_i32(add(fullscreen, 4)?)?),
        );
        out.insert(
            "resolution.heightFullscreen".into(),
            Value::from(self.process.read_i32(add(fullscreen, 8)?)?),
        );
        for (key, name) in [
            (3, "audio.volume.master"),
            (5, "audio.volume.music"),
            (4, "audio.volume.effect"),
        ] {
            out.insert(
                name.into(),
                Value::from(js_fixed2(self.process.read_f64(fa(key)?)? * 100.0)),
            );
        }
        out.insert(
            "mouse.sensitivity".into(),
            Value::from(self.process.read_f64(fa(21)?)?),
        );
        let cache = self.root(GameRoot::RulesetConfigCache)?;
        if cache == 0 {
            return Err(corrupt("ruleset config cache", cache, "null cache"));
        }
        let dict = self.ptr(add(
            cache,
            self.offsets
                .osu_game_rulesets_ruleset_config_cache
                .config_cache,
        )?)?;
        if dict == 0 {
            return Err(corrupt("ruleset config cache", cache, "null dictionary"));
        }
        let entries = self.ptr(add(dict, 0x10)?)?;
        let count = self.process.read_i32(add(dict, 0x38)?)?;
        if !(0..=64).contains(&count) {
            return Err(corrupt("ruleset config cache", dict, "invalid count"));
        }
        if count > 0 && entries == 0 {
            return Err(corrupt("ruleset config cache", dict, "null entries"));
        }
        for index in 0..count as usize {
            let entry = add(entries, 0x10 + index * 0x18)?;
            if self.string_at(entry)? != "mania" {
                continue;
            }
            let config = self.ptr(add(entry, 8)?)?;
            if config == 0 {
                continue;
            }
            let mania = self.config_store(config)?;
            out.insert(
                "mania.scrollSpeed".into(),
                Value::from(self.process.read_f64(address(&mania, 0, "mania config")?)?),
            );
            out.insert(
                "mania.scrollDirection".into(),
                Value::from(self.process.read_i32(address(&mania, 1, "mania config")?)?),
            );
            break;
        }
        Ok(out)
    }

    pub(super) fn read_gamemode(&self) -> Result<i64, MemoryError> {
        let bindable = self.root(GameRoot::Ruleset)?;
        let info = self.ptr(add(bindable, 0x20)?)?;
        self.i32(add(
            info,
            self.offsets
                .osu_game_rulesets_ruleset_info
                .online_id_k_backing_field,
        )?)
    }
}
