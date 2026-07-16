use super::*;

impl StableMemory {
    pub(super) fn leaderboard_player(
        &self,
        b: usize,
        mode: i64,
    ) -> Result<Option<LeaderboardPlayer>, MemoryError> {
        let e = self.ptr(self.add(b, 0x20)?)?;
        if e == 0 {
            return Ok(None);
        }
        let x = self.ptr(self.add(e, 0x1c)?)?;
        let m = mods(self.i32(self.add(x, 8)?)? ^ self.i32(self.add(x, 0xc)?)?);
        let score = self.ptr(self.add(b, 0x20)?)?;
        let up = if score == 0 {
            0
        } else {
            self.ptr(self.add(score, 0x48)?)?
        };
        let uid = if up == 0 {
            0
        } else {
            self.i32(self.add(up, 0x70)?)?
        };
        let s = legacy_stats(
            mode,
            self.process.read_i16(self.add(e, 0x8a)?)? as i64,
            self.process.read_i16(self.add(e, 0x88)?)? as i64,
            self.process.read_i16(self.add(e, 0x8c)?)? as i64,
            self.process.read_i16(self.add(e, 0x92)?)? as i64,
            0,
            0,
        );
        Ok(Some(LeaderboardPlayer {
            user_id: uid,
            name: self.string_ptr(self.add(b, 8)?)?,
            score: self.i32(self.add(b, 0x30)?)? as f64,
            combo: self.process.read_i16(self.add(e, 0x94)?)? as i64,
            accuracy: accuracy(mode, &s, &m),
            max_combo: self.process.read_i16(self.add(e, 0x68)?)? as i64,
            mods: m,
            statistics: s,
            team: self.i32(self.add(b, 0x40)?)?,
            position: self.i32(self.add(b, 0x2c)?)?,
            is_passing: self.process.read_u8(self.add(b, 0x4b)?)? != 0,
        }))
    }
    pub(super) fn setting_positions(
        &self,
        address: usize,
        config: bool,
    ) -> Result<Vec<usize>, MemoryError> {
        let slots = self.process.read_stable_dictionary_slots(address)?;
        let mut out = vec![];
        for (i, s) in slots.into_iter().enumerate() {
            if config {
                if let Ok(k) = self.string_ptr(s)
                    && CONFIG.iter().any(|(n, _, _)| *n == k)
                {
                    out.push(i)
                }
            } else if let Ok(k) = self.process.read_i32(s)
                && BINDINGS.iter().any(|(id, _)| *id == k)
            {
                out.push(i)
            }
        }
        Ok(out)
    }
    pub(super) fn setting_records(
        &self,
        address: usize,
        positions: &[usize],
    ) -> Result<Vec<Option<[u8; 16]>>, MemoryError> {
        if positions.is_empty() {
            return Ok(Vec::new());
        }
        let items = self.ptr(self.add(address, 8)?)?;
        let addresses = positions
            .iter()
            .map(|position| self.add(items, 8 + 0x10 * position))
            .collect::<Result<Vec<_>, _>>()?;
        let mut buffers = vec![[0u8; 16]; addresses.len()];
        let batch = {
            let mut requests = addresses
                .iter()
                .copied()
                .zip(buffers.iter_mut())
                .map(|(address, buffer)| ReadRequest::new(address, buffer))
                .collect::<Vec<_>>();
            self.process.read_many(&mut requests)
        };
        if batch.is_ok() {
            return Ok(buffers.into_iter().map(Some).collect());
        }
        Ok(addresses
            .into_iter()
            .map(|address| {
                self.process
                    .read_bytes(address, 16)
                    .ok()
                    .and_then(|bytes| bytes.try_into().ok())
            })
            .collect())
    }
    pub(super) fn config_values(
        &self,
        address: usize,
        positions: &[usize],
    ) -> Result<Vec<(String, Value)>, MemoryError> {
        let records = self.setting_records(address, positions)?;
        let mut candidates = Vec::new();
        for record in records.into_iter().flatten() {
            let key_pointer = u32::from_le_bytes(record[0..4].try_into().unwrap()) as usize;
            let bind = u32::from_le_bytes(record[4..8].try_into().unwrap()) as usize;
            let Ok(key) = self.string(key_pointer) else {
                continue;
            };
            let Some((_, kind, path)) = CONFIG.iter().find(|(name, _, _)| *name == key) else {
                continue;
            };
            let (offset, length) = match *kind {
                "bool" => (0xc, 1),
                "enum" => (0xc, 4),
                "int" | "double" => (4, 8),
                "bstring" => (4, 4),
                _ => continue,
            };
            let Ok(value_address) = self.add(bind, offset) else {
                continue;
            };
            candidates.push((key, *kind, *path, value_address, length));
        }
        let mut buffers = vec![[0u8; 8]; candidates.len()];
        let mut valid = vec![true; candidates.len()];
        let batch = {
            let mut requests = candidates
                .iter()
                .zip(buffers.iter_mut())
                .map(|(candidate, buffer)| {
                    ReadRequest::new(candidate.3, &mut buffer[..candidate.4])
                })
                .collect::<Vec<_>>();
            self.process.read_many(&mut requests)
        };
        if batch.is_err() {
            for (index, candidate) in candidates.iter().enumerate() {
                match self.process.read_bytes(candidate.3, candidate.4) {
                    Ok(bytes) => buffers[index][..candidate.4].copy_from_slice(&bytes),
                    Err(_) => valid[index] = false,
                }
            }
        }
        let mut output = Vec::new();
        for (index, (_, kind, path, _, _)) in candidates.into_iter().enumerate() {
            if !valid[index] {
                continue;
            }
            let value = match kind {
                "bool" => json!(buffers[index][0] != 0),
                "enum" => json!(i32::from_le_bytes(buffers[index][0..4].try_into().unwrap())),
                "int" | "double" => json!(f64::from_le_bytes(buffers[index])),
                "bstring" => {
                    let pointer =
                        u32::from_le_bytes(buffers[index][0..4].try_into().unwrap()) as usize;
                    let Ok(value) = self.string(pointer) else {
                        continue;
                    };
                    json!(value)
                }
                _ => continue,
            };
            output.push((path.into(), value));
        }
        Ok(output)
    }
    pub(super) fn beatmap_scroll_speed(
        &mut self,
        global: Option<f64>,
    ) -> Result<Option<f64>, MemoryError> {
        if self.mania_speed_address == 0 && self.gameplay_mode == 3 {
            let p = Pattern::parse(MANIA_PATTERN, false).map_err(|e| MemoryError::Backend {
                operation: "parse mania pattern",
                message: e.to_string(),
            })?;
            self.mania_speed_address = self.process.scan_first(&p)?.unwrap_or(0)
        }
        if self.mania_speed_address == 0 {
            return Ok(global);
        }
        let ptr = self.ptr(self.add(self.mania_speed_address, 1)?)?;
        Ok(Some(self.process.read_i32(ptr)? as f64))
    }
}

pub(super) const CONFIG: [(&str, &str, &str); 37] = [
    ("VolumeUniversal", "int", "audio.volume.master"),
    ("VolumeEffect", "int", "audio.volume.effect"),
    ("VolumeMusic", "int", "audio.volume.music"),
    ("_ReleaseStream", "enum", "client.branch"),
    ("DimLevel", "int", "background.dim"),
    ("ShowStoryboard", "bool", "background.storyboard"),
    ("ScoreMeter", "enum", "scoreMeter.type"),
    ("ScoreMeterScale", "double", "scoreMeter.size"),
    ("Offset", "int", "audio.offset.universal"),
    ("CursorSize", "double", "cursor.size"),
    ("MouseSpeed", "double", "mouse.sensitivity"),
    ("Fullscreen", "bool", "resolution.fullscreen"),
    ("Width", "int", "resolution.width"),
    ("Height", "int", "resolution.height"),
    ("WidthFullscreen", "int", "resolution.widthFullscreen"),
    ("HeightFullscreen", "int", "resolution.heightFullscreen"),
    ("AutomaticCursorSizing", "bool", "cursor.autoSize"),
    ("IgnoreBeatmapSamples", "bool", "audio.ignoreBeatmapSounds"),
    ("SkinSamples", "bool", "audio.useSkinSamples"),
    ("LastVersion", "bstring", "client.version"),
    ("ManiaSpeed", "int", "mania.scrollSpeed"),
    ("ManiaSpeedBPMScale", "bool", "mania.speedBPMScale"),
    (
        "UsePerBeatmapManiaSpeed",
        "bool",
        "mania.usePerBeatmapSpeedScale",
    ),
    ("MouseDisableButtons", "bool", "mouse.disableButtons"),
    ("MouseDisableWheel", "bool", "mouse.disableWheel"),
    ("ProgressBarType", "enum", "progressBarType"),
    ("RankType", "enum", "leaderboardType"),
    ("UpdatePending", "bool", "client.updateAvailable"),
    ("UseSkinCursor", "bool", "cursor.useSkinCursor"),
    ("RawInput", "bool", "mouse.rawInput"),
    ("TreeSortMode", "enum", "groupType"),
    ("TreeSortMode2", "enum", "sortType"),
    ("EditorDefaultSkin", "bool", "skin.useDefaultSkinInEditor"),
    ("ComboColourSliderBall", "bool", "skin.tintSliderBall"),
    ("IgnoreBeatmapSkins", "bool", "skin.ignoreBeatmapSkins"),
    ("Skin", "bstring", "skin.name"),
    ("UseTaikoSkin", "bool", "skin.useTaikoSkin"),
];
pub(super) const BINDINGS: [(i32, &str); 11] = [
    (2, "keybinds.osu.k1"),
    (3, "keybinds.osu.k2"),
    (4, "keybinds.osu.smokeKey"),
    (13, "keybinds.fruits.Dash"),
    (11, "keybinds.fruits.k1"),
    (12, "keybinds.fruits.k2"),
    (6, "keybinds.taiko.innerLeft"),
    (7, "keybinds.taiko.innerRight"),
    (8, "keybinds.taiko.outerLeft"),
    (9, "keybinds.taiko.outerRight"),
    (23, "keybinds.quickRetry"),
];
pub(super) fn key_name(v: i32) -> Option<&'static str> {
    Some(match v {
        0 => "None",
        8 => "Back",
        9 => "Tab",
        13 => "Enter",
        20 => "CapsLock",
        27 => "Escape",
        32 => "Space",
        33 => "PageUp",
        34 => "PageDown",
        35 => "End",
        36 => "Home",
        37 => "Left",
        38 => "Up",
        39 => "Right",
        40 => "Down",
        41 => "Select",
        42 => "Print",
        43 => "Execute",
        44 => "PrintScreen",
        45 => "Insert",
        46 => "Delete",
        47 => "Help",
        48 => "D0",
        49 => "D1",
        50 => "D2",
        51 => "D3",
        52 => "D4",
        53 => "D5",
        54 => "D6",
        55 => "D7",
        56 => "D8",
        57 => "D9",
        65 => "A",
        66 => "B",
        67 => "C",
        68 => "D",
        69 => "E",
        70 => "F",
        71 => "G",
        72 => "H",
        73 => "I",
        74 => "J",
        75 => "K",
        76 => "L",
        77 => "M",
        78 => "N",
        79 => "O",
        80 => "P",
        81 => "Q",
        82 => "R",
        83 => "S",
        84 => "T",
        85 => "U",
        86 => "V",
        87 => "W",
        88 => "X",
        89 => "Y",
        90 => "Z",
        91 => "LeftWindows",
        92 => "RightWindows",
        93 => "Apps",
        95 => "Sleep",
        96 => "NumPad0",
        97 => "NumPad1",
        98 => "NumPad2",
        99 => "NumPad3",
        100 => "NumPad4",
        101 => "NumPad5",
        102 => "NumPad6",
        103 => "NumPad7",
        104 => "NumPad8",
        105 => "NumPad9",
        106 => "Multiply",
        107 => "Add",
        108 => "Separator",
        109 => "Subtract",
        110 => ".",
        111 => "/",
        112 => "F1",
        113 => "F2",
        114 => "F3",
        115 => "F4",
        116 => "F5",
        117 => "F6",
        118 => "F7",
        119 => "F8",
        120 => "F9",
        121 => "F10",
        122 => "F11",
        123 => "F12",
        124 => "F13",
        125 => "F14",
        126 => "F15",
        127 => "F16",
        128 => "F17",
        129 => "F18",
        130 => "F19",
        131 => "F20",
        132 => "F21",
        133 => "F22",
        134 => "F23",
        135 => "F24",
        144 => "NumLock",
        145 => "Scroll",
        160 => "LeftShift",
        161 => "RightShift",
        162 => "LeftControl",
        163 => "RightControl",
        164 => "LeftAlt",
        165 => "RightAlt",
        166 => "BrowserBack",
        167 => "BrowserForward",
        168 => "BrowserRefresh",
        169 => "BrowserStop",
        170 => "BrowserSearch",
        171 => "BrowserFavorites",
        172 => "BrowserHome",
        173 => "VolumeMute",
        174 => "VolumeDown",
        175 => "VolumeUp",
        176 => "MediaNextTrack",
        177 => "MediaPreviousTrack",
        178 => "MediaStop",
        179 => "MediaPlayPause",
        180 => "LaunchMail",
        181 => "SelectMedia",
        182 => "LaunchApplication1",
        183 => "LaunchApplication2",
        186 => ";",
        187 => "+",
        188 => ",",
        189 => "-",
        190 => ". ",
        191 => "?",
        192 => "~",
        219 => "[",
        220 => "|",
        221 => "]",
        222 => "\"",
        223 => "Oem8",
        226 => "\\",
        229 => "ProcessKey",
        246 => "Attn",
        247 => "Crsel",
        248 => "Exsel",
        249 => "EraseEof",
        250 => "Play",
        251 => "Zoom",
        253 => "Pa1",
        254 => "OemClear",
        _ => return None,
    })
}
