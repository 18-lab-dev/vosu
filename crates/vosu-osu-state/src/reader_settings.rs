//! Narrow, `Copy` view of the configuration fields that capture scheduling
//! and the reducer actually read.
//!
//! Defined here (not `vosu_core::config::Config` itself) so that
//! `vosu-osu-state` does not depend on `vosu-core`
//! (`scripts/crate-boundaries.txt`: no such edge exists). The root crate's
//! `runtime` builds a value from the live `Config` and forwards it on every change.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReaderSettings {
    pub calculate_pp: bool,
    /// Accurate extended hit-error collection requested by state-bus subscribers.
    pub collect_hit_errors: bool,
    pub poll_rate: f64,
    pub precise_data_poll_rate: f64,
    pub show_mp_commands: bool,
    pub read_mania_scroll_speed: bool,
}

impl Default for ReaderSettings {
    /// Mirrors `crate::config::Config::default()` for these fields.
    fn default() -> Self {
        Self {
            calculate_pp: true,
            collect_hit_errors: false,
            poll_rate: 150.0,
            precise_data_poll_rate: 10.0,
            show_mp_commands: false,
            read_mania_scroll_speed: true,
        }
    }
}
