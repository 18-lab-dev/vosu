//! The fixture's own OS code: its load-time constructor, its module path,
//! the security readback, stopping its own process, and forging a frame
//! into the plugin → host ring the way a hostile plugin could.
//!
//! This is deliberately plugin code, not a `vosu-sys` helper: everything
//! here is what any third-party library loaded into a plugin host can do.
//! The ring constants mirror the region layout of
//! `crates/vosu-core/src/plugin/process/ring.rs` (plugin-abi.md §7.2) and
//! the frame head of `frame.rs`; the corrupt-frame smoke test fails if they
//! drift.

/// Ring region header bytes before the data area.
const HEADER_BYTES: u64 = 256;
/// Data bytes per direction.
const CAPACITY: u64 = 2 * 1024 * 1024;
const CAPACITY_WORD: u64 = 0;
const MAGIC_WORD: u64 = 8;
const WRITE_CURSOR: u64 = 64;
const MAGIC: u64 = u64::from_le_bytes(*b"vosuRNG1");
const HEAD_BYTES: usize = 24;

/// A frame head whose `total_len` (20) disagrees with its `payload_len`
/// (8): every consumer must refuse it as a length mismatch.
fn forged_head() -> [u8; HEAD_BYTES] {
    let mut head = [0_u8; HEAD_BYTES];
    head[0..4].copy_from_slice(&20_u32.to_le_bytes());
    head[4..8]
        .copy_from_slice(&(vosu_plugin_api::vosu_opcode::VOSU_OP_LOG_RECORD as u32).to_le_bytes());
    head[20..24].copy_from_slice(&8_u32.to_le_bytes());
    head
}

/// The plugin host's endpoint argument: its last command-line argument.
fn endpoint_argument() -> Option<String> {
    std::env::args().next_back()
}

#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(windows)]
pub use windows::*;

#[cfg(target_os = "linux")]
mod linux {
    use std::fs::File;
    use std::io::Write as _;
    use std::mem::ManuallyDrop;
    use std::os::fd::FromRawFd as _;
    use std::os::unix::fs::FileExt as _;
    use std::path::PathBuf;

    use super::{
        CAPACITY, CAPACITY_WORD, HEAD_BYTES, HEADER_BYTES, MAGIC, MAGIC_WORD, WRITE_CURSOR,
        endpoint_argument, forged_head,
    };

    const SIGSTOP: i32 = 19;
    const CLONE_NEWUSER: i32 = 0x1000_0000;
    const EPERM: i32 = 1;

    unsafe extern "C" {
        fn setns(fd: i32, flags: i32) -> i32;
        fn raise(signal: i32) -> i32;
        fn fork() -> i32;
        fn unshare(flags: i32) -> i32;
        fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
        fn _exit(code: i32) -> !;
        fn __errno_location() -> *mut i32;
    }

    /// Registered in `.init_array`: the dynamic loader runs it while it
    /// maps this library, before `vosu_plugin_entry_v1` is resolved.
    #[used]
    #[unsafe(link_section = ".init_array")]
    static CONSTRUCTOR: extern "C" fn() = constructor;

    extern "C" fn constructor() {
        crate::control::on_load(library_path(), &readback());
    }

    /// This library's file, from the mapping that holds this function.
    /// The plugin host loads `/proc/self/fd/<n>`, and the mapping names the
    /// file that descriptor opened.
    fn library_path() -> Option<PathBuf> {
        let address = constructor as *const () as usize;
        let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
        maps.lines().find_map(|line| {
            let mut fields = line.splitn(6, ' ');
            let (start, end) = fields.next()?.split_once('-')?;
            let start = usize::from_str_radix(start, 16).ok()?;
            let end = usize::from_str_radix(end, 16).ok()?;
            let path = fields.nth(4)?.trim_start();
            (start..end)
                .contains(&address)
                .then(|| PathBuf::from(path))
                .filter(|path| path.is_absolute())
        })
    }

    /// The security state as this process sees it now: UID, all five
    /// capability sets, `NoNewPrivs`, `Seccomp`, the thread count and the
    /// user-namespace probe ([`userns_probe`]).
    pub fn readback() -> String {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let mut fields = Vec::new();
        for key in [
            "Uid",
            "Threads",
            "NoNewPrivs",
            "Seccomp",
            "CapInh",
            "CapPrm",
            "CapEff",
            "CapBnd",
            "CapAmb",
        ] {
            let value = status
                .lines()
                .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
                .map(|value| value.split_whitespace().collect::<Vec<_>>().join(","))
                .unwrap_or_else(|| "missing".to_owned());
            fields.push(format!("{}={value}", key.to_ascii_lowercase()));
        }
        fields.push(format!("userns={}", userns_probe()));
        // SAFETY: an invalid descriptor cannot change namespaces.
        let setns_errno = unsafe {
            if setns(-1, CLONE_NEWUSER) == 0 {
                0
            } else {
                *__errno_location()
            }
        };
        fields.push(format!("setns_errno={setns_errno}"));
        fields.join(" ")
    }

    /// `unshare(CLONE_NEWUSER)` in a forked child, which is single-threaded
    /// even when this process is not (a multithreaded caller always gets
    /// `EINVAL`), and whose namespace never affects this process: `EPERM`
    /// when refused, `succeeded` when not, otherwise `errno<N>`.
    fn userns_probe() -> String {
        // SAFETY: the child makes only raw async-signal-safe calls
        // (`unshare`, reading errno, `_exit`) and never returns.
        let pid = unsafe { fork() };
        if pid == 0 {
            // SAFETY: as above; errno is this thread's own.
            unsafe {
                let code = if unshare(CLONE_NEWUSER) == 0 {
                    0
                } else {
                    *__errno_location()
                };
                _exit(code.clamp(0, 255));
            }
        }
        if pid < 0 {
            return "fork_failed".to_owned();
        }
        let mut status = 0;
        // SAFETY: waits for the child forked above, into a local status.
        if unsafe { waitpid(pid, &mut status, 0) } != pid || status & 0x7f != 0 {
            return "wait_failed".to_owned();
        }
        match (status >> 8) & 0xff {
            0 => "succeeded".to_owned(),
            EPERM => "EPERM".to_owned(),
            errno => format!("errno{errno}"),
        }
    }

    /// A Linux child has no console to suppress.
    pub fn without_console(_command: &mut std::process::Command) {}

    /// Ends this process abnormally at once: `SIGABRT`.
    pub fn crash() -> ! {
        std::process::abort()
    }

    /// Stops every thread of this process, the ring reader included.
    pub fn stop_process() {
        // SAFETY: `raise` is async-signal-safe and only signals this
        // process; SIGSTOP cannot be caught, so no handler runs.
        unsafe {
            raise(SIGSTOP);
        }
    }

    /// Writes [`forged_head`] at the plugin → host ring's published write
    /// cursor, publishes it, and rings core's doorbell. Returns the cursor.
    pub fn corrupt_plugin_to_host() -> Result<String, String> {
        let argument = endpoint_argument().ok_or("no endpoint argument")?;
        let fds: Vec<i32> = argument
            .split(',')
            .map(str::parse)
            .collect::<Result<_, _>>()
            .map_err(|_| format!("endpoint argument {argument:?} is not four descriptors"))?;
        let [_, _, mapping, doorbell] = fds[..] else {
            return Err(format!(
                "endpoint argument {argument:?} is not four descriptors"
            ));
        };
        let ring = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!("/proc/self/fd/{mapping}"))
            .map_err(|error| format!("reopening the ring memfd: {error}"))?;
        let word = |offset: u64| -> Result<u64, String> {
            let mut bytes = [0_u8; 8];
            ring.read_exact_at(&mut bytes, offset)
                .map_err(|error| error.to_string())?;
            Ok(u64::from_le_bytes(bytes))
        };
        if word(CAPACITY_WORD)? != CAPACITY || word(MAGIC_WORD)? != MAGIC {
            return Err("descriptor does not hold the expected ring layout".to_owned());
        }
        let write = word(WRITE_CURSOR)?;
        let head = forged_head();
        for (at, byte) in head.iter().enumerate() {
            let index = (write + at as u64) & (CAPACITY - 1);
            ring.write_all_at(&[*byte], HEADER_BYTES + index)
                .map_err(|error| error.to_string())?;
        }
        ring.write_all_at(&(write + HEAD_BYTES as u64).to_le_bytes(), WRITE_CURSOR)
            .map_err(|error| error.to_string())?;
        // SAFETY: `doorbell` is the plugin host's live plugin → host eventfd
        // (its fixed endpoint descriptor), which it keeps open for its
        // whole life; `ManuallyDrop` never closes it here.
        let mut bell = ManuallyDrop::new(unsafe { File::from_raw_fd(doorbell) });
        bell.write_all(&1_u64.to_ne_bytes())
            .map_err(|error| format!("ringing the doorbell: {error}"))?;
        Ok(format!("cursor={write}"))
    }
}

#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStringExt as _;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        CAPACITY, CAPACITY_WORD, HEAD_BYTES, HEADER_BYTES, MAGIC, MAGIC_WORD, WRITE_CURSOR,
        endpoint_argument, forged_head,
    };

    const DLL_PROCESS_ATTACH: u32 = 1;
    const FILE_MAP_WRITE: u32 = 2;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleFileNameW(module: *mut c_void, name: *mut u16, size: u32) -> u32;
        fn GetCurrentProcess() -> *mut c_void;
        fn MapViewOfFile(
            section: *mut c_void,
            access: u32,
            offset_high: u32,
            offset_low: u32,
            bytes: usize,
        ) -> *mut c_void;
        fn UnmapViewOfFile(base: *const c_void) -> i32;
        fn SetEvent(event: *mut c_void) -> i32;
        fn QueryInformationJobObject(
            job: *mut c_void,
            class: i32,
            info: *mut c_void,
            length: u32,
            returned: *mut u32,
        ) -> i32;
    }

    const JOB_OBJECT_BASIC_PROCESS_ID_LIST: i32 = 3;
    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    /// `JOBOBJECT_EXTENDED_LIMIT_INFORMATION`.
    #[repr(C)]
    struct ExtendedLimit {
        _per_process_user_time: i64,
        _per_job_user_time: i64,
        limit_flags: u32,
        _minimum_working_set: usize,
        _maximum_working_set: usize,
        _active_process_limit: u32,
        _affinity: usize,
        _priority_class: u32,
        _scheduling_class: u32,
        _io_counters: [u64; 6],
        _process_memory_limit: usize,
        _job_memory_limit: usize,
        _peak_process_memory: usize,
        _peak_job_memory: usize,
    }

    /// `JOBOBJECT_BASIC_PROCESS_ID_LIST` with room for 32 IDs.
    #[repr(C)]
    struct ProcessIdList {
        assigned: u32,
        listed: u32,
        ids: [usize; 32],
    }

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtSuspendProcess(process: *mut c_void) -> i32;
    }

    /// The loader calls this while it maps the library, under the loader
    /// lock, before `vosu_plugin_entry_v1` is resolved.
    #[unsafe(no_mangle)]
    extern "system" fn DllMain(module: *mut c_void, reason: u32, _reserved: *mut c_void) -> i32 {
        if reason == DLL_PROCESS_ATTACH {
            crate::control::on_load(library_path(module), &readback());
        }
        1
    }

    fn library_path(module: *mut c_void) -> Option<PathBuf> {
        let mut buffer = vec![0_u16; 32_768];
        // SAFETY: `module` is this DLL's handle from `DllMain`; the buffer
        // is writable for exactly the length passed.
        let len = unsafe { GetModuleFileNameW(module, buffer.as_mut_ptr(), buffer.len() as u32) };
        let len = usize::try_from(len).ok()?;
        (len > 0 && len < buffer.len())
            .then(|| PathBuf::from(std::ffi::OsString::from_wide(&buffer[..len])))
    }

    /// Windows has no capability readback. This is this process's own
    /// immediate job, read with a null job handle: its limit flags and
    /// the processes it holds (`job=none` outside any job). The test reads
    /// token and job identities independently too.
    pub fn readback() -> String {
        // SAFETY: all-zero is a valid value of this plain C struct.
        let mut limits: ExtendedLimit = unsafe { std::mem::zeroed() };
        // SAFETY: a null job names this process's immediate job; the buffer
        // is writable for exactly the length passed.
        let limited = unsafe {
            QueryInformationJobObject(
                std::ptr::null_mut(),
                JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                (&raw mut limits).cast(),
                size_of::<ExtendedLimit>() as u32,
                std::ptr::null_mut(),
            )
        };
        if limited == 0 {
            return "job=none".to_owned();
        }
        let mut list = ProcessIdList {
            assigned: 0,
            listed: 0,
            ids: [0; 32],
        };
        // SAFETY: as above, for the process-ID list.
        let listed = unsafe {
            QueryInformationJobObject(
                std::ptr::null_mut(),
                JOB_OBJECT_BASIC_PROCESS_ID_LIST,
                (&raw mut list).cast(),
                size_of::<ProcessIdList>() as u32,
                std::ptr::null_mut(),
            )
        };
        let pids = if listed == 0 {
            "unreadable".to_owned()
        } else {
            list.ids[..(list.listed as usize).min(list.ids.len())]
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(",")
        };
        format!(
            "job_flags={:#x} job_assigned={} job_pids={pids}",
            limits.limit_flags, list.assigned
        )
    }

    /// Starts a console child without a console window: the plugin host
    /// has none to share.
    pub fn without_console(command: &mut std::process::Command) {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    /// Ends this process abnormally at once with the real fail-fast of
    /// `std::process::abort` (`STATUS_STACK_BUFFER_OVERRUN`), as on Linux.
    /// Windows Error Reporting used to hold such a process for 1–6 s before
    /// it exited, which turned a startup crash into a startup timeout; the
    /// plugin host now sets its error mode so the fail-fast exits promptly
    /// (plugin-abi.md §7.4 "Crash reporting"), and these startup crash
    /// modes are the evidence for it.
    pub fn crash() -> ! {
        std::process::abort()
    }

    /// Suspends every thread of this process, the ring reader included.
    pub fn stop_process() {
        // SAFETY: `GetCurrentProcess` returns this process's pseudo handle,
        // which carries every access right `NtSuspendProcess` needs.
        unsafe {
            NtSuspendProcess(GetCurrentProcess());
        }
    }

    fn parse_handles(argument: &str) -> Option<(usize, usize)> {
        let p2h = argument
            .split(',')
            .find_map(|part| part.strip_prefix("p2h="))?;
        let (section, event) = p2h.split_once(':')?;
        Some((
            usize::from_str_radix(section, 16).ok()?,
            usize::from_str_radix(event, 16).ok()?,
        ))
    }

    /// Writes [`forged_head`] at the plugin → host ring's published write
    /// cursor through a second view of the inherited section, publishes it,
    /// and signals core's event. Returns the cursor.
    pub fn corrupt_plugin_to_host() -> Result<String, String> {
        let argument = endpoint_argument().ok_or("no endpoint argument")?;
        let (section, event) = parse_handles(&argument)
            .ok_or_else(|| format!("endpoint argument {argument:?} names no p2h handles"))?;
        let len = (HEADER_BYTES + CAPACITY) as usize;
        // SAFETY: `section` is the plugin host's live plugin → host section
        // handle, which it keeps open for its whole life; a failed map
        // returns null, checked below.
        let base = unsafe { MapViewOfFile(section as *mut c_void, FILE_MAP_WRITE, 0, 0, len) };
        if base.is_null() {
            return Err(format!(
                "mapping section {section:#x}: {}",
                std::io::Error::last_os_error()
            ));
        }
        let base = base.cast::<u8>();
        let word = |offset: u64| {
            // SAFETY: `offset` is one of the 8-aligned header words inside
            // the `len`-byte view, which stays mapped until the unmap below;
            // the peer accesses these words atomically too.
            unsafe { &*base.add(offset as usize).cast::<AtomicU64>() }
        };
        let result = if word(CAPACITY_WORD).load(Ordering::Acquire) != CAPACITY
            || word(MAGIC_WORD).load(Ordering::Acquire) != MAGIC
        {
            Err("section does not hold the expected ring layout".to_owned())
        } else {
            let write = word(WRITE_CURSOR).load(Ordering::Acquire);
            for (at, byte) in forged_head().iter().enumerate() {
                let index = (write + at as u64) & (CAPACITY - 1);
                // SAFETY: `HEADER_BYTES + index` is inside the data area of
                // the mapped view; this process is the ring's only producer
                // and nothing else writes the free bytes past its cursor.
                unsafe {
                    base.add((HEADER_BYTES + index) as usize)
                        .write_volatile(*byte)
                };
            }
            word(WRITE_CURSOR).store(write + HEAD_BYTES as u64, Ordering::Release);
            Ok(format!("cursor={write}"))
        };
        // SAFETY: `base` is the view mapped above, unmapped exactly once.
        unsafe { UnmapViewOfFile(base.cast()) };
        // SAFETY: `event` is the plugin host's live plugin → host event
        // handle; a failure only leaves core unwoken, reported below.
        if result.is_ok() && unsafe { SetEvent(event as *mut c_void) } == 0 {
            return Err(format!(
                "signalling event {event:#x}: {}",
                std::io::Error::last_os_error()
            ));
        }
        result
    }
}
