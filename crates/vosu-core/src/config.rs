//! Typed TOML-backed server configuration and live persistence.

mod document;
mod io;
mod migration;
mod network;
mod parser;
mod reload;
mod schema;
mod store;

pub use document::ConfigDocument;
pub use io::{AtomicReplace, ConfigStoreOptions, SystemAtomicReplace};
pub use schema::{
    BINDING_ORDER, Config, ConfigDefault, ConfigItem, ConfigKey, ConfigSchema, DEFAULT_SCHEMA,
    NEWLINE_INSERTIONS,
};
pub use store::{ConfigChanged, ConfigStore};

#[cfg(test)]
mod tests;
