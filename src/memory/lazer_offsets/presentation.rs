use serde::Deserialize;

/// Enum literals measured from the same client assemblies as its field offsets.
/// Numeric OsuSetting keys have shifted between releases; never pin them in the reader.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct LocalConfigKeys {
    pub release_stream: i32,
    pub score_display_mode: i32,
    pub beatmap_leaderboard_sort_mode: i32,
    pub song_select_sorting_mode: i32,
    pub song_select_group_mode: i32,
    pub gameplay_leaderboard: i32,
    pub replay_settings_overlay: i32,
    pub dim_level: i32,
    pub blur_level: i32,
    pub audio_offset: i32,
    pub volume_inactive: i32,
    pub menu_cursor_size: i32,
    pub gameplay_cursor_size: i32,
    pub auto_cursor_size: i32,
    pub show_storyboard: i32,
    pub mouse_disable_buttons: i32,
    pub mouse_disable_wheel: i32,
    pub beatmap_hitsounds: i32,
    pub beatmap_skins: i32,
    pub beatmap_colours: i32,
    pub hit_lighting: i32,
    pub scaling: i32,
    pub scaling_position_x: i32,
    pub scaling_position_y: i32,
    pub scaling_size_x: i32,
    pub scaling_size_y: i32,
}

impl LocalConfigKeys {
    /// Keys and bindable value widths in the local decoder's stable field order.
    pub const fn fields(&self) -> [(i32, usize); 26] {
        [
            (self.release_stream, 4),
            (self.score_display_mode, 4),
            (self.beatmap_leaderboard_sort_mode, 4),
            (self.song_select_sorting_mode, 4),
            (self.song_select_group_mode, 4),
            (self.gameplay_leaderboard, 1),
            (self.replay_settings_overlay, 1),
            (self.dim_level, 8),
            (self.blur_level, 8),
            (self.audio_offset, 8),
            (self.volume_inactive, 8),
            (self.menu_cursor_size, 4),
            (self.gameplay_cursor_size, 4),
            (self.auto_cursor_size, 1),
            (self.show_storyboard, 1),
            (self.mouse_disable_buttons, 1),
            (self.mouse_disable_wheel, 1),
            (self.beatmap_hitsounds, 1),
            (self.beatmap_skins, 1),
            (self.beatmap_colours, 1),
            (self.hit_lighting, 1),
            (self.scaling, 4),
            (self.scaling_position_x, 4),
            (self.scaling_position_y, 4),
            (self.scaling_size_x, 4),
            (self.scaling_size_y, 4),
        ]
    }
}
