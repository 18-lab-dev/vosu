//! Launch-argument-derived settings for an osu! instance, moved from
//! `src/instance.rs` (F8). Pure data: argv decoding lives in
//! `vosu_osu_reader::discovery::instance_settings`, which builds this type.

use std::ffi::OsString;

#[derive(Debug, Clone, PartialEq)]
pub struct InstanceSettings {
    pub tournament_spectator: bool,
    pub tournament_ipc_id: Option<f64>,
    pub devserver_endpoint: Option<OsString>,
}
