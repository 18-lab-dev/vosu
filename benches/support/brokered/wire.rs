//! The prototype plan protocol every brokered variant carries unchanged: the
//! plan encoding, core's dependent-stage executor and the flat response
//! (every read's bytes in stage/read order). Only the transport differs
//! between variants, so batching (one submission and one response per plan)
//! and the bytes on the wire are identical everywhere.

use sha2::{Digest, Sha256};
use vosu_plugin_api::memory::{MemoryError, ProcessMemory, ReadRequest};

use super::plans::{Plan, ReadOp, Source};

/// Executes every stage of `plan` against `process`, resolving
/// `Source::Prev` references from the previous stages' bytes. Each stage
/// becomes exactly one `read_many` call (one native batch).
pub fn execute_plan(
    process: &dyn ProcessMemory,
    root: u64,
    plan: &Plan,
) -> Result<Vec<Vec<u8>>, MemoryError> {
    let mut resolved: Vec<Vec<u64>> = Vec::with_capacity(plan.stages.len());
    let mut all_bytes: Vec<Vec<u8>> = Vec::with_capacity(plan.reads());

    for stage in &plan.stages {
        let mut buffers: Vec<Vec<u8>> = stage.iter().map(|op| vec![0u8; op.len as usize]).collect();
        let addresses: Vec<u64> = stage
            .iter()
            .map(|op| {
                let base = match op.source {
                    Source::Root => root,
                    Source::Prev(stage_index, op_index) => resolved[stage_index][op_index],
                };
                base.wrapping_add_signed(op.offset)
            })
            .collect();
        let mut requests: Vec<ReadRequest<'_>> = addresses
            .iter()
            .zip(buffers.iter_mut())
            .map(|(address, buffer)| ReadRequest::new(*address as usize, buffer.as_mut_slice()))
            .collect();
        process.read_many(&mut requests)?;
        drop(requests);

        let mut pointers = Vec::with_capacity(stage.len());
        for (op, buffer) in stage.iter().zip(&buffers) {
            pointers.push(if op.as_pointer {
                u64::from_le_bytes(buffer[..8].try_into().expect("pointer read is 8 bytes"))
            } else {
                0
            });
        }
        resolved.push(pointers);
        all_bytes.extend(buffers);
    }
    Ok(all_bytes)
}

/// Core's whole brokered step: decode one submitted plan, execute it and
/// return the flat response. Every brokered variant (prototype and
/// candidate) calls exactly this.
pub fn serve(process: &dyn ProcessMemory, root: u64, request: &[u8]) -> Vec<u8> {
    let plan = decode_plan(request);
    let results = execute_plan(process, root, &plan).expect("core executes brokered plan");
    results.into_iter().flatten().collect()
}

pub fn encode_plan(plan: &Plan) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend((plan.stages.len() as u32).to_le_bytes());
    for stage in &plan.stages {
        bytes.extend((stage.len() as u32).to_le_bytes());
        for op in stage {
            match op.source {
                Source::Root => {
                    bytes.push(0);
                    bytes.extend(0u32.to_le_bytes());
                    bytes.extend(0u32.to_le_bytes());
                }
                Source::Prev(stage_index, op_index) => {
                    bytes.push(1);
                    bytes.extend((stage_index as u32).to_le_bytes());
                    bytes.extend((op_index as u32).to_le_bytes());
                }
            }
            bytes.extend(op.offset.to_le_bytes());
            bytes.extend(op.len.to_le_bytes());
            bytes.push(u8::from(op.as_pointer));
        }
    }
    bytes
}

pub fn decode_plan(bytes: &[u8]) -> Plan {
    let mut cursor = 0usize;
    let mut take = |len: usize| {
        let slice = &bytes[cursor..cursor + len];
        cursor += len;
        slice
    };
    let stage_count = u32::from_le_bytes(take(4).try_into().unwrap()) as usize;
    let mut stages = Vec::with_capacity(stage_count);
    for _ in 0..stage_count {
        let op_count = u32::from_le_bytes(take(4).try_into().unwrap()) as usize;
        let mut ops = Vec::with_capacity(op_count);
        for _ in 0..op_count {
            let tag = take(1)[0];
            let prev_stage = u32::from_le_bytes(take(4).try_into().unwrap()) as usize;
            let prev_index = u32::from_le_bytes(take(4).try_into().unwrap()) as usize;
            let offset = i64::from_le_bytes(take(8).try_into().unwrap());
            let len = u32::from_le_bytes(take(4).try_into().unwrap());
            let as_pointer = take(1)[0] != 0;
            let source = if tag == 0 {
                Source::Root
            } else {
                Source::Prev(prev_stage, prev_index)
            };
            ops.push(ReadOp::new(source, offset, len, as_pointer));
        }
        stages.push(ops);
    }
    Plan { stages }
}

/// Lowercase hex SHA-256, the `result_sha256` metric's form.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
