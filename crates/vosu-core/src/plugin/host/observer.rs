//! Generic, read-only discovery notification seam (M6b G0,
//! `docs/v2/plugin-abi.md` §6.1 "Catalog offers"). The *existing*
//! [`crate::plugin::DiscoveryService`] stays the only process poller: it
//! keeps calling [`PluginHost::process_appeared`]/
//! [`PluginHost::process_exited`], and this host forwards each observation to
//! at most one registered [`DiscoveryObserver`] after installed matchers had
//! their chance. An observer never attaches sessions, registers matchers,
//! loads code or changes D18 overlap/pre-filter refusal for installed plugins.
//!
//! An observer's extra pre-filter interest is a bounded list of hint names,
//! merged into [`PluginHost::prefilter`] through [`Prefilter::union`]: it can
//! never widen the filter or displace installed names (separate
//! `OBSERVER_PREFILTER_BUDGET`), and a change of its
//! [`DiscoveryObserver::generation`] advances
//! [`PluginHost::discovery_generation`], which makes discovery re-enumerate and
//! re-offer every tracked process exactly like an installed-set change.
//!
//! Ordering: the observer `Arc` is cloned under this slot's own lock and every
//! observer method runs with no host lock held, so an observer may take its own
//! locks freely. Observers must stay synchronous and bounded (no I/O).

use std::sync::Arc;

use super::{PluginHost, ProcessRecord};
use crate::plugin::matcher::{Prefilter, host_platform};

/// Receives discovery's existing process observations.
pub trait DiscoveryObserver: Send + Sync {
    /// One observed process occurrence. `claimed` is whether an installed,
    /// enabled plugin's matcher accepted it (D18 runtime rule).
    fn process_observed(&self, process: &ProcessRecord, claimed: bool);
    /// The pid's current occurrence exited (or was superseded by a reused pid).
    fn process_exited(&self, pid: u32);
    /// Extra enumeration-cost hint names; empty adds nothing. Observers can
    /// never request an unfiltered pre-filter, and excess names are ignored.
    fn prefilter_names(&self) -> Vec<String>;
    /// Monotonic interest revision; any change re-offers tracked processes.
    fn generation(&self) -> u64;
}

#[derive(Default)]
pub(super) struct ObserverSlot {
    observer: Option<Arc<dyn DiscoveryObserver>>,
    /// The observer generation last folded into `epoch`.
    seen: Option<u64>,
    /// Strictly increases on every observer replacement or generation change.
    epoch: u64,
}

impl PluginHost {
    /// Installs (or with `None`, removes) the single discovery observer.
    /// Replacing it always advances [`Self::discovery_generation`].
    pub fn set_discovery_observer(&self, observer: Option<Arc<dyn DiscoveryObserver>>) {
        let mut slot = self
            .discovery_observer
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        slot.observer = observer;
        slot.seen = None;
        slot.epoch += 1;
    }

    pub(super) fn discovery_observer(&self) -> Option<Arc<dyn DiscoveryObserver>> {
        self.discovery_observer
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .observer
            .clone()
    }

    /// Discovery's re-offer revision: the installed matcher generation plus a
    /// host-owned epoch that strictly increases whenever the observer or its
    /// interest generation changes. Both terms only grow, so any change is
    /// visible to discovery's equality check.
    #[must_use]
    pub fn discovery_generation(&self) -> u64 {
        let observer = self.discovery_observer();
        let observed = observer.as_ref().map(|observer| observer.generation());
        let epoch = {
            let mut slot = self
                .discovery_observer
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let current = slot
                .observer
                .as_ref()
                .zip(observer.as_ref())
                .is_some_and(|(slot, read)| Arc::ptr_eq(slot, read));
            if current && slot.seen != observed {
                slot.seen = observed;
                slot.epoch += 1;
            }
            slot.epoch
        };
        self.matcher_generation().wrapping_add(epoch)
    }

    /// The observer's extra pre-filter interest, unioned with the installed
    /// matchers' own pre-filter by [`Self::prefilter`].
    pub(super) fn observer_prefilter(&self, installed: Prefilter) -> Prefilter {
        match self.discovery_observer() {
            Some(observer) => installed.union(&observer.prefilter_names(), host_platform()),
            None => installed,
        }
    }

    pub(super) fn notify_observed(&self, process: &ProcessRecord, claimed: bool) {
        if let Some(observer) = self.discovery_observer() {
            observer.process_observed(process, claimed);
        }
    }

    pub(super) fn notify_exited(&self, pid: u32) {
        if let Some(observer) = self.discovery_observer() {
            observer.process_exited(pid);
        }
    }
}
