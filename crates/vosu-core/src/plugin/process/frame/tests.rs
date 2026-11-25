use proptest::prelude::*;
use vosu_plugin_api::{VOSU_READ_PLAN_SYNC, vosu_opcode};

use super::control::*;
use super::*;

fn header(opcode: u32, flags: u32, correlation: u64) -> FrameHeader {
    FrameHeader {
        opcode,
        flags,
        correlation,
    }
}

fn op(opcode: vosu_opcode) -> u32 {
    opcode as u32
}

#[test]
fn head_layout_is_exact_little_endian_fields() {
    let head = encode_head(&header(0x0403_0201, 0x0807_0605, 0x100f_0e0d_0c0b_0a09), 3).unwrap();
    assert_eq!(
        head,
        [
            23, 0, 0, 0, // total_len = 20 + 3, excluding itself
            0x01, 0x02, 0x03, 0x04, // opcode
            0x05, 0x06, 0x07, 0x08, // flags
            0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, // correlation
            3, 0, 0, 0, // payload_len
        ]
    );
    assert_eq!(HEAD_BYTES, 24);
    assert_eq!(MAX_FRAME_BYTES, 1024 * 1024 + 24);
}

#[test]
fn exact_maximum_payload_is_legal_and_one_byte_more_is_refused() {
    let head = encode_head(&header(1, 0, 0), MAX_PAYLOAD_BYTES).unwrap();
    assert_eq!(decode_head(&head).unwrap().1, MAX_PAYLOAD_BYTES);
    assert_eq!(
        encode_head(&header(1, 0, 0), MAX_PAYLOAD_BYTES + 1),
        Err(FrameError::Oversize {
            len: MAX_PAYLOAD_BYTES + 1
        })
    );
    assert!(encode_head(&header(1, 0, 0), usize::MAX).is_err());
}

#[test]
fn forged_heads_are_refused_without_overflow() {
    let mut head = encode_head(&header(1, 0, 0), 8).unwrap();
    head[0..4].copy_from_slice(&29_u32.to_le_bytes());
    assert_eq!(
        decode_head(&head),
        Err(FrameError::LengthMismatch {
            total_len: 29,
            payload_len: 8
        })
    );
    // payload_len past the cap is refused even when total_len "agrees".
    let len = MAX_PAYLOAD_BYTES as u32 + 1;
    head[0..4].copy_from_slice(&(len + 20).to_le_bytes());
    head[20..24].copy_from_slice(&len.to_le_bytes());
    assert!(matches!(
        decode_head(&head),
        Err(FrameError::Oversize { .. })
    ));
    // total_len = u32::MAX with payload_len = u32::MAX - 20 would overflow
    // naive 32-bit sums; both fail closed.
    head[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
    head[20..24].copy_from_slice(&(u32::MAX - 20).to_le_bytes());
    assert!(decode_head(&head).is_err());
    head[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(decode_head(&head).is_err());
}

#[test]
fn public_policy_follows_each_opcodes_direction_and_flags() {
    let mut to_host = DirectionPolicy::new(Direction::PluginToHost);
    let mut to_plugin = DirectionPolicy::new(Direction::HostToPlugin);
    for opcode in [
        vosu_opcode::VOSU_OP_PUBLISH_BATCH,
        vosu_opcode::VOSU_OP_SESSION_IDENTIFY,
        vosu_opcode::VOSU_OP_ACTION_CALL,
        vosu_opcode::VOSU_OP_ACTION_RESULT,
    ] {
        assert_eq!(to_host.admit(&header(op(opcode), 0, 9), 10), Ok(()));
    }
    for opcode in [
        vosu_opcode::VOSU_OP_READ_PLAN_RESULT,
        vosu_opcode::VOSU_OP_STATE_UPDATE,
        vosu_opcode::VOSU_OP_ACTION_RESULT,
    ] {
        assert_eq!(to_plugin.admit(&header(op(opcode), 0, 9), 10), Ok(()));
    }
    assert!(matches!(
        to_host.admit(&header(op(vosu_opcode::VOSU_OP_STATE_UPDATE), 0, 0), 0),
        Err(PolicyViolation::Opcode { .. })
    ));
    assert!(matches!(
        to_plugin.admit(&header(op(vosu_opcode::VOSU_OP_PUBLISH_FIELD), 0, 0), 0),
        Err(PolicyViolation::Opcode { .. })
    ));
    assert!(matches!(
        to_host.admit(&header(7, 0, 0), 0),
        Err(PolicyViolation::Opcode { opcode: 7, .. })
    ));
    let submit = op(vosu_opcode::VOSU_OP_READ_PLAN_SUBMIT);
    assert_eq!(
        to_host.admit(&header(submit, VOSU_READ_PLAN_SYNC, 0), 56),
        Ok(())
    );
    assert!(matches!(
        to_host.admit(&header(submit, VOSU_READ_PLAN_SYNC | 2, 0), 56),
        Err(PolicyViolation::Flags { .. })
    ));
    assert!(matches!(
        to_host.admit(
            &header(
                op(vosu_opcode::VOSU_OP_PUBLISH_FIELD),
                VOSU_READ_PLAN_SYNC,
                0
            ),
            48
        ),
        Err(PolicyViolation::Flags { .. })
    ));
    assert!(matches!(
        to_plugin.admit(&header(op(vosu_opcode::VOSU_OP_READ_PLAN_RESULT), 1, 0), 16),
        Err(PolicyViolation::Flags { .. })
    ));
}

#[test]
fn private_frames_obey_direction_payload_flags_and_correlation() {
    let mut to_host = DirectionPolicy::new(Direction::PluginToHost);
    let mut to_plugin = DirectionPolicy::new(Direction::HostToPlugin);
    assert_eq!(to_plugin.admit(&header(CREATE, 0, 0), 300), Ok(()));
    assert_eq!(to_host.admit(&header(READY, 0, 0), 12), Ok(()));
    assert_eq!(to_host.admit(&header(FAILED, 0, 0), 40), Ok(()));
    assert_eq!(to_plugin.admit(&header(DESTROY, 0, 0), 0), Ok(()));
    assert_eq!(to_host.admit(&header(DESTROYED, 0, 0), 0), Ok(()));
    assert_eq!(to_plugin.admit(&header(STATUS, 0, 17), 4), Ok(()));

    for (direction, frame, len) in [
        (Direction::PluginToHost, header(CREATE, 0, 0), 1),
        (Direction::HostToPlugin, header(READY, 0, 0), 1),
        (Direction::HostToPlugin, header(ACK_NEXT, 0, 1), 0),
        (Direction::PluginToHost, header(STATUS, 0, 1), 4),
        (Direction::PluginToHost, header(0x8000_0042, 0, 0), 0),
    ] {
        assert!(
            matches!(
                DirectionPolicy::new(direction).admit(&frame, len),
                Err(PolicyViolation::Opcode { .. })
            ),
            "{frame:?}"
        );
    }
    assert!(matches!(
        to_plugin.admit(&header(STATUS, 0, 1), 5),
        Err(PolicyViolation::PrivatePayload { len: 5, .. })
    ));
    assert!(matches!(
        to_plugin.admit(&header(DESTROY, 0, 0), 1),
        Err(PolicyViolation::PrivatePayload { .. })
    ));
    assert!(matches!(
        to_plugin.admit(&header(DESTROY, 1, 0), 0),
        Err(PolicyViolation::Flags { .. })
    ));
    assert!(matches!(
        to_host.admit(&header(READY, 0, 3), 0),
        Err(PolicyViolation::PrivateCorrelation { .. })
    ));
}

#[test]
fn ack_next_requires_increasing_ids_and_one_following_public_frame() {
    let publish = op(vosu_opcode::VOSU_OP_PUBLISH_BATCH);
    let mut policy = DirectionPolicy::new(Direction::PluginToHost);
    assert_eq!(policy.admit(&header(ACK_NEXT, 0, 5), 0), Ok(()));
    assert_eq!(policy.admit(&header(publish, 0, 0), 16), Ok(()));
    assert_eq!(
        policy.admit(&header(publish, 0, 0), 16),
        Ok(()),
        "pairing is one frame"
    );
    assert!(matches!(
        policy.admit(&header(ACK_NEXT, 0, 5), 0),
        Err(PolicyViolation::RequestId { id: 5, previous: 5 })
    ));

    let mut policy = DirectionPolicy::new(Direction::PluginToHost);
    assert_eq!(policy.admit(&header(ACK_NEXT, 0, 1), 0), Ok(()));
    assert!(matches!(
        policy.admit(&header(ACK_NEXT, 0, 2), 0),
        Err(PolicyViolation::Pairing { .. })
    ));
    let mut policy = DirectionPolicy::new(Direction::PluginToHost);
    assert_eq!(policy.admit(&header(ACK_NEXT, 0, 1), 0), Ok(()));
    assert!(matches!(
        policy.admit(&header(DESTROYED, 0, 0), 0),
        Err(PolicyViolation::Pairing { .. })
    ));
}

#[test]
fn reply_next_must_be_followed_by_a_read_plan_result() {
    let result = op(vosu_opcode::VOSU_OP_READ_PLAN_RESULT);
    let mut policy = DirectionPolicy::new(Direction::HostToPlugin);
    assert_eq!(policy.admit(&header(REPLY_NEXT, 0, 3), 0), Ok(()));
    assert_eq!(policy.admit(&header(result, 0, 0), 40), Ok(()));
    assert_eq!(policy.admit(&header(REPLY_NEXT, 0, 3), 0), Ok(()));
    assert!(matches!(
        policy.admit(&header(op(vosu_opcode::VOSU_OP_STATE_UPDATE), 0, 0), 40),
        Err(PolicyViolation::Pairing { .. })
    ));
}

proptest! {
    #![proptest_config(vosu_proptest::vosu_proptest_config())]

    /// Every legal head round-trips field for field.
    #[test]
    fn prop_frame_head_round_trips(
        opcode in any::<u32>(),
        flags in any::<u32>(),
        correlation in any::<u64>(),
        len in 0..=MAX_PAYLOAD_BYTES,
    ) {
        let frame = header(opcode, flags, correlation);
        let head = encode_head(&frame, len).unwrap();
        prop_assert_eq!(decode_head(&head), Ok((frame, len)));
    }

    /// Arbitrary head bytes never panic, and anything accepted is
    /// self-consistent and within the cap.
    #[test]
    fn prop_arbitrary_heads_decode_only_when_consistent(bytes in any::<[u8; HEAD_BYTES]>()) {
        if let Ok((frame, len)) = decode_head(&bytes) {
            prop_assert!(len <= MAX_PAYLOAD_BYTES);
            prop_assert_eq!(encode_head(&frame, len).unwrap(), bytes);
        }
    }

    /// The policy never panics on arbitrary headers in either direction.
    #[test]
    fn prop_policy_total_on_arbitrary_headers(
        frames in proptest::collection::vec(
            (
                prop_oneof![any::<u32>(), 0_u32..=72, 0x8000_0000_u32..=0x8000_0013],
                prop_oneof![Just(0_u32), Just(1), any::<u32>()],
                any::<u64>(),
                0..=8_usize,
            ),
            0..32,
        ),
        to_host in any::<bool>(),
    ) {
        let direction = if to_host { Direction::PluginToHost } else { Direction::HostToPlugin };
        let mut policy = DirectionPolicy::new(direction);
        for (opcode, flags, correlation, len) in frames {
            let _ = policy.admit(&header(opcode, flags, correlation), len);
        }
    }
}
