//! Bounded host-side proof cache, never an SDK trust exemption. Only exact
//! structural bytes previously validated by this host can reuse their proof.
use super::*;
use std::cell::RefCell;

#[derive(Clone, Copy)]
pub(super) struct Layout {
    pub total_ops: usize,
    pub total_len: u64,
    pub ref_width_x86: bool,
    pub ref_width_x64: bool,
}

#[derive(Default)]
struct Cache {
    payload: Vec<u8>,
    layout: Option<Layout>,
}

thread_local! {
    static LAST: RefCell<Cache> = RefCell::new(Cache::default());
}

pub(super) fn validate(
    payload: &[u8],
    flags: u32,
) -> Result<(vosu_read_plan_submit_header, Layout), ExecuteBytesError> {
    validate_flags(vosu_opcode::VOSU_OP_READ_PLAN_SUBMIT, flags)
        .map_err(|error| ExecuteBytesError::Message(error.into()))?;
    let header: vosu_read_plan_submit_header =
        read_flat_header(payload).map_err(|error| ExecuteBytesError::Message(error.into()))?;
    LAST.with(|last| {
        let mut last = last.borrow_mut();
        // Plan IDs and absolute root addresses are execution data, not layout.
        // No hash collision can authorize an unchecked read: compare all other
        // bytes, including lengths, offsets, references and reserved fields.
        const ADDRESS: usize = std::mem::offset_of!(vosu_read_plan_submit_header, root)
            + std::mem::offset_of!(vosu_plan_base_flat, address);
        if payload.len() == last.payload.len()
            && payload[8..ADDRESS] == last.payload[8..ADDRESS]
            && payload[ADDRESS + 8..] == last.payload[ADDRESS + 8..]
            && let Some(layout) = last.layout
        {
            return Ok((header, layout));
        }
        validate_read_plan_submit(payload)
            .map_err(|error| ExecuteBytesError::Message(error.into()))?;
        let mut layout = Layout {
            total_ops: 0,
            total_len: 0,
            // These proofs are reusable only with the exact structural bytes
            // compared above. Keep both bitnesses: a reader thread may switch
            // between sessions without changing the encoded plan.
            ref_width_x86: validate_read_plan_ref_op_width(payload, 4).is_ok(),
            ref_width_x64: validate_read_plan_ref_op_width(payload, 8).is_ok(),
        };
        // Noncanonical raw input may alias stage/op tables with the ignored
        // header bytes. Execute it after full validation, but never cache it.
        let mut cacheable =
            header.stages_offset as usize >= size_of::<vosu_read_plan_submit_header>();
        for index in 0..header.stage_count as usize {
            let stage: vosu_stage_header = read_struct_at(
                payload,
                header.stages_offset as usize + index * size_of::<vosu_stage_header>(),
            );
            cacheable &= stage.ops_offset as usize >= size_of::<vosu_read_plan_submit_header>();
            layout.total_ops += stage.op_count as usize;
            for index in 0..stage.op_count as usize {
                let op: vosu_plan_op = read_struct_at(
                    payload,
                    stage.ops_offset as usize + index * size_of::<vosu_plan_op>(),
                );
                layout.total_len += u64::from(op.length);
            }
        }
        if cacheable {
            last.payload.clear();
            last.payload.extend_from_slice(payload);
            last.layout = Some(layout);
        }
        Ok((header, layout))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vosu_plugin_api::{
        Message, PlanBase, PlanOp, ReadPlanSubmit, Stage, StageBase, vosu_stage_base,
    };

    fn payload() -> Vec<u8> {
        Message::ReadPlanSubmit(ReadPlanSubmit {
            plan: 1,
            handle: 1,
            root: PlanBase::Absolute(0x1000),
            stages: vec![Stage {
                base: StageBase::Root,
                base_offset: 0,
                ops: vec![PlanOp {
                    offset: 0,
                    length: 4,
                }],
            }],
            sync: false,
        })
        .encode_payload()
        .unwrap()
    }

    #[test]
    fn changed_layout_and_flags_cannot_reuse_a_valid_proof() {
        let bytes = payload();
        validate(&bytes, 0).unwrap();
        let header: vosu_read_plan_submit_header = read_flat_header(&bytes).unwrap();
        let stage: vosu_stage_header = read_struct_at(&bytes, header.stages_offset as usize);
        let mut malformed = bytes.clone();
        // Same allocation, shape and total length: only one reserved op byte changes.
        malformed[stage.ops_offset as usize + 12] = 1;
        assert!(validate(&malformed, 0).is_err());
        assert!(validate(&bytes, u32::MAX).is_err());
        let mut changed_data = bytes.clone();
        changed_data[..8].copy_from_slice(&2u64.to_ne_bytes());
        let address = std::mem::offset_of!(vosu_read_plan_submit_header, root)
            + std::mem::offset_of!(vosu_plan_base_flat, address);
        changed_data[address..address + 8].copy_from_slice(&0x2000u64.to_ne_bytes());
        let (header, layout) = validate(&changed_data, 0).unwrap();
        assert_eq!(header.plan, 2);
        assert_eq!(header.root.address, 0x2000);
        assert_eq!(layout.total_ops, 1);
        assert_eq!(layout.total_len, 4);
    }

    #[test]
    fn cached_width_proofs_follow_layout_not_session_bitness() {
        for width in [4, 8, 1, 4] {
            let bytes = Message::ReadPlanSubmit(ReadPlanSubmit {
                plan: 1,
                handle: 1,
                root: PlanBase::Absolute(0x1000),
                stages: vec![
                    Stage {
                        base: StageBase::Root,
                        base_offset: 0,
                        ops: vec![PlanOp {
                            offset: 0,
                            length: width,
                        }],
                    },
                    Stage {
                        base: StageBase::Dependent {
                            ref_stage: 0,
                            ref_op: 0,
                        },
                        base_offset: 0,
                        ops: vec![PlanOp {
                            offset: 0,
                            length: 1,
                        }],
                    },
                ],
                sync: false,
            })
            .encode_payload()
            .unwrap();
            // Second validation hits the structural proof cache. Both proofs
            // must remain valid when a submitting thread switches session.
            for _ in 0..2 {
                let (_, layout) = validate(&bytes, 0).unwrap();
                assert_eq!(layout.ref_width_x86, width == 4);
                assert_eq!(layout.ref_width_x64, width == 8);
            }
        }
    }

    #[test]
    fn aliased_header_tables_never_enter_the_proof_cache() {
        let mut bytes = payload();
        // A zero-op stage aliases plan/handle fields. A new plan id then changes
        // its kind, which must still be rejected on the next submit.
        let stage = vosu_stage_header {
            base: vosu_stage_base {
                kind: 0,
                ref_stage: 0,
                ref_op: 0,
                reserved: 0,
                offset: 0,
            },
            ops_offset: 0,
            op_count: 0,
        };
        write_struct_at(&mut bytes, 0, &stage);
        let mut header: vosu_read_plan_submit_header = read_flat_header(&bytes).unwrap();
        header.stages_offset = 0;
        header.stage_count = 1;
        write_struct_at(&mut bytes, 0, &header);
        assert!(validate(&bytes, 0).is_ok());
        bytes[0] = 2;
        assert!(validate(&bytes, 0).is_err());
    }
}
