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

    unsafe extern "C" {
        fn raise(signal: i32) -> i32;
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
    /// capability sets, `NoNewPrivs`, `Seccomp` and the thread count.
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
        fields.join(" ")
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

    /// Windows has no capability readback; the test reads token and job
    /// identities independently.
    pub fn readback() -> String {
        String::new()
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
