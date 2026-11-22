//! `prop_*` bounds tests for flat payload validation (`src/validate.rs`),
//! using `vosu_proptest::vosu_proptest_config()`.

use proptest::prelude::*;
use vosu_plugin_api::{
    VOSU_FRAME_MAX_BYTES, VOSU_READ_PLAN_MAX_BYTES, VOSU_READ_PLAN_SYNC,
    VOSU_STREAM_CHUNK_MAX_BYTES, ValidationError, validate_bytes_range, validate_flags,
    validate_frame_len, validate_read_plan_len, validate_stream_chunk_len, vosu_opcode,
};
use vosu_proptest::vosu_proptest_config;

proptest! {
    #![proptest_config(vosu_proptest_config())]

    /// A byte range entirely inside the buffer always validates.
    #[test]
    fn prop_bytes_range_in_bounds_accepted(buffer_len in 0u32..4096, offset in 0u32..4096, extra in 0u32..4096) {
        prop_assume!(offset <= buffer_len);
        let len = (buffer_len - offset).min(extra);
        prop_assert!(validate_bytes_range(buffer_len as usize, offset, len).is_ok());
    }

    /// Any `offset + len` that runs past the buffer's end is rejected,
    /// regardless of how it got there.
    #[test]
    fn prop_bytes_range_out_of_bounds_rejected(buffer_len in 0u32..4096, offset in 0u32..u32::MAX, len in 0u32..u32::MAX) {
        let in_bounds = offset
            .checked_add(len)
            .is_some_and(|end| (end as u64) <= buffer_len as u64);
        prop_assume!(!in_bounds);
        prop_assert_eq!(
            validate_bytes_range(buffer_len as usize, offset, len),
            if offset.checked_add(len).is_none() {
                Err(ValidationError::Overflow)
            } else {
                Err(ValidationError::OutOfBounds)
            }
        );
    }

    /// A length at or under `VOSU_FRAME_MAX_BYTES` is always accepted, and
    /// one single byte over is always rejected: the cap is exact, not
    /// approximate.
    #[test]
    fn prop_frame_len_cap_is_exact(delta in 0u32..4096) {
        let max = VOSU_FRAME_MAX_BYTES as usize;
        prop_assert!(validate_frame_len(max.saturating_sub(delta as usize)).is_ok());
        prop_assert!(validate_frame_len(max + 1 + delta as usize).is_err());
    }

    /// Same exactness for the narrower read-plan cap.
    #[test]
    fn prop_read_plan_len_cap_is_exact(delta in 0u32..4096) {
        let max = VOSU_READ_PLAN_MAX_BYTES as usize;
        prop_assert!(validate_read_plan_len(max.saturating_sub(delta as usize)).is_ok());
        prop_assert!(validate_read_plan_len(max + 1 + delta as usize).is_err());
    }

    /// Same exactness for the stream-chunk cap.
    #[test]
    fn prop_stream_chunk_len_cap_is_exact(delta in 0u32..4096) {
        let max = VOSU_STREAM_CHUNK_MAX_BYTES;
        prop_assert!(validate_stream_chunk_len(max.saturating_sub(delta)).is_ok());
        prop_assert!(validate_stream_chunk_len(max.saturating_add(1).saturating_add(delta)).is_err());
    }

    /// `VOSU_OP_READ_PLAN_SUBMIT` accepts only `0` or exactly
    /// `VOSU_READ_PLAN_SYNC`; any other bit is unrecognized (D5).
    #[test]
    fn prop_read_plan_submit_flags_only_sync_bit_recognized(flags in any::<u32>()) {
        let result = validate_flags(vosu_opcode::VOSU_OP_READ_PLAN_SUBMIT, flags);
        if flags & !VOSU_READ_PLAN_SYNC == 0 {
            prop_assert!(result.is_ok());
        } else {
            prop_assert_eq!(result, Err(ValidationError::UnrecognizedFlags));
        }
    }

    /// Every other opcode recognizes no flag bit at all (D4): `flags` must
    /// be exactly `0`.
    #[test]
    fn prop_non_read_plan_submit_flags_must_be_zero(flags in any::<u32>()) {
        let result = validate_flags(vosu_opcode::VOSU_OP_PUBLISH_FIELD, flags);
        if flags == 0 {
            prop_assert!(result.is_ok());
        } else {
            prop_assert_eq!(result, Err(ValidationError::UnrecognizedFlags));
        }
    }
}
