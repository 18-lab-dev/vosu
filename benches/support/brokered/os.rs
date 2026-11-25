//! Per-process measurement for the brokered-reads runner: allocation
//! counting, own CPU time and context switches, host load on the CPUs the
//! run may use, and CPU affinity. Each process measures only itself; the
//! plugin side reports its figures to core in its summary file.
//!
//! - **CPU**: Linux `getrusage(RUSAGE_SELF)` user+system; Windows
//!   `GetProcessTimes` kernel+user, which Windows charges by clock-tick
//!   sampling, so Windows also reports exact executed cycles
//!   (`QueryProcessCycleTime`).
//! - **Context switches** (the `wakeups` metric): Linux voluntary switches
//!   (`ru_nvcsw`, one per blocking wait that actually slept); Windows the sum
//!   of the process's threads' `ContextSwitches`
//!   (`NtQuerySystemInformation(SystemProcessInformation)`), which also
//!   counts preemptions.
//! - **Host load**: busy CPU time of the CPUs in this process's affinity
//!   (Linux `/proc/stat` `cpuN` lines) or of the whole machine (Windows
//!   `GetSystemTimes`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

/// Counts every allocation (and reallocation) this process makes.
pub struct CountingAllocator;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

// SAFETY: every method forwards to `System` with the caller's arguments
// unchanged, so `System`'s `GlobalAlloc` contract carries over; the wrapper
// only adds a relaxed counter increment, which neither allocates nor panics.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: forwarded under the caller's `alloc` contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: forwarded under the caller's `alloc_zeroed` contract.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: forwarded under the caller's `realloc` contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded under the caller's `dealloc` contract.
        unsafe { System.dealloc(ptr, layout) }
    }
}

pub fn allocations() -> u64 {
    ALLOCATIONS.load(Ordering::Relaxed)
}

/// One process's own counters at an instant.
#[derive(Clone, Copy, Debug, Default)]
pub struct Usage {
    pub cpu_ns: u64,
    /// Windows `QueryProcessCycleTime` (exact executed cycles, unlike the
    /// tick-sampled `GetProcessTimes`); always 0 on Linux.
    pub cpu_cycles: u64,
    pub context_switches: u64,
    pub allocations: u64,
}

impl Usage {
    /// Opens a window: the context-switch reading (which allocates and,
    /// on Windows, costs a system-wide query) first, then CPU, then the
    /// allocation count, so none of the readings' own work lands inside.
    pub fn begin() -> Self {
        let context_switches = platform::context_switches();
        let cpu_ns = platform::cpu_ns();
        let cpu_cycles = platform::cpu_cycles();
        Self {
            cpu_ns,
            cpu_cycles,
            context_switches,
            allocations: allocations(),
        }
    }

    /// Closes a window, mirroring [`begin`](Self::begin).
    pub fn end() -> Self {
        let allocations = allocations();
        let cpu_cycles = platform::cpu_cycles();
        let cpu_ns = platform::cpu_ns();
        Self {
            cpu_ns,
            cpu_cycles,
            context_switches: platform::context_switches(),
            allocations,
        }
    }

    pub fn since(self, earlier: Self) -> Self {
        Self {
            cpu_ns: self.cpu_ns.saturating_sub(earlier.cpu_ns),
            cpu_cycles: self.cpu_cycles.saturating_sub(earlier.cpu_cycles),
            context_switches: self
                .context_switches
                .saturating_sub(earlier.context_switches),
            allocations: self.allocations.saturating_sub(earlier.allocations),
        }
    }

    pub fn add(&mut self, delta: Self) {
        self.cpu_ns += delta.cpu_ns;
        self.cpu_cycles += delta.cpu_cycles;
        self.context_switches += delta.context_switches;
        self.allocations += delta.allocations;
    }
}

/// Busy CPU time, in nanoseconds, of the CPUs `cpus` names (Linux) or of
/// the whole machine (Windows).
pub fn host_busy_ns(cpus: &[usize]) -> u64 {
    platform::host_busy_ns(cpus)
}

/// This process's CPU affinity, as sorted CPU indices.
pub fn own_affinity() -> Vec<usize> {
    platform::own_affinity()
}

/// `child`'s CPU affinity, as sorted CPU indices.
pub fn child_affinity(child: &std::process::Child) -> Vec<usize> {
    platform::child_affinity(child)
}

/// Formats sorted CPU indices as a range list, e.g. `0-7,16-23`.
pub fn cpu_list(cpus: &[usize]) -> String {
    let mut parts = Vec::new();
    let mut index = 0;
    while index < cpus.len() {
        let start = cpus[index];
        let mut end = start;
        while index + 1 < cpus.len() && cpus[index + 1] == end + 1 {
            index += 1;
            end = cpus[index];
        }
        parts.push(if start == end {
            start.to_string()
        } else {
            format!("{start}-{end}")
        });
        index += 1;
    }
    parts.join(",")
}

#[cfg(target_os = "linux")]
mod platform {
    fn rusage() -> libc::rusage {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: `RUSAGE_SELF` with a correctly sized, zero-initialized out
        // pointer is `getrusage`'s documented contract.
        let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
        assert_eq!(result, 0, "getrusage(RUSAGE_SELF) failed");
        // SAFETY: getrusage succeeded, so every field is initialized.
        unsafe { usage.assume_init() }
    }

    pub fn cpu_ns() -> u64 {
        let usage = rusage();
        let micros = |time: libc::timeval| time.tv_sec as u64 * 1_000_000 + time.tv_usec as u64;
        (micros(usage.ru_utime) + micros(usage.ru_stime)) * 1_000
    }

    pub fn cpu_cycles() -> u64 {
        0
    }

    pub fn context_switches() -> u64 {
        rusage().ru_nvcsw as u64
    }

    pub fn host_busy_ns(cpus: &[usize]) -> u64 {
        // SAFETY: `sysconf` has no memory-safety preconditions.
        let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        assert!(ticks_per_second > 0, "sysconf(_SC_CLK_TCK)");
        let stat = std::fs::read_to_string("/proc/stat").expect("read /proc/stat");
        let mut busy_ticks = 0u64;
        for line in stat.lines() {
            let mut fields = line.split_ascii_whitespace();
            let Some(cpu) = fields
                .next()
                .and_then(|name| name.strip_prefix("cpu"))
                .and_then(|index| index.parse::<usize>().ok())
            else {
                continue;
            };
            if !cpus.contains(&cpu) {
                continue;
            }
            let values: Vec<u64> = fields.map(|value| value.parse().unwrap_or(0)).collect();
            // user nice system idle iowait irq softirq steal
            busy_ticks += [0, 1, 2, 5, 6, 7]
                .iter()
                .filter_map(|&index| values.get(index))
                .sum::<u64>();
        }
        busy_ticks * 1_000_000_000 / ticks_per_second as u64
    }

    fn affinity_of(pid: libc::pid_t) -> Vec<usize> {
        let mut set = std::mem::MaybeUninit::<libc::cpu_set_t>::zeroed();
        // SAFETY: `set` is a zeroed, correctly sized `cpu_set_t`; the kernel
        // writes at most `size_of::<cpu_set_t>()` bytes into it.
        let result =
            unsafe { libc::sched_getaffinity(pid, size_of::<libc::cpu_set_t>(), set.as_mut_ptr()) };
        assert_eq!(result, 0, "sched_getaffinity({pid})");
        // SAFETY: sched_getaffinity succeeded and initialized the set.
        let set = unsafe { set.assume_init() };
        (0..libc::CPU_SETSIZE as usize)
            // SAFETY: `cpu` is below `CPU_SETSIZE`, the set's bit count.
            .filter(|&cpu| unsafe { libc::CPU_ISSET(cpu, &set) })
            .collect()
    }

    pub fn own_affinity() -> Vec<usize> {
        affinity_of(0)
    }

    pub fn child_affinity(child: &std::process::Child) -> Vec<usize> {
        affinity_of(child.id() as libc::pid_t)
    }
}

#[cfg(windows)]
mod platform {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Wdk::System::SystemInformation::{
        NtQuerySystemInformation, SystemProcessInformation,
    };
    use windows_sys::Win32::Foundation::{FILETIME, HANDLE, STATUS_INFO_LENGTH_MISMATCH};
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentProcessId, GetProcessAffinityMask, GetProcessTimes,
        GetSystemTimes,
    };
    use windows_sys::Win32::System::WindowsProgramming::{
        QueryProcessCycleTime, SYSTEM_PROCESS_INFORMATION, SYSTEM_THREAD_INFORMATION,
    };

    fn hundreds_ns(time: FILETIME) -> u64 {
        (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
    }

    pub fn cpu_ns() -> u64 {
        let mut times = [FILETIME::default(); 4];
        let [creation, exit, kernel, user] = &mut times;
        // SAFETY: the pseudo-handle names this process; all four out pointers
        // are valid, writable `FILETIME`s.
        let ok = unsafe { GetProcessTimes(GetCurrentProcess(), creation, exit, kernel, user) };
        assert_ne!(ok, 0, "GetProcessTimes");
        (hundreds_ns(times[2]) + hundreds_ns(times[3])) * 100
    }

    pub fn cpu_cycles() -> u64 {
        let mut cycles = 0u64;
        // SAFETY: the pseudo-handle names this process; `cycles` is a valid
        // out pointer.
        let ok = unsafe { QueryProcessCycleTime(GetCurrentProcess(), &mut cycles) };
        assert_ne!(ok, 0, "QueryProcessCycleTime");
        cycles
    }

    /// The sum of this process's threads' `ContextSwitches`
    /// (`SYSTEM_THREAD_INFORMATION`'s third reserved word in `winternl.h`,
    /// named `ContextSwitches` in the documented `ntddk` layout).
    pub fn context_switches() -> u64 {
        // SAFETY: no preconditions.
        let pid = unsafe { GetCurrentProcessId() } as usize;
        let mut capacity = 1 << 20;
        loop {
            let mut storage = vec![0u64; capacity / 8];
            let mut returned = 0u32;
            // SAFETY: `storage` is writable for `capacity` bytes and
            // `returned` is a valid out pointer.
            let status = unsafe {
                NtQuerySystemInformation(
                    SystemProcessInformation,
                    storage.as_mut_ptr().cast(),
                    capacity as u32,
                    &mut returned,
                )
            };
            if status == STATUS_INFO_LENGTH_MISMATCH {
                capacity = (returned as usize + (1 << 16)).max(capacity * 2);
                continue;
            }
            assert!(status >= 0, "NtQuerySystemInformation: {status:#x}");
            let len = (returned as usize).min(capacity);
            // SAFETY: `storage` holds `len` initialized bytes and outlives
            // the borrow.
            let bytes = unsafe { std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), len) };
            return context_switches_of(bytes, pid);
        }
    }

    fn context_switches_of(bytes: &[u8], pid: usize) -> u64 {
        let process_size = size_of::<SYSTEM_PROCESS_INFORMATION>();
        let thread_size = size_of::<SYSTEM_THREAD_INFORMATION>();
        let mut offset = 0usize;
        loop {
            assert!(
                offset + process_size <= bytes.len(),
                "process entry in bounds"
            );
            // SAFETY: the entry lies within `bytes` (checked above);
            // `read_unaligned` tolerates any alignment.
            let process = unsafe {
                bytes
                    .as_ptr()
                    .add(offset)
                    .cast::<SYSTEM_PROCESS_INFORMATION>()
                    .read_unaligned()
            };
            if process.UniqueProcessId as usize == pid {
                let threads = process.NumberOfThreads as usize;
                let first = offset + process_size;
                assert!(
                    first + threads * thread_size <= bytes.len(),
                    "thread entries in bounds"
                );
                return (0..threads)
                    .map(|index| {
                        // SAFETY: entry `index` lies within `bytes` (checked
                        // above); `read_unaligned` tolerates any alignment.
                        let thread = unsafe {
                            bytes
                                .as_ptr()
                                .add(first + index * thread_size)
                                .cast::<SYSTEM_THREAD_INFORMATION>()
                                .read_unaligned()
                        };
                        u64::from(thread.Reserved3)
                    })
                    .sum();
            }
            if process.NextEntryOffset == 0 {
                panic!("own process {pid} missing from SystemProcessInformation");
            }
            offset += process.NextEntryOffset as usize;
        }
    }

    pub fn host_busy_ns(_cpus: &[usize]) -> u64 {
        let mut idle = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: all three out pointers are valid, writable `FILETIME`s.
        let ok = unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) };
        assert_ne!(ok, 0, "GetSystemTimes");
        // Kernel time includes idle time.
        (hundreds_ns(kernel) + hundreds_ns(user)).saturating_sub(hundreds_ns(idle)) * 100
    }

    fn affinity_of(process: HANDLE) -> Vec<usize> {
        let mut process_mask = 0usize;
        let mut system_mask = 0usize;
        // SAFETY: `process` is a live process handle with query access; both
        // out pointers are valid.
        let ok = unsafe { GetProcessAffinityMask(process, &mut process_mask, &mut system_mask) };
        assert_ne!(ok, 0, "GetProcessAffinityMask");
        (0..usize::BITS as usize)
            .filter(|bit| process_mask & (1 << bit) != 0)
            .collect()
    }

    pub fn own_affinity() -> Vec<usize> {
        // SAFETY: no preconditions; the pseudo-handle needs no closing.
        affinity_of(unsafe { GetCurrentProcess() })
    }

    pub fn child_affinity(child: &std::process::Child) -> Vec<usize> {
        affinity_of(child.as_raw_handle() as HANDLE)
    }
}
