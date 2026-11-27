//! Pure JavaScript-compatible wire mechanics for bundled legacy adapters.

pub mod filtered;
pub mod fixed;
pub mod json;
pub mod wire_fixtures;

/// Upstream's path join normalizes lazer's empty skin-folder string to a dot.
/// This is a display value, not authority to open the current directory.
pub fn skin_folder(lazer: bool, folder: Option<&str>) -> &str {
    match folder.unwrap_or_default() {
        "" if lazer => ".",
        folder => folder,
    }
}

/// Before the first object the progressive calculator can still be empty.
/// Legacy play payloads expose the chart's 100%-accuracy pp as their FC seed.
pub fn full_combo_pp(fc: f64, perfect: f64, status: i64, play_time: f64, first_object: i64) -> f64 {
    use vosu_osu_model::enums::GameState;

    if fc == 0.0 && status == i64::from(GameState::Play.value()) && play_time <= first_object as f64
    {
        perfect
    } else {
        fc
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vosu_osu_model::enums::GameState;

    #[test]
    fn only_empty_lazer_skin_is_normalized() {
        assert_eq!(skin_folder(true, None), ".");
        assert_eq!(skin_folder(true, Some("")), ".");
        assert_eq!(skin_folder(true, Some("chosen")), "chosen");
        assert_eq!(skin_folder(false, None), "");
        assert_eq!(skin_folder(false, Some("chosen")), "chosen");
    }

    #[test]
    fn full_combo_seed_is_only_used_before_progression_in_play() {
        let play = i64::from(GameState::Play.value());
        let menu = i64::from(GameState::SelectPlay.value());
        assert_eq!(full_combo_pp(0.0, 8.75, play, 100.0, 2_000), 8.75);
        assert_eq!(full_combo_pp(0.0, 8.75, play, 2_000.0, 2_000), 8.75);
        assert_eq!(full_combo_pp(0.0, 8.75, play, 2_001.0, 2_000), 0.0);
        assert_eq!(full_combo_pp(0.0, 8.75, menu, 100.0, 2_000), 0.0);
        assert_eq!(full_combo_pp(4.0, 8.75, play, 100.0, 2_000), 4.0);
    }
}
