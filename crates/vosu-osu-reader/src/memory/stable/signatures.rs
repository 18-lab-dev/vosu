use super::*;

pub(super) const LEADER_START: usize = 8;
pub(super) const CHAT_PATTERN: &str = "A1 ?? ?? ?? ?? 89 45 F0 8B D1 85 C9 75";
pub(super) const MANIA_PATTERN: &str = "A3 ?? ?? ?? ?? EB ?? DD 45 08 DB 5D E8 8B 45 E8 83 F8 28";
pub(super) const PATTERNS: [(&str, i64); 16] = [
    ("F8 01 74 04 83 65", 0),
    ("5E 5F 5D C3 A1 ?? ?? ?? ?? 89 ?? 04", 0),
    ("8B CE 83 3D ?? ?? ?? ?? 00 75 ?? 80", 4),
    ("74 2C 85 FF 75 28 A1 ?? ?? ?? ?? 8D 15", 0),
    ("83 E0 20 85 C0 7E 2F", 0),
    (
        "8D 45 EC 50 8B 0D ?? ?? ?? ?? 8B D7 39 09 E8 ?? ?? ?? ?? 85 C0 74 ?? 8B 4D EC",
        6,
    ),
    (
        "8D 7D D0 B9 08 00 00 00 33 C0 F3 AB 8B CE 89 4D DC B9",
        0x2a,
    ),
    ("7D 15 A1 ?? ?? ?? ?? 85 C0", 0),
    ("55 8B EC 80 3D ?? ?? ?? ?? 00 75 26 80 3D", 0),
    ("48 83 F8 04 73 1E", -4),
    ("C8 FF ?? ?? ?? ?? ?? 81 0D ?? ?? ?? ?? ?? 08 00 00", 9),
    ("55 8B EC 83 EC 08 A1 ?? ?? ?? ?? 85 C0", 7),
    ("FF 15 ?? ?? ?? ?? A1 ?? ?? ?? ?? 8B 48 54 33 D2", 7),
    ("B8 0B 00 00 8B 35", -0xb),
    ("8B 0D ?? ?? ?? ?? 85 C0 74 05 8B 50 30", -4),
    ("A1 ?? ?? ?? ?? 89 46 04 8B D6 E8", 1),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StablePatterns {
    pub base_addr: usize,
    pub play_time_addr: usize,
    pub chat_checker_ptr: usize,
    pub skin_data_addr: usize,
    pub settings_class_addr: usize,
    pub configuration_addr: usize,
    pub bindings_addr: usize,
    pub rulesets_addr: usize,
    pub can_run_slowly_addr: usize,
    pub status_ptr: usize,
    pub menu_mods_ptr: usize,
    pub get_audio_length_ptr: usize,
    pub user_profile_ptr: usize,
    pub raw_login_status_ptr: usize,
    pub spectating_user_ptr: usize,
    pub game_time_ptr: usize,
}

pub(super) fn shifted(base: usize, offset: i64) -> Result<usize, MemoryError> {
    if offset >= 0 {
        checked_address_add(base, offset as usize)
    } else {
        base.checked_sub(offset.unsigned_abs() as usize)
            .ok_or(MemoryError::AddressOverflow {
                base,
                offset: offset.unsigned_abs() as usize,
            })
    }
}

impl StablePatterns {
    pub fn resolve(process: &ProcessHandle) -> MemoryRead<Self> {
        let parsed = PATTERNS
            .iter()
            .map(|(s, _)| {
                Pattern::parse(s, false).map_err(|e| MemoryError::Backend {
                    operation: "parse stable pattern",
                    message: e.to_string(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let matches = process.scan_batch(&parsed)?;
        if matches.len() != PATTERNS.len() {
            return Ok(ReadStatus::NotReady);
        }
        let mut a = [0usize; 16];
        for found in matches {
            a[found.index] = shifted(found.address, PATTERNS[found.index].1)?;
        }
        if a.contains(&0) {
            return Ok(ReadStatus::NotReady);
        }
        Ok(ReadStatus::Ready(Self {
            base_addr: a[0],
            play_time_addr: a[1],
            chat_checker_ptr: a[2],
            skin_data_addr: a[3],
            settings_class_addr: a[4],
            configuration_addr: a[5],
            bindings_addr: a[6],
            rulesets_addr: a[7],
            can_run_slowly_addr: a[8],
            status_ptr: a[9],
            menu_mods_ptr: a[10],
            get_audio_length_ptr: a[11],
            user_profile_ptr: a[12],
            raw_login_status_ptr: a[13],
            spectating_user_ptr: a[14],
            game_time_ptr: a[15],
        }))
    }
}
