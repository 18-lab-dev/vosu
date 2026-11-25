//! The native backends implement the SDK's single read vocabulary
//! (`vosu_plugin_api::memory`): no second error/status type, no conversion
//! layer, and the SDK's ordered defaults keep their pointer-width and
//! publication semantics over a real process.

use std::sync::Arc;

use vosu_plugin_api::memory::{
    ChainReadRequest, IndirectReadRequest, MemoryError, ProcessBitness, ProcessMemory,
    read_chains_default, read_indirect_many_default,
};

fn native_self() -> Arc<dyn ProcessMemory> {
    let pid = std::process::id();
    #[cfg(target_os = "linux")]
    let memory = super::open_process(pid).unwrap();
    #[cfg(windows)]
    let memory = super::WindowsProcessMemory::open(pid).unwrap();
    assert_eq!(super::process_bitness(pid).unwrap(), ProcessBitness::X64);
    Arc::new(memory)
}

#[test]
fn native_backend_runs_sdk_default_chains_over_this_process() {
    let target = Box::new(0x1122_3344_5566_7788_u64);
    let holder = Box::new(std::ptr::from_ref::<u64>(&target) as usize);
    let holder_address = std::ptr::from_ref::<usize>(&holder) as usize;
    let memory = native_self();

    let mut value = [0; 8];
    memory
        .read_chain(holder_address, &[0], ProcessBitness::X64, &mut value)
        .unwrap();
    assert_eq!(u64::from_ne_bytes(value), *target);

    let mut direct = [0; 8];
    let mut resolved = 0;
    {
        let mut reads = [
            ChainReadRequest::new(holder_address, &[0], &mut value)
                .with_resolved_address(&mut resolved),
            ChainReadRequest::new(holder_address, &[], &mut direct),
        ];
        read_chains_default(&*memory, &mut reads, ProcessBitness::X64).unwrap();
    }
    assert_eq!(resolved, *holder);
    assert_eq!(usize::from_ne_bytes(direct), *holder);

    let mut pointee = [0; 4];
    read_indirect_many_default(
        &*memory,
        &mut [IndirectReadRequest::new(holder_address, 4, &mut pointee)],
        ProcessBitness::X64,
    )
    .unwrap();
    assert_eq!(pointee, 0x1122_3344_u32.to_ne_bytes());
}

#[test]
fn native_failure_is_the_sdk_error_without_partial_success() {
    let memory = native_self();
    let mut dst = [0xee; 8];
    let error: MemoryError = memory.read_exact(usize::MAX - 3, &mut dst).unwrap_err();
    assert!(
        matches!(
            error,
            MemoryError::AddressOverflow { .. } | MemoryError::Read { .. }
        ),
        "{error:?}"
    );
    let mut resolved = 0x55;
    let failed = {
        let mut reads = [ChainReadRequest::new(usize::MAX - 3, &[0], &mut dst)
            .with_resolved_address(&mut resolved)];
        read_chains_default(&*memory, &mut reads, ProcessBitness::X64)
    };
    assert!(failed.is_err());
    assert_eq!(resolved, 0x55);
}
