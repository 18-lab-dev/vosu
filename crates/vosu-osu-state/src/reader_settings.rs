//! Narrow, `Copy` view of the configuration fields that capture scheduling
//! and the reducer actually read.
//!
//! Defined here (not `crate::config::Config` itself) so that `state` does
//! not depend on `config` (module-boundaries.txt: breaking the
//! `state -> config` edge). `crate::runtime` builds a value from the live
//! `Config` and forwards it on every change.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReaderSettings {
    pub calculate_pp: bool,
    pub enable_key_overlay: bool,
    pub enable_mania_key_hit_errors: bool,
    pub enable_hit_offsets: bool,
    pub poll_rate: f64,
    pub precise_data_poll_rate: f64,
    pub v2_subscription_debounce: f64,
    pub show_mp_commands: bool,
    pub read_mania_scroll_speed: bool,
}

impl Default for ReaderSettings {
    /// Mirrors `crate::config::Config::default()` for these fields.
    fn default() -> Self {
        Self {
            calculate_pp: true,
            enable_key_overlay: true,
            enable_mania_key_hit_errors: false,
            enable_hit_offsets: false,
            poll_rate: 150.0,
            precise_data_poll_rate: 10.0,
            v2_subscription_debounce: 1.0,
            show_mp_commands: false,
            read_mania_scroll_speed: true,
        }
    }
}
