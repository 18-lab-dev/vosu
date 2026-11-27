//! Reviewed evidence gaps expire when replay no longer reports the exact finding.
use super::{
    compare::{self, Detail, Mismatch},
    divergence::Rule,
    fixture::{self, Fixture, Platform, Provenance},
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet, btree_map},
    path::Path,
};

pub(super) const FILE: &str = "pending-recapture.json";
const FORMAT: &str = "vosu-golden-pending-recapture-v1";
const DECISION: &str = "run5-replay-triage \"Main decision\"";

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Reason {
    S2RawClockPairAbsent,
    FftPreBarrierHistoryAbsent,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Entry {
    platform: String,
    case: String,
    part: String,
    frame: Option<usize>,
    pointer: String,
    reason: Reason,
    decision: String,
    review_id: String,
}

type Key<'a> = (&'a str, &'a str, Option<usize>, &'a str);

impl Entry {
    fn key(&self) -> Key<'_> {
        (&self.case, &self.part, self.frame, &self.pointer)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    format: String,
    entries: Vec<Entry>,
}

pub(super) fn load(
    root: &Path,
    files: &BTreeSet<String>,
    platforms: &[Platform],
    provenance: Provenance,
) -> Result<Vec<Entry>, String> {
    if !files.contains(FILE) {
        return Ok(Vec::new());
    }
    let wire: Wire = serde_json::from_value(fixture::json(root, FILE)?)
        .map_err(|error| format!("{FILE}: {error}"))?;
    if wire.format != FORMAT || wire.entries.is_empty() || provenance != Provenance::Captured {
        return Err(format!(
            "{FILE}: expected nonempty reviewed captured evidence gaps"
        ));
    }
    let mut seen = BTreeSet::new();
    for entry in &wire.entries {
        let valid_case = platforms.iter().any(|p| {
            p.run5 && p.name == entry.platform && p.cases.iter().any(|c| c.id == entry.case)
        });
        let valid_part = matches!(
            (entry.part.as_str(), entry.frame),
            ("body", None) | ("ws.frame", Some(_))
        );
        let valid_pointer = matches!(
            (entry.reason, entry.pointer.as_str()),
            (Reason::S2RawClockPairAbsent, "/game/paused")
                | (
                    Reason::FftPreBarrierHistoryAbsent,
                    "/menu/mainMenu/bassDensity" | "/settings/bassDensity"
                )
        );
        let valid_review = !entry.review_id.is_empty()
            && entry.review_id.len() <= 128
            && entry
                .review_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:#-".contains(&b));
        if !valid_case
            || !valid_part
            || !valid_pointer
            || entry.decision != DECISION
            || !valid_review
        {
            return Err(format!("{FILE}: invalid entry {entry:?}"));
        }
        if !seen.insert((entry.platform.as_str(), entry.key())) {
            return Err(format!("{FILE}: duplicate entry {entry:?}"));
        }
    }
    Ok(wire.entries)
}

/// Match the complete set, not just a case or a subset of its differences.
fn check_platform(platform: &str, entries: &[Entry], mismatches: &[Mismatch]) -> Vec<String> {
    let mut remaining: BTreeMap<_, _> = entries
        .iter()
        .filter(|entry| entry.platform == platform)
        .map(|entry| (entry.key(), entry))
        .collect();
    let mut failures = Vec::new();
    for mismatch in mismatches {
        let Mismatch { case, part, detail } = mismatch;
        match detail {
            Detail::Other(text) => failures.push(format!("{platform} {case} {part}: {text}")),
            Detail::Json { frame, deltas } => {
                for delta in deltas {
                    let key: Key<'_> = (case, part, *frame, &delta.pointer);
                    match remaining.entry(key) {
                        btree_map::Entry::Occupied(entry)
                            if Some(entry.get().reason) == delta.reason =>
                        {
                            entry.remove();
                        }
                        _ => failures.push(format!(
                            "{platform} {case} {part}: unlisted JSON difference at {:?} (frame {frame:?}, evidence gap {:?})",
                            delta.pointer, delta.reason
                        )),
                    }
                }
            }
        }
    }
    failures.extend(remaining.values().map(|entry| {
        format!("{platform} {} {}: stale {FILE} entry at {} (frame {:?}); remove it if replay now matches", entry.case, entry.part, entry.pointer, entry.frame)
    }));
    failures
}

/// Replay every platform. Any unlisted observation or unused divergence fails.
pub(super) fn run(fixture: &Fixture, rules: &[Rule]) -> Result<(), String> {
    let mut used = BTreeSet::new();
    let mut failures = Vec::new();
    for platform in &fixture.platforms {
        let observed = compare::observe_with_rules(platform, rules)?;
        let report = compare::compare(platform, &observed, rules);
        used.extend(report.used);
        let mismatches = check_platform(&platform.name, &fixture.pending, &report.mismatches);
        if mismatches.is_empty() {
            let pending = fixture
                .pending
                .iter()
                .filter(|entry| entry.platform == platform.name)
                .count();
            eprintln!(
                "adapter_golden {}: {:?}; {pending} exact pending-recapture entries",
                platform.name,
                platform.counts()
            );
        } else {
            failures.extend(mismatches);
        }
    }
    for rule in rules {
        if !used.contains(&rule.index) {
            failures.push(format!(
                "divergence rule {} ({}) is unused",
                rule.index, rule.ledger_id
            ));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload_clock::Delta;

    const PLATFORM: &str = "windows-lazer";
    const CASE: &str = "state-http.json-v2.get.menu";
    const POINTER: &str = "/game/paused";
    const REASON: Reason = Reason::S2RawClockPairAbsent;

    fn entry() -> Entry {
        Entry {
            platform: PLATFORM.into(),
            case: CASE.into(),
            part: "body".into(),
            frame: None,
            pointer: POINTER.into(),
            reason: REASON,
            decision: DECISION.into(),
            review_id: "review-id".into(),
        }
    }

    fn mismatch(deltas: &[(&str, Option<Reason>)]) -> Mismatch {
        Mismatch {
            case: CASE.into(),
            part: "body".into(),
            detail: Detail::Json {
                frame: None,
                deltas: deltas
                    .iter()
                    .map(|(pointer, reason)| Delta {
                        pointer: (*pointer).into(),
                        reason: *reason,
                    })
                    .collect(),
            },
        }
    }

    #[test]
    fn exact_match_required() {
        assert!(
            check_platform(
                PLATFORM,
                &[entry()],
                &[mismatch(&[(POINTER, Some(REASON))])]
            )
            .is_empty()
        );
    }

    #[test]
    fn unlisted_failure_fails() {
        let failures = check_platform(PLATFORM, &[], &[mismatch(&[(POINTER, Some(REASON))])]);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("unlisted JSON difference"));
    }

    #[test]
    fn stale_entry_fails() {
        let failures = check_platform(PLATFORM, &[entry()], &[]);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("stale pending-recapture.json entry"));
        assert!(failures[0].contains("remove it"));
    }

    #[test]
    fn other_pointer_in_listed_part_fails() {
        let failures = check_platform(
            PLATFORM,
            &[entry()],
            &[mismatch(&[
                (POINTER, Some(REASON)),
                ("/game/focused", None),
            ])],
        );
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("/game/focused"));
    }

    #[test]
    fn different_value_failure_on_listed_pointer_fails() {
        let failures = check_platform(PLATFORM, &[entry()], &[mismatch(&[(POINTER, None)])]);
        assert_eq!(failures.len(), 2);
        assert!(failures[0].contains("unlisted JSON difference"));
        assert!(failures[1].contains("stale"));
    }

    #[test]
    fn different_frame_in_listed_case_fails() {
        let mut entry = entry();
        entry.part = "ws.frame".into();
        entry.frame = Some(0);
        let mut mismatch = mismatch(&[(POINTER, Some(REASON))]);
        mismatch.part = "ws.frame".into();
        let Detail::Json { frame, .. } = &mut mismatch.detail else {
            unreachable!()
        };
        *frame = Some(2);
        let failures = check_platform(PLATFORM, &[entry], &[mismatch]);
        assert_eq!(failures.len(), 2);
        assert!(failures[0].contains("frame Some(2)"));
        assert!(failures[1].contains("frame Some(0)"));
    }
}
