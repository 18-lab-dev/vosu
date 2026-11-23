//! F2: a property test comparing `execute`'s decoded output with
//! `execute_bytes`'s decoded output over generated read plans, so the raw,
//! zero-allocation executor (`raw.rs`, a parser of untrusted bytes) is
//! checked against the simpler, allocating one it must agree with byte for
//! byte once decoded.
//!
//! Each generated plan is one to three stages deep. Every stage's first op
//! is forced to [`ProcessBitness::X64`]'s pointer width (8 bytes) so it can
//! always serve as a later stage's dependent `ref_op`; stage 0 is always
//! `StageBase::Root`, every later stage picks `Root` or `Dependent` on an
//! earlier stage's op 0. Every op's `offset` is drawn from three bands
//! against the single mapped region at `BASE`: inside it (an ordinary
//! read), past its end (a [`MemoryError::Read`] against the fake
//! memory's holes), and far enough negative to underflow `u64` (address
//! overflow) — so one run's ops already mix successes, holes, and
//! overflows within the same multi-op stage. The root is `Absolute` or
//! one of two module names, one resolved and one not, covering an
//! unresolved root propagating `NotReady` down a dependent chain.

use proptest::prelude::*;
use vosu_plugin_api::Message;

use super::helpers::{FakeMemory, far_deadline, op};
use super::*;

/// The one region every generated plan's memory reads against; large
/// enough that an 8-byte pointer op anywhere inside it still fits.
const BASE: u64 = 0x1_0000;
const REGION_LEN: usize = 64;

#[derive(Debug, Clone, Copy)]
enum GenBase {
    Root,
    Dependent(u32),
}

#[derive(Debug, Clone)]
struct GenStage {
    base: GenBase,
    base_offset: i64,
    /// Extra ops beyond the forced, pointer-width op 0.
    extra: Vec<(i64, u32)>,
}

fn offset_strategy() -> impl Strategy<Value = i64> {
    prop_oneof![
        (0u32..(REGION_LEN as u32 - 8)).prop_map(i64::from),
        (REGION_LEN as u32..(REGION_LEN as u32 + 200)).prop_map(i64::from),
        Just(-1_000_000_000_000i64),
    ]
}

fn length_strategy() -> impl Strategy<Value = u32> {
    prop_oneof![Just(1u32), Just(2), Just(4), Just(8)]
}

fn stage_strategy(stage_index: u32) -> impl Strategy<Value = GenStage> {
    let base_strategy: BoxedStrategy<GenBase> = if stage_index == 0 {
        Just(GenBase::Root).boxed()
    } else {
        prop_oneof![
            Just(GenBase::Root),
            (0..stage_index).prop_map(GenBase::Dependent),
        ]
        .boxed()
    };
    (
        base_strategy,
        -16i64..16,
        prop::collection::vec((offset_strategy(), length_strategy()), 0..4),
    )
        .prop_map(|(base, base_offset, extra)| GenStage {
            base,
            base_offset,
            extra,
        })
}

fn plan_strategy() -> impl Strategy<Value = (PlanBase, Vec<GenStage>)> {
    let root_strategy = prop_oneof![
        Just(PlanBase::Absolute(BASE)),
        Just(PlanBase::Module {
            name: "resolved".to_owned(),
            offset: 0,
        }),
        Just(PlanBase::Module {
            name: "unresolved".to_owned(),
            offset: 0,
        }),
    ];
    let stage_count = 1usize..=3;
    (root_strategy, stage_count).prop_flat_map(|(root, stage_count)| {
        let stages = (0..stage_count as u32)
            .map(stage_strategy)
            .collect::<Vec<_>>();
        (Just(root), stages)
    })
}

fn build_submit(root: PlanBase, gen_stages: Vec<GenStage>) -> ReadPlanSubmit {
    let stages = gen_stages
        .into_iter()
        .map(|gen_stage| {
            let base = match gen_stage.base {
                GenBase::Root => StageBase::Root,
                GenBase::Dependent(ref_stage) => StageBase::Dependent {
                    ref_stage,
                    ref_op: 0,
                },
            };
            let mut ops = vec![op(0, 8)];
            ops.extend(
                gen_stage
                    .extra
                    .into_iter()
                    .map(|(offset, length)| op(offset, length)),
            );
            Stage {
                base,
                base_offset: gen_stage.base_offset,
                ops,
            }
        })
        .collect();
    ReadPlanSubmit {
        plan: 1,
        handle: 1,
        root,
        stages,
        sync: false,
    }
}

struct HoleyModules;

impl ModuleResolver for HoleyModules {
    fn resolve(&self, module: &str) -> Option<u64> {
        match module {
            "resolved" => Some(BASE),
            _ => None,
        }
    }
}

/// Runs both execution entry points against the same plan and fake
/// memory, decodes `execute_bytes`'s raw result, and asserts the two
/// agree field for field.
fn assert_execute_matches_execute_bytes(submit: ReadPlanSubmit) -> Result<(), TestCaseError> {
    let region: Vec<u8> = (0..REGION_LEN as u8).collect();
    let memory = FakeMemory::new(vec![(BASE as usize, region)]);
    let deadline = far_deadline();

    let typed = execute(
        &submit,
        &memory,
        ProcessBitness::X64,
        &HoleyModules,
        deadline,
    );

    let message = Message::ReadPlanSubmit(submit);
    let flags = message.flags();
    let payload = message.encode_payload().unwrap();
    let raw = execute_bytes(
        &payload,
        flags,
        &memory,
        ProcessBitness::X64,
        &HoleyModules,
        deadline,
    );

    match (typed, raw) {
        (Ok(typed_result), Ok(raw_bytes)) => {
            let decoded =
                Message::decode(vosu_opcode::VOSU_OP_READ_PLAN_RESULT, 0, &raw_bytes).unwrap();
            let Message::ReadPlanResult(raw_result) = decoded else {
                return Err(TestCaseError::fail("expected a decoded ReadPlanResult"));
            };
            prop_assert_eq!(typed_result.plan, raw_result.plan);
            prop_assert_eq!(typed_result.results, raw_result.results);
        }
        (Err(typed_error), Err(ExecuteBytesError::Execute(raw_error))) => {
            prop_assert_eq!(typed_error, raw_error);
        }
        (typed, raw) => {
            return Err(TestCaseError::fail(format!(
                "execute and execute_bytes disagreed on whether the plan was rejected: \
                 execute={typed:?}, execute_bytes rejected as execution error={raw:?}"
            )));
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(vosu_proptest::vosu_proptest_config())]

    /// `execute`'s decoded `ReadPlanResult` and `execute_bytes`'s decoded
    /// `ReadPlanResult` are identical for any structurally valid plan
    /// (module roots resolved and unresolved, dependent chains, address
    /// overflow, and a mix of successful/hole/overflowing ops within one
    /// multi-op stage), run against the same fake memory with holes.
    #[test]
    fn prop_execute_matches_execute_bytes((root, gen_stages) in plan_strategy()) {
        let submit = build_submit(root, gen_stages);
        assert_execute_matches_execute_bytes(submit)?;
    }
}
