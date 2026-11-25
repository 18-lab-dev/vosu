//! Saved trust records of tools a 1.x catalog installed. Catalog v2 owns
//! fetching, planning and installation (`/api/catalog/*`, M6b H0); this host
//! keeps only the exact-identity rule that already-installed legacy tools'
//! records were written with, so their approval survives until M7/M8.
use super::*;

const OFFICIAL_SOURCE: &str = "18-lab-dev/vosu-tools";
const VOSULAY_SOURCE: &str = "18-lab-dev/vosulay";
const OVERLAYS_SOURCE: &str = "18-lab-dev/vosu-overlays";

/// Historical default identities are migration inputs only, never trust aliases.
pub(super) fn migrated_source(source: &str) -> Option<&'static str> {
    match source {
        "shiruviaa/vosu-tools" => Some(OFFICIAL_SOURCE),
        "shiruviaa/vosulay" => Some(VOSULAY_SOURCE),
        _ => None,
    }
}

fn official_source(source: &str) -> bool {
    matches!(source, OFFICIAL_SOURCE | VOSULAY_SOURCE | OVERLAYS_SOURCE)
}

pub(super) fn record_source(state: &mut Saved, source: &str) {
    if state.catalog_source.as_deref() != Some(source) {
        state.trusted = false;
    }
    state.official = official_source(source);
    state.trusted |= state.official;
    state.catalog_source = Some(source.into());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_exact_official_source_identities_grant_automatic_trust() {
        let mut state = Saved::default();
        for source in [OFFICIAL_SOURCE, VOSULAY_SOURCE, OVERLAYS_SOURCE] {
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
