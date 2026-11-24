//! osu! value types and osu!-exact helpers shared by the reader, calculator,
//! and adapters: memory/state/enum models and the calculation root helpers
//! (JS-compatible rounding and number writing, mods, grades, legacy hits).

pub mod enums;
pub mod launch;
pub mod memory;
#[path = "root.rs"]
mod root;
pub mod states;

pub use root::*;

#[cfg(test)]
mod tests {
    use super::{enums, memory};

    #[test]
    fn numeric_enums_use_source_discriminants() {
        assert_eq!(
            serde_json::to_string(&enums::GameState::ResultScreen).unwrap(),
            "7"
        );
        assert_eq!(
            serde_json::from_str::<enums::GroupType>("12").unwrap(),
            enums::GroupType::SearchShowAll
        );
        assert_eq!(enums::GroupType::Title.value(), 13);
        assert_eq!(enums::Bitness::X64.value(), 64);
        assert!(enums::GroupType::from_value(11).is_none());
    }

    #[test]
    fn undefined_fields_are_omitted_but_nullable_fields_are_null() {
        let matchmaking = memory::MatchmakingStats {
            rating: 1234.5,
            rank: None,
            plays: 10,
            wins: 6,
            is_provisional: true,
        };
        assert_eq!(
            serde_json::to_string(&matchmaking).unwrap(),
            r#"{"rating":1234.5,"rank":null,"plays":10,"wins":6,"isProvisional":true}"#
        );
    }
}
