//! Native process-memory trait operations, kept separate from process discovery.
use super::*;

impl ProcessMemory for WindowsProcessMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        read_exact_impl(address, dst, |address, dst| self.raw_read(address, dst))
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        read_many_impl(reads, |address, dst| self.raw_read(address, dst))
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        let mut regions = Vec::new();
        let mut address = 0usize;
        loop {
            // SAFETY: MEMORY_BASIC_INFORMATION is plain integers and pointers, so all-zero is valid.
            let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: info is a correctly sized writable output buffer.
            let returned = unsafe {
                VirtualQueryEx(
                    self.handle.raw(),
                    address as *const c_void,
                    &mut info,
                    size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            if returned == 0 {
                break;
            }
            // Signature scans intentionally mirror the source backend: only writable
            // committed mappings participate, avoiding false positives in image pages.
            if info.State == MEM_COMMIT
                && info.Protect & (PAGE_READWRITE | PAGE_EXECUTE_READWRITE) != 0
            {
                regions.push(MemoryRegion {
                    address: info.BaseAddress as usize,
                    size: info.RegionSize,
                });
            }
            let next = (info.BaseAddress as usize)
                .checked_add(info.RegionSize)
                .ok_or(MemoryError::AddressOverflow {
                    base: info.BaseAddress as usize,
                    offset: info.RegionSize,
                })?;
            if next <= address {
                break;
            }
            address = next;
        }
        Ok(regions)
    }

    fn process_id(&self) -> Option<u32> {
        Some(self.pid)
    }

    fn is_alive(&self) -> bool {
        let mut exit_code = 0;
        // SAFETY: handle remains owned and exit_code is a valid writable pointer.
        unsafe {
            GetExitCodeProcess(self.handle.raw(), &mut exit_code) != 0
                && exit_code == STILL_ACTIVE as u32
        }
    }

    fn reads_pin_process_identity(&self) -> bool {
        // ReadProcessMemory uses our owned HANDLE, never a fresh PID lookup.
        true
    }
}
