pub mod api;

#[cfg(test)]
mod tests {
    use super::api;

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
