//! Per-field sampling-lane metadata and the demand-union rule (MJ,
//! ss:386-442): 1.x's four-flag `SubscriptionDemand`
//! (`crates/vosu-osu-state/src/context.rs:215-235`), generalized to a
//! path-keyed table the walker fills from `x-sampling-lane` annotations.

use std::sync::Arc;

/// Union of every sampling lane a subscription selector can demand.
/// `effective_always` fields raise demand regardless of game status;
/// `effective_gameplay` fields raise it only in `play`/`pause` (MC).
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct LaneBits {
    pub regular_always: bool,
    pub precise_always: bool,
    pub regular_gameplay: bool,
    pub precise_gameplay: bool,
}

impl LaneBits {
    /// Unions two selectors' lane bits (MJ: a parent selector's demand is
    /// the union of its descendants' lanes).
    pub fn union(self, other: Self) -> Self {
        Self {
            regular_always: self.regular_always || other.regular_always,
            precise_always: self.precise_always || other.precise_always,
            regular_gameplay: self.regular_gameplay || other.regular_gameplay,
            precise_gameplay: self.precise_gameplay || other.precise_gameplay,
        }
    }

    /// Effective (regular, precise) demand: `always | (gameplay && in_play)`
    /// (MJ). `in_play` is `vosu.status.status ∈ {play, pause}` (MC).
    pub fn effective(self, in_play: bool) -> (bool, bool) {
        (
            self.regular_always || (self.regular_gameplay && in_play),
            self.precise_always || (self.precise_gameplay && in_play),
        )
    }

    /// Parses one `x-sampling-lane` annotation's `lane`/`activity` pair
    /// (ss:401-411). `None` for any value outside the declared enums.
    pub(super) fn from_annotation(lane: &str, activity: &str) -> Option<Self> {
        let (regular, precise) = match lane {
            "regular" => (true, false),
            "precise" => (false, true),
            "both" => (true, true),
            _ => return None,
        };
        let mut bits = Self::default();
        match activity {
            "always" => {
                bits.regular_always = regular;
                bits.precise_always = precise;
            }
            "gameplay" => {
                bits.regular_gameplay = regular;
                bits.precise_gameplay = precise;
            }
            _ => return None,
        }
        Some(bits)
    }
}

/// Whether an annotated schema `path` and a query `selector` are
/// prefix-compatible in either direction (MJ, 1.x's `overlaps`,
/// `src/server/v2_subscription.rs:269`): one is a prefix of the other once
/// an array `items` wildcard (`"*"`) on either side matches any single
/// token. An empty `selector` (the namespace root) overlaps every path.
pub(super) fn paths_overlap(path: &[Arc<str>], selector: &[&str]) -> bool {
    path.iter()
        .zip(selector.iter())
        .all(|(p, s)| p.as_ref() == "*" || *s == "*" || p.as_ref() == *s)
}
