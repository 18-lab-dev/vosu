//! The external osu! process-runtime fixture (M5 P0): the same
//! [`vosu_osu::plugin::OsuPlugin`] producer the bundled plugin registers
//! in-process, exported as a `cdylib` for a `runtime = "process"` package
//! (`tests/fixtures/plugin_host/osu/manifest.toml`).
//!
//! Development and test only (`required-features = ["test-harness"]`, which
//! release builds reject). The bundled manifest stays target-free and
//! in-process; only this separately staged library runs in a plugin host.
//!
//! When its data directory holds `parity.json`, the instance runs M5's
//! source-oracle publication ingress instead of capturing a client.

vosu_plugin_internal::export_plugin!(concurrent vosu_osu::fixtures::FixtureOrProducer);
