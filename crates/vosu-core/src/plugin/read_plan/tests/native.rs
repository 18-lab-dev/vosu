//! `execute` against this process's own real memory through the native
//! [`ProcessMemory`] backend (Linux `open_process`/Windows
//! `WindowsProcessMemory`), as opposed to every other test's fake memory.
//! Split out of `tests.rs` so that module stays under the maintained-file
//! line limit (`docs/architecture/source-map.md`).

use super::helpers::{NoModules, far_deadline, op};
use super::*;

#[cfg(target_os = "linux")]
#[test]
fn execute_reads_this_process_own_memory_through_the_native_backend() {
    use vosu_sys::process::{open_process, process_bitness};

    let marker: Box<u64> = Box::new(0x1122_3344_5566_7788);
    let address = std::ptr::from_ref::<u64>(&marker) as u64;
    let pid = std::process::id();
    let memory = open_process(pid).unwrap();
    let bitness = process_bitness(pid).unwrap();

    let submit = ReadPlanSubmit {
        plan: 13,
        handle: 1,
        root: PlanBase::Absolute(address),
        stages: vec![Stage {
            base: StageBase::Root,
            base_offset: 0,
            ops: vec![op(0, 8)],
        }],
        sync: false,
    };

    let result = execute(&submit, &memory, bitness, &NoModules, far_deadline()).unwrap();
    assert_eq!(
        result.results,
        vec![ReadResult::Ok(marker.to_le_bytes().to_vec())]
    );
}

#[cfg(windows)]
#[test]
fn execute_reads_this_process_own_memory_through_the_native_backend() {
    use vosu_sys::process::WindowsProcessMemory;

    let marker: Box<u64> = Box::new(0x1122_3344_5566_7788);
    let address = std::ptr::from_ref::<u64>(&marker) as u64;
    let pid = std::process::id();
    let memory = WindowsProcessMemory::open(pid).unwrap();
    let bitness = memory.bitness().unwrap();

    let submit = ReadPlanSubmit {
        plan: 13,
        handle: 1,
        root: PlanBase::Absolute(address),
        stages: vec![Stage {
            base: StageBase::Root,
            base_offset: 0,
            ops: vec![op(0, 8)],
        }],
        sync: false,
    };

    let result = execute(&submit, &memory, bitness, &NoModules, far_deadline()).unwrap();
    assert_eq!(
        result.results,
        vec![ReadResult::Ok(marker.to_le_bytes().to_vec())]
    );
}
