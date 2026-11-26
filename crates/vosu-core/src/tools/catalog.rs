//! Catalog v2 uses committed installed provenance for tool trust. Saved
//! source and official flags remain only for the 1.x migration.
use super::*;
use crate::catalog::{install::InstalledRecord, sources::SourceIdentity};

const OFFICIAL_SOURCE: &str = "18-lab-dev/vosu-tools";
const VOSULAY_SOURCE: &str = "18-lab-dev/vosulay";

/// Historical default identities are migration inputs only, never trust aliases.
pub(super) fn migrated_source(source: &str) -> Option<&'static str> {
    match source {
        "shiruviaa/vosu-tools" => Some(OFFICIAL_SOURCE),
        "shiruviaa/vosulay" => Some(VOSULAY_SOURCE),
        _ => None,
    }
}

/// The installed record is evidence; only the current exact source can
/// activate fixture authority. Do not store the result in tool preferences.
pub(super) fn installed_source(record: &InstalledRecord) -> (&'static str, bool) {
    if !record.matches_fixture_source() {
        ("user", record.fixture)
    } else if record.source.is_official() {
        (
            if record.fixture {
                "official-fixture"
            } else {
                "official"
            },
            false,
        )
    } else {
        ("user", false)
    }
}

pub(super) fn record_source(state: &mut Saved, source: &str) {
    if state.catalog_source.as_deref() != Some(source) {
        state.trusted = false;
    }
    state.official =
        SourceIdentity::parse(source).is_ok_and(|parsed| parsed.identity.is_allowlisted());
    state.trusted |= state.official;
    state.catalog_source = Some(source.into());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_exact_official_source_identities_grant_automatic_trust() {
        let mut state = Saved::default();
        for source in crate::catalog::sources::OFFICIAL_SOURCES {
            record_source(&mut state, source);
            assert!(state.official && state.trusted);
            // Removing and re-adding the identical source does not change identity.
            state = Saved::default();
            record_source(&mut state, source);
            assert!(state.official && state.trusted);
        }
        for source in [
            "another/tools",
            "18-lab-dev/vosu-tools@v1.0.0",
            "https://github.com/18-lab-dev/vosu-tools/releases/latest/download/catalog.json",
            "18-lab-dev/vosu-overlays@v0.4.0",
            "https://github.com/18-lab-dev/vosu-overlays/releases/latest/download/catalog.json",
            "shiruviaa/vosu-overlays",
            "18-lab-dev/vosulay@v1.5.0",
            "https://github.com/18-lab-dev/vosulay/releases/latest/download/catalog.json",
            "18-Lab-Dev/vosulay",
            // Historical identities are migration inputs, not live trust aliases.
            "shiruviaa/vosu-tools",
            "shiruviaa/vosulay",
        ] {
            record_source(&mut state, source);
            assert!(!state.official && !state.trusted, "{source}");
            state.trusted = true;
            record_source(&mut state, source);
            assert!(
                state.trusted,
                "an approved same-source update preserves approval"
            );
        }
    }
}
