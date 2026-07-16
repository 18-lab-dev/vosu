//! Counter discovery, settings, downloads, and lifecycle.

mod model;
mod service;
mod settings;
#[cfg(test)]
mod tests;

pub use model::{
    CompactValues, Counter, CounterSetting, DownloadRequest, LifecycleReply, SettingInput,
    SettingType,
};
pub use service::{
    CounterDownloader, CounterError, CounterOpener, CounterService, HardenedCounterDownloader,
    ReloadableCounterService, SystemOpener, parse_metadata,
};
pub use settings::{SettingsError, SettingsStore, validate_component};
