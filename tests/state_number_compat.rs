use vosu_core::state::StateValue;
use vosu_osu_model::write_js_json_number;

#[test]
fn state_and_model_number_writers_match_shared_vectors() {
    for &(value, expected) in vosu_proptest::ECMA_NUMBER_CASES {
        let mut state = Vec::new();
        StateValue::F64(value).write_json(&mut state);
        let mut model = Vec::new();
        write_js_json_number(&mut model, value).unwrap();
        assert_eq!(state, model, "writers differ for {value:?}");
        assert_eq!(state, expected.as_bytes(), "value {value:?}");
    }
}
