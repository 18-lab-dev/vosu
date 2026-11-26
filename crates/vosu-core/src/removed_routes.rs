//! The D20 removed-route recorder (adapter-surfaces.md A4.5, A7.4, A7.5).
//!
//! Core's transport records an admitted request to a removed route under a
//! fixed feature, attributed to an already validated installed overlay or to
//! that feature's unknown bucket. Nothing here sees a URL, query, peer,
//! Referer, Origin or body. The map is bounded (128 known overlays with LRU
//! eviction, one unknown bucket per feature), expires lazily after 24 hours
//! without a request and is read only through [`RemovedRoutes::snapshot`].
//! One recorder lives for the daemon process; restart clears it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// A removed request family, reported by its fixed id.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RemovedFeature {
    /// `/websocket/v2/subscribe`.
    SelectedV2Feed,
    /// `/files/skin/`.
    LegacySkinIndex,
    /// `/api/ingame` (A7.5).
    IngamePage,
}

impl RemovedFeature {
    pub const ALL: [Self; 3] = [
        Self::SelectedV2Feed,
        Self::LegacySkinIndex,
        Self::IngamePage,
    ];

    /// The `DashboardStatus.removed_routes[].feature` id.
    pub const fn id(self) -> &'static str {
        match self {
            Self::SelectedV2Feed => "selected-v2-feed",
            Self::LegacySkinIndex => "legacy-skin-index",
            Self::IngamePage => "ingame-page",
        }
    }
}

/// Known overlay identities retained at once; the least recently seen goes.
pub const MAX_KNOWN_OVERLAYS: usize = 128;
/// Inactivity after which a notice expires.
pub const TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Every known identity under every feature, plus one unknown bucket each.
pub const MAX_NOTICES: usize = (MAX_KNOWN_OVERLAYS + 1) * RemovedFeature::ALL.len();
const _: () = assert!(MAX_NOTICES == 387);

/// The A7.5 warning, logged once per process on the first `/api/ingame` request.
pub const INGAME_WARNING: &str =
    "/api/ingame was removed in vosu 2.0; vosulay is the in-game overlay path";

/// Safe advisory data: a validated installed overlay id or `None`, never a URL,
/// credential or claimed name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemovedNotice {
    pub overlay: Option<String>,
    pub feature: RemovedFeature,
    pub first_seen: Instant,
    pub last_seen: Instant,
    pub attempts: u64,
}

#[derive(Debug, Default)]
pub struct RemovedRoutes {
    notices: Mutex<Vec<RemovedNotice>>,
    ingame_warned: AtomicBool,
}

impl RemovedRoutes {
    fn expire(notices: &mut Vec<RemovedNotice>, now: Instant) {
        notices.retain(|notice| now.saturating_duration_since(notice.last_seen) < TTL);
    }

    /// Every admitted request to a removed route, recorded or not (a
    /// dashboard preview is not): the first `/api/ingame` one logs the A7.5
    /// warning, once per recorder, and nothing else is logged.
    pub fn admitted(&self, feature: RemovedFeature) {
        if feature == RemovedFeature::IngamePage && self.first_ingame_request() {
            tracing::warn!("{INGAME_WARNING}");
        }
    }

    /// Records one admitted request. `overlay` must already be validated as a
    /// currently installed overlay; anything else is `None` (unknown).
    pub fn record(&self, overlay: Option<String>, feature: RemovedFeature, now: Instant) {
        let mut notices = self.notices.lock().unwrap_or_else(PoisonError::into_inner);
        Self::expire(&mut notices, now);
        if let Some(notice) = notices
            .iter_mut()
            .find(|notice| notice.overlay == overlay && notice.feature == feature)
        {
            notice.last_seen = now;
            notice.attempts = notice.attempts.saturating_add(1);
            return;
        }
        if overlay.is_some() && !notices.iter().any(|notice| notice.overlay == overlay) {
            let mut known: Vec<(&str, Instant)> = Vec::new();
            for notice in notices.iter() {
                let Some(id) = notice.overlay.as_deref() else {
                    continue;
                };
                match known.iter_mut().find(|(known, _)| *known == id) {
                    Some((_, seen)) => *seen = (*seen).max(notice.last_seen),
                    None => known.push((id, notice.last_seen)),
                }
            }
            if known.len() >= MAX_KNOWN_OVERLAYS
                && let Some(oldest) = known
                    .iter()
                    .min_by_key(|(_, seen)| *seen)
                    .map(|(id, _)| (*id).to_owned())
            {
                notices.retain(|notice| notice.overlay.as_deref() != Some(oldest.as_str()));
            }
        }
        notices.push(RemovedNotice {
            overlay,
            feature,
            first_seen: now,
            last_seen: now,
            attempts: 1,
        });
    }

    /// True exactly once per recorder, under any concurrency.
    fn first_ingame_request(&self) -> bool {
        !self.ingame_warned.swap(true, Ordering::AcqRel)
    }

    /// The bounded read: expiry first, then each known identity revalidated by
    /// `installed` (outside the lock); a stale identity is dropped, never shown.
    pub fn snapshot(&self, now: Instant, installed: impl Fn(&str) -> bool) -> Vec<RemovedNotice> {
        let snapshot = {
            let mut notices = self.notices.lock().unwrap_or_else(PoisonError::into_inner);
            Self::expire(&mut notices, now);
            notices.clone()
        };
        let mut snapshot: Vec<_> = snapshot
            .into_iter()
            .filter(|notice| notice.overlay.as_deref().is_none_or(&installed))
            .collect();
        snapshot.truncate(MAX_NOTICES);
        snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::RemovedFeature::{IngamePage, LegacySkinIndex, SelectedV2Feed};
    use super::*;

    #[test]
    fn bounded_deduplicated_unknown_cannot_evict_known_and_expiry_is_lazy() {
        let notices = RemovedRoutes::default();
        let now = Instant::now();
        for index in 0..129 {
            notices.record(
                Some(format!("overlay-{index}")),
                SelectedV2Feed,
                now + Duration::from_secs(index),
            );
        }
        for _ in 0..1000 {
            notices.record(None, SelectedV2Feed, now);
            notices.record(None, LegacySkinIndex, now);
        }
        let snapshot = notices.snapshot(now, |_| true);
        assert_eq!(snapshot.len(), 130);
        assert!(
            !snapshot
                .iter()
                .any(|notice| notice.overlay.as_deref() == Some("overlay-0"))
        );
        assert_eq!(
            snapshot
                .iter()
                .filter(|notice| notice.overlay.is_none())
                .count(),
            2
        );
        assert!(
            snapshot
                .iter()
                .filter(|notice| notice.overlay.is_none())
                .all(|notice| notice.attempts == 1000)
        );
        assert_eq!(notices.snapshot(now, |_| false).len(), 2);
        assert!(
            notices
                .snapshot(now + TTL + Duration::from_secs(129), |_| true)
                .is_empty()
        );
    }

    #[test]
    fn every_identity_under_every_feature_fills_exactly_the_cap() {
        let notices = RemovedRoutes::default();
        let now = Instant::now();
        for round in 0..2 {
            for index in 0..=MAX_KNOWN_OVERLAYS {
                for feature in RemovedFeature::ALL {
                    notices.record(Some(format!("overlay-{round}-{index}")), feature, now);
                    notices.record(None, feature, now);
                }
            }
        }
        let snapshot = notices.snapshot(now, |_| true);
        assert_eq!(snapshot.len(), MAX_NOTICES);
        assert_eq!(MAX_NOTICES, 387);
        let known: std::collections::HashSet<_> = snapshot
            .iter()
            .filter_map(|notice| notice.overlay.as_deref())
            .collect();
        assert_eq!(known.len(), MAX_KNOWN_OVERLAYS);
    }

    #[test]
    fn the_least_recently_seen_identity_is_evicted_not_the_first_recorded() {
        let notices = RemovedRoutes::default();
        let now = Instant::now();
        for index in 0..MAX_KNOWN_OVERLAYS as u64 {
            notices.record(
                Some(format!("overlay-{index}")),
                IngamePage,
                now + Duration::from_secs(index),
            );
        }
        // A later request under another feature refreshes overlay-0.
        notices.record(
            Some("overlay-0".into()),
            SelectedV2Feed,
            now + Duration::from_secs(500),
        );
        notices.record(
            Some("overlay-new".into()),
            IngamePage,
            now + Duration::from_secs(501),
        );
        let snapshot = notices.snapshot(now + Duration::from_secs(501), |_| true);
        let ids: Vec<_> = snapshot
            .iter()
            .filter_map(|notice| notice.overlay.as_deref())
            .collect();
        assert!(ids.contains(&"overlay-0"));
        assert!(!ids.contains(&"overlay-1"));
        assert!(ids.contains(&"overlay-new"));
    }

    #[test]
    fn ingame_warning_gate_opens_once_under_concurrency() {
        let notices = RemovedRoutes::default();
        let opened = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..16 {
                scope.spawn(|| {
                    if notices.first_ingame_request() {
                        opened.fetch_add(1, Ordering::Relaxed);
                    }
                });
            }
        });
        assert_eq!(opened.into_inner(), 1);
        assert!(!notices.first_ingame_request());
    }

    #[test]
    fn counts_saturate() {
        let notices = RemovedRoutes::default();
        let now = Instant::now();
        notices.record(None, IngamePage, now);
        notices.notices.lock().unwrap()[0].attempts = u64::MAX;
        notices.record(None, IngamePage, now);
        assert_eq!(notices.snapshot(now, |_| true)[0].attempts, u64::MAX);
    }
}
