//! The command protocol between a production bench (core, this bench
//! process) and its plugin (`examples/brokered_reads_plugin.rs`) running in
//! a real `vosu plugin-host` child. Shared verbatim by both sides.
//!
//! Core writes `command-<n>` into the instance's data directory and rings
//! the doorbell: a `DEMAND_CHANGED` whose `handle` is [`DOORBELL`]` | n`.
//! The plugin's own worker thread (never its dispatch thread, which may not
//! submit `SYNC` plans) runs the command and publishes `reply-<n>` by
//! rename. Commands and replies are `key=value` lines; a reply carrying an
//! `error` key fails the bench. The protocol itself is never timed: the
//! plugin times its own round trips, exactly as every brokered variant's
//! submitting side does.

use std::path::{Path, PathBuf};

/// The doorbell's high bits; the low 32 bits carry the command number.
pub const DOORBELL: u64 = 0xB5B5_0000_0000_0000;
pub const DOORBELL_MASK: u64 = 0xFFFF_FFFF_0000_0000;

pub fn command_path(dir: &Path, n: u64) -> PathBuf {
    dir.join(format!("command-{n}"))
}

pub fn reply_path(dir: &Path, n: u64) -> PathBuf {
    dir.join(format!("reply-{n}"))
}

/// Written by the plugin when core attaches a session for `pid`.
pub fn attached_path(dir: &Path, pid: u32) -> PathBuf {
    dir.join(format!("attached-{pid}"))
}

/// Ordered `key=value` lines.
#[derive(Debug, Default, Clone)]
pub struct Fields {
    entries: Vec<(String, String)>,
}

impl Fields {
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with(mut self, key: &str, value: impl std::fmt::Display) -> Self {
        self.set(key, value);
        self
    }

    pub fn set(&mut self, key: &str, value: impl std::fmt::Display) {
        self.entries.push((key.to_owned(), value.to_string()));
    }

    pub fn parse(text: &str) -> Self {
        Self {
            entries: text
                .lines()
                .filter_map(|line| line.split_once('='))
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect(),
        }
    }

    pub fn render(&self) -> String {
        self.entries
            .iter()
            .map(|(key, value)| format!("{key}={value}\n"))
            .collect()
    }

    pub fn optional(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    pub fn text(&self, key: &str) -> &str {
        self.optional(key)
            .unwrap_or_else(|| panic!("{key} missing from {self:?}"))
    }

    pub fn number(&self, key: &str) -> u64 {
        let text = self.text(key);
        text.parse()
            .unwrap_or_else(|_| panic!("{key}={text} is not an integer"))
    }

    pub fn float(&self, key: &str) -> f64 {
        let text = self.text(key);
        text.parse()
            .unwrap_or_else(|_| panic!("{key}={text} is not a number"))
    }
}
