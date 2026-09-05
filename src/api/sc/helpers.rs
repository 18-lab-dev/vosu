use super::*;

pub(super) fn current_mods(regular: &RegularState, status: i64) -> &CalculateMods {
    match status {
        PLAY => &regular.gameplay.mods,
        RESULT_SCREEN => &regular.result_screen.raw.mods,
        _ => &regular.global.menu_mods,
    }
}

pub(super) fn current_mode(regular: &RegularState, status: i64) -> Option<i64> {
    match status {
        PLAY => Some(regular.gameplay.mode),
        RESULT_SCREEN => Some(regular.result_screen.raw.mode),
        _ => regular.menu.gamemode,
    }
}

pub(super) fn round4(value: f64) -> f64 {
    fix_decimals_to(value, 4).expect("four decimals are valid")
}

pub(super) fn country_name(value: i64) -> String {
    i32::try_from(value)
        .ok()
        .and_then(CountryCodes::from_value)
        .map(|country| format!("{country:?}").to_uppercase())
        .unwrap_or_default()
}

pub(super) fn ruleset_name(value: Option<i64>) -> Option<String> {
    value
        .and_then(|value| i32::try_from(value).ok())
        .and_then(Rulesets::from_value)
        .map(|mode| format!("{mode:?}").to_lowercase())
}

pub(super) fn bpm_text(minimum: f64, maximum: f64, common: f64) -> String {
    if minimum == maximum {
        js_number_text(maximum)
    } else {
        format!(
            "{}-{} ({})",
            js_number_text(js_round(minimum)),
            js_number_text(js_round(maximum)),
            js_number_text(js_round(common))
        )
    }
}

pub(super) fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}

pub(super) fn js_number_text(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value == f64::INFINITY {
        "Infinity".to_owned()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_owned()
    } else {
        js_json_number(value)
    }
}

pub(super) fn js_undefined(value: Option<&str>) -> &str {
    value.unwrap_or("undefined")
}

pub(super) fn grade_number(grade: &str) -> Option<i64> {
    match grade {
        "XH" => Some(0),
        "X" => Some(1),
        "SH" => Some(2),
        "S" => Some(3),
        "A" => Some(4),
        "B" => Some(5),
        "C" => Some(6),
        "D" => Some(7),
        "None" => Some(8),
        _ => None,
    }
}

pub(super) fn format_milliseconds(milliseconds: f64) -> String {
    let hours = pad_start_2(js_number_text((milliseconds / 3_600_000.0).floor()));
    let minutes = pad_start_2(js_number_text(
        ((milliseconds % 3_600_000.0) / 60_000.0).floor(),
    ));
    let seconds = pad_start_2(js_number_text(
        ((milliseconds % 60_000.0) / 1_000.0).floor(),
    ));
    let millis = js_number_text(milliseconds % 1_000.0);
    format!("{hours}:{minutes}:{seconds}.{millis}")
}

pub(super) fn map_time_projection(
    full: f64,
    first_object: i64,
    play_time: f64,
) -> (f64, String, f64) {
    (
        full,
        format_milliseconds(full - play_time),
        full - first_object as f64,
    )
}

pub(super) fn pad_start_2(value: String) -> String {
    if value.len() < 2 {
        format!("0{value}")
    } else {
        value
    }
}

pub(super) fn path_join(folder: Option<&str>, file: Option<&str>) -> String {
    safe_join([folder.unwrap_or_default(), file.unwrap_or_default()])
}

pub(super) fn map_strains(x_axis: &[f64], data: Option<&[f64]>) -> IndexMap<String, f64> {
    let data = data.unwrap_or_default();
    let mut insertion = IndexMap::new();
    for (index, time) in x_axis.iter().enumerate() {
        let value = data.get(index).copied().unwrap_or(f64::NAN);
        insertion.insert(
            js_number_text(*time),
            if value <= 0.0 { 0.0 } else { value },
        );
    }
    let mut entries: Vec<_> = insertion.into_iter().enumerate().collect();
    entries.sort_by(|(left_pos, (left, _)), (right_pos, (right, _))| {
        match (array_index(left), array_index(right)) {
            (Some(left), Some(right)) => left.cmp(&right),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => left_pos.cmp(right_pos),
        }
    });
    entries.into_iter().map(|(_, entry)| entry).collect()
}

pub(super) fn array_index(key: &str) -> Option<u32> {
    let value = key.parse::<u32>().ok()?;
    (value != u32::MAX && value.to_string() == key).then_some(value)
}

pub(super) fn encode_json(value: &impl Serialize) -> Result<String, ApiError> {
    String::from_utf8(encode_json_bytes(value)?)
        .map_err(|error| ApiError::serialization(error.to_string()))
}

pub(super) fn encode_json_bytes(value: &impl Serialize) -> Result<Vec<u8>, ApiError> {
    crate::api::json::encode(value, 0)
}

#[derive(Serialize)]
pub(super) struct KeyOverlayWire {
    #[serde(rename = "Enabled")]
    enabled: bool,
    #[serde(rename = "K1Pressed")]
    k1_pressed: bool,
    #[serde(rename = "K1Count")]
    k1_count: i64,
    #[serde(rename = "K2Pressed")]
    k2_pressed: bool,
    #[serde(rename = "K2Count")]
    k2_count: i64,
    #[serde(rename = "M1Pressed")]
    m1_pressed: bool,
    #[serde(rename = "M1Count")]
    m1_count: i64,
    #[serde(rename = "M2Pressed")]
    m2_pressed: bool,
    #[serde(rename = "M2Count")]
    m2_count: i64,
}
impl KeyOverlayWire {
    pub(super) fn new(enabled: bool, keys: &[KeyOverlayButton]) -> Self {
        let key = |index: usize| keys.get(index);
        Self {
            enabled,
            k1_pressed: key(0).is_some_and(|key| key.is_pressed),
            k1_count: key(0).map_or(0, |key| key.count),
            k2_pressed: key(1).is_some_and(|key| key.is_pressed),
            k2_count: key(1).map_or(0, |key| key.count),
            m1_pressed: key(2).is_some_and(|key| key.is_pressed),
            m1_count: key(2).map_or(0, |key| key.count),
            m2_pressed: key(3).is_some_and(|key| key.is_pressed),
            m2_count: key(3).map_or(0, |key| key.count),
        }
    }
}

#[derive(Serialize)]
pub(super) struct ModsWire {
    #[serde(rename = "ModsXor1")]
    xor1: i64,
    #[serde(rename = "ModsXor2")]
    xor2: i64,
    #[serde(rename = "Value")]
    value: i64,
}

#[derive(Serialize)]
pub(super) struct LeaderboardMainWire {
    #[serde(rename = "IsLeaderboardVisible")]
    visible: bool,
    #[serde(rename = "Username")]
    username: String,
    #[serde(rename = "Score")]
    score: f64,
    #[serde(rename = "Combo")]
    combo: i64,
    #[serde(rename = "MaxCombo")]
    max_combo: i64,
    #[serde(rename = "Mods")]
    mods: ModsWire,
    #[serde(rename = "Hit300")]
    hit300: i64,
    #[serde(rename = "Hit100")]
    hit100: i64,
    #[serde(rename = "Hit50")]
    hit50: i64,
    #[serde(rename = "HitMiss")]
    hit_miss: i64,
    #[serde(rename = "Team")]
    team: i64,
    #[serde(rename = "Position")]
    position: i64,
    #[serde(rename = "IsPassing")]
    is_passing: bool,
}
impl LeaderboardMainWire {
    pub(super) fn from_state(visible: bool, player: &LeaderboardPlayerState) -> Self {
        Self {
            visible,
            username: player.name.clone(),
            score: player.score,
            combo: player.combo,
            max_combo: player.max_combo,
            mods: ModsWire {
                xor1: -1,
                xor2: -1,
                value: player.mods.number,
            },
            hit300: player.statistics.great,
            hit100: player.statistics.ok,
            hit50: player.statistics.meh,
            hit_miss: player.statistics.miss,
            team: player.team,
            position: player.position,
            is_passing: player.is_passing,
        }
    }
}

#[derive(Serialize)]
pub(super) struct LeaderboardPlayerWire {
    #[serde(rename = "Username")]
    username: String,
    #[serde(rename = "Score")]
    score: f64,
    #[serde(rename = "Combo")]
    combo: i64,
    #[serde(rename = "MaxCombo")]
    max_combo: i64,
    #[serde(rename = "Mods")]
    mods: ModsWire,
    #[serde(rename = "Hit300")]
    hit300: i64,
    #[serde(rename = "Hit100")]
    hit100: i64,
    #[serde(rename = "Hit50")]
    hit50: i64,
    #[serde(rename = "HitMiss")]
    hit_miss: i64,
    #[serde(rename = "Team")]
    team: i64,
    #[serde(rename = "Position")]
    position: i64,
    #[serde(rename = "IsPassing")]
    is_passing: bool,
}
impl LeaderboardPlayerWire {
    pub(super) fn from_player(player: &LeaderboardPlayer) -> Self {
        Self {
            username: player.name.clone(),
            score: player.score,
            combo: player.combo,
            max_combo: player.max_combo,
            mods: ModsWire {
                xor1: -1,
                xor2: -1,
                value: player.mods.number,
            },
            hit300: player.statistics.great,
            hit100: player.statistics.ok,
            hit50: player.statistics.meh,
            hit_miss: player.statistics.miss,
            team: player.team,
            position: player.position,
            is_passing: player.is_passing,
        }
    }
}
