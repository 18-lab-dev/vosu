//! Normal development-plugin ABI export, never part of the bundled registry.
// Compile the fixture modules directly rather than linking the testkit rlib:
// that rlib already exports the legacy echo plugin's ABI entry symbol.
#[path = "../src/actions.rs"]
pub mod actions;
#[path = "../src/fixture.rs"]
pub mod fixture;
#[path = "../src/resources.rs"]
pub mod resources;
vosu_plugin_api::export_plugin!(fixture::ApiFixturePlugin);
