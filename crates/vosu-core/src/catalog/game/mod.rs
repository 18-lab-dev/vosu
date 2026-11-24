//! Catalog game detection and normal requested install offers (M6b G0;
//! `catalog-v2.md` "Game detection", `plugin-abi.md` §6.1, D18).
//!
//! [`GameOffers`] is a [`crate::plugin::DiscoveryObserver`]: the existing
//! discovery service and its existing process observations drive it; it adds no
//! poller, timer or thread. Every enabled, successfully fetched and SDK-validated
//! plugin entry that carries a `matcher` (game plugin, M6a Q7), is host/target
//! supported and whose id is not installed is compiled with the runtime
//! [`crate::plugin::CompiledMatcher`] exactly as installed plugins are (M6a Q6:
//! `comm_hints` never accept a process; `linux_name_contains` is a Linux-only
//! userspace matcher). Nothing is registered with the host's `MatcherEngine`, so
//! overlapping catalog alternatives stay allowed and installed overlap/256-entry
//! pre-filter refusal is unchanged. Following `catalog-v2.md` literally, a
//! process an installed, enabled game plugin already matched still produces
//! offers for matching uninstalled alternatives, flagged `installed_match`.
//!
//! Offers are keyed by `(source, plugin id, pid, start_time)`, bounded by
//! [`MAX_GAME_OFFERS`] and expose only [`GameOfferView`] (no path, argv or
//! command line). They are dropped on process exit or PID reuse, and every
//! catalog publication (source/installed generation or snapshot change) clears
//! them and advances the observer generation so discovery re-offers tracked
//! processes. Offers never download, attach, approve or trust anything:
//! [`GameOffers::accept`] only builds and registers a normal P0 plan whose
//! single request names the offer's exact source identity, so the add is
//! `reason: "requested"`; packages are fetched only by the executor after the
//! plan's own confirmation.
//!
//! Composition contract (root wiring is a later unit): register one shared
//! `Arc<GameOffers>` built for [`crate::plugin::host_platform`] with
//! `PluginHost::set_discovery_observer`, and call [`GameOffers::refresh`] after
//! every S0 candidate publication, source mutation (which clears S0's snapshot)
//! and installed-set change, passing the same installed generation P0 pins in
//! `Prerequisites::installed_generation`. Installed ids include bundled plugins.
mod candidates;
mod offers;
#[cfg(test)]
mod tests;

pub use offers::{
    GameOfferView, GameOffers, InstalledPlugins, MAX_GAME_OFFERS, OfferError, PlanInputs,
};
