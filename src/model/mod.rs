pub mod api;
pub mod enums;
pub mod memory;
pub mod states;

#[cfg(test)]
mod tests {
    use super::{api, enums, memory};

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
    fn struct_declaration_order_controls_wire_order() {
        let value = api::v1::SelectedMods {
            num: 8,
            text: "HD".into(),
        };
        assert_eq!(
            serde_json::to_string(&value).unwrap(),
            r#"{"num":8,"str":"HD"}"#
        );
    }

    #[test]
    fn undefined_fields_are_omitted_but_nullable_fields_are_null() {
        let timing = api::sc::MapTimingPoint {
            start_time: 100.0,
            bpm: None,
            beat_length: 500.0,
        };
        assert_eq!(
            serde_json::to_string(&timing).unwrap(),
            r#"{"startTime":100.0,"beatLength":500.0}"#
        );

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

    #[test]
    fn numeric_and_oddly_cased_keys_are_exact() {
        let hits = api::v2::LeaderboardHits {
            h0: 1,
            h50: 2,
            h100: 3,
            h300: 4,
            geki: 5,
            katu: 6,
        };
        assert_eq!(
            serde_json::to_string(&hits).unwrap(),
            r#"{"0":1,"50":2,"100":3,"300":4,"geki":5,"katu":6}"#
        );

        let fruits = api::v2::KeybindsFruits {
            k1: "A".into(),
            k2: "D".into(),
            dash: "Shift".into(),
        };
        assert_eq!(
            serde_json::to_string(&fruits).unwrap(),
            r#"{"k1":"A","k2":"D","Dash":"Shift"}"#
        );
    }
}
