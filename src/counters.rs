//! Counter discovery, settings, downloads, and lifecycle.

mod model;
mod names;
mod service;
mod settings;
#[cfg(test)]
mod tests;

pub use model::{
    CompactValues, Counter, CounterSetting, DownloadRequest, LifecycleReply, SettingInput,
    SettingType,
};
pub use names::{OverlayName, OverlayNames};
pub use service::{
    CounterDownloader, CounterError, CounterOpener, CounterService, HardenedCounterDownloader,
    ReloadableCounterService, SystemOpener, parse_metadata,
};
pub use settings::{
    Profiles, SettingsError, SettingsStore, qualified_counter, split_counter, validate_component,
};
pub(crate) use settings::{coerce_checkbox, coerce_number, normalize_settings};
