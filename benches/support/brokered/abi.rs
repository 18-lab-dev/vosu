//! The frozen prototype plans as `VOSU_OP_READ_PLAN_SUBMIT` stages
//! (plugin-abi.md §6.3), shared by the production plugin
//! (`examples/brokered_reads_plugin/p1.rs`) and the runner's in-process
//! `executor` diagnostic.
//!
//! A prototype stage's reads may each name their own base; an ABI stage
//! has exactly one base (the plan root or one earlier op's pointer). A
//! prototype stage is therefore split into one ABI stage per run of reads
//! sharing a base, in order. The reads, their offsets, lengths and order,
//! and so the response bytes, are unchanged: precise's 8 prototype stages
//! (one of them reading through 18 different per-key pointers) become 25
//! ABI stages; regular's 5 stay 5.

use vosu_plugin_api::{PlanBase, PlanOp, ReadPlanSubmit, Stage, StageBase};

use super::plans::{Plan, ReadOp, Source};

fn same_base(a: &ReadOp, b: &ReadOp) -> bool {
    match (a.source, b.source) {
        (Source::Root, Source::Root) => true,
        (Source::Prev(a_stage, a_op), Source::Prev(b_stage, b_op)) => {
            (a_stage, a_op) == (b_stage, b_op)
        }
        _ => false,
    }
}

/// `plan`'s ABI stages, in submission (and so result) order.
pub fn stages(plan: &Plan) -> Vec<Stage> {
    // Each prototype (stage, read)'s ABI (stage, op), for later references.
    let mut location: Vec<Vec<(u32, u32)>> = Vec::new();
    let mut stages: Vec<Stage> = Vec::new();
    for stage in &plan.stages {
        let mut positions = Vec::new();
        let mut start = 0;
        while start < stage.len() {
            let end = start
                + stage[start..]
                    .iter()
                    .take_while(|op| same_base(op, &stage[start]))
                    .count();
            let base = match stage[start].source {
                Source::Root => StageBase::Root,
                Source::Prev(prior, index) => {
                    let (ref_stage, ref_op) = location[prior][index];
                    StageBase::Dependent { ref_stage, ref_op }
                }
            };
            for index in 0..end - start {
                positions.push((stages.len() as u32, index as u32));
            }
            stages.push(Stage {
                base,
                base_offset: 0,
                ops: stage[start..end]
                    .iter()
                    .map(|op| PlanOp {
                        offset: op.offset,
                        length: op.len,
                    })
                    .collect(),
            });
            start = end;
        }
        location.push(positions);
    }
    stages
}

/// `plan` as one submission rooted at the absolute address `root`.
pub fn submission(plan: &Plan, id: u64, handle: u64, root: u64, sync: bool) -> ReadPlanSubmit {
    ReadPlanSubmit {
        plan: id,
        handle,
        root: PlanBase::Absolute(root),
        stages: stages(plan),
        sync,
    }
}
