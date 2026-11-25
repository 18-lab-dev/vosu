//! Empty reads complete without preparing or issuing any native I/O.

use super::*;

#[derive(Debug)]
struct NoSyscall;

impl ProcessVmReadv for NoSyscall {
    fn readv(
        &self,
        _pid: libc::pid_t,
        _local: &mut [libc::iovec],
        _remote: &[libc::iovec],
    ) -> Result<usize, i32> {
        panic!("empty reads must not issue process_vm_readv")
    }

    fn iov_max(&self) -> usize {
        panic!("empty reads must not prepare an iovec batch")
    }
}

#[test]
fn empty_reads_do_not_prepare_or_issue_native_io() {
    let memory = LinuxProcessMemory::with_parts(20, PathBuf::new(), Arc::new(NoSyscall));

    memory.read_exact(usize::MAX, &mut []).expect("exact read");
    memory
        .read_scanner_region_exact(usize::MAX, &mut [])
        .expect("scanner read");
    let mut empty = [];
    let mut other_empty = [];
    let mut reads = [
        ReadRequest::new(usize::MAX, &mut empty),
        ReadRequest::new(usize::MAX, &mut other_empty),
    ];
    for count in [1, 2] {
        memory.read_many(&mut reads[..count]).expect("batched read");
    }
}
