//! Unnamed section and auto-reset event primitives.
//!
//! Both objects are created without a name and without an inheritable
//! security descriptor; only the spawn code makes the child's copies
//! inheritable. A pagefile-backed `SEC_COMMIT` section has a fixed size, so
//! the peer cannot shrink it under this process's view. The peer can set or
//! reset either event, which only changes when a waiter wakes: waiters
//! always recheck shared state, and `SetEvent` never blocks.

use std::io;
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    HANDLE, INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    PAGE_READWRITE, SEC_COMMIT, UnmapViewOfFile,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, INFINITE, SetEvent, WaitForSingleObject,
};

/// A read/write view of a section, unmapped on drop.
pub(in crate::plugin_process) struct Mapping {
    section: OwnedHandle,
    base: NonNull<u8>,
}

impl Mapping {
    pub(in crate::plugin_process) fn base(&self) -> NonNull<u8> {
        self.base
    }

    pub(in crate::plugin_process) fn handle(&self) -> BorrowedHandle<'_> {
        self.section.as_handle()
    }
}

// SAFETY: the view is plain shared memory; `SharedRegion` accesses it only
// atomically, and `UnmapViewOfFile` in `Drop` runs once from any thread.
unsafe impl Send for Mapping {}
// SAFETY: see `Send`; `Mapping` exposes no interior mutation itself.
unsafe impl Sync for Mapping {}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: `base` is the address `MapViewOfFile` returned; the owning
        // `SharedRegion` (and every reference into it) is being dropped.
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.base.as_ptr().cast(),
            });
        }
    }
}

/// Creates an unnamed, non-inheritable section of exactly `len` bytes and
/// maps it.
pub(in crate::plugin_process) fn create_mapping(len: usize) -> io::Result<Mapping> {
    let size = u64::try_from(len)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "ring region too large"))?;
    // SAFETY: a pagefile-backed (`INVALID_HANDLE_VALUE`) unnamed section with
    // default, non-inheritable security.
    let raw = unsafe {
        CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            std::ptr::null(),
            PAGE_READWRITE | SEC_COMMIT,
            (size >> 32) as u32,
            size as u32,
            std::ptr::null(),
        )
    };
    let section = owned(raw)?;
    map(section, len)
}

/// Maps an inherited section, refusing one smaller than `expected_len`
/// (`MapViewOfFile` fails rather than mapping past the section's end).
pub(in crate::plugin_process) fn adopt_mapping(
    section: OwnedHandle,
    expected_len: usize,
) -> io::Result<Mapping> {
    map(section, expected_len)
}

fn map(section: OwnedHandle, len: usize) -> io::Result<Mapping> {
    // SAFETY: `section` is a live section handle; the view covers `len`
    // bytes from offset 0 and the system chooses its (64 KiB-aligned) base.
    let view = unsafe {
        MapViewOfFile(
            section.as_raw_handle() as HANDLE,
            FILE_MAP_READ | FILE_MAP_WRITE,
            0,
            0,
            len,
        )
    };
    let base = NonNull::new(view.Value.cast::<u8>()).ok_or_else(io::Error::last_os_error)?;
    Ok(Mapping { section, base })
}

/// One direction's wakeup primitive: an unnamed auto-reset event the peer
/// sets after publishing and this side waits on. A set is a hint, never a
/// message: waiters always recheck shared state after waking.
#[derive(Debug)]
pub struct Doorbell {
    event: OwnedHandle,
}

impl Doorbell {
    /// A new unnamed, non-inheritable, auto-reset, initially clear event.
    pub fn create() -> io::Result<Self> {
        // SAFETY: default security, auto-reset (`bManualReset = 0`), initially
        // nonsignaled, no name.
        let raw = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        Ok(Self { event: owned(raw)? })
    }

    /// Adopts an inherited event handle. The spawn code that recovered the
    /// handle value from the launch descriptor is responsible for it naming
    /// the inherited event.
    pub fn from_inherited(event: OwnedHandle) -> io::Result<Self> {
        Ok(Self { event })
    }

    /// The handle the spawn code makes inheritable for the child.
    pub fn as_handle(&self) -> BorrowedHandle<'_> {
        self.event.as_handle()
    }

    /// Wakes the waiter. `SetEvent` never blocks.
    pub fn ring(&self) -> io::Result<()> {
        // SAFETY: `event` is a live event handle.
        if unsafe { SetEvent(self.raw()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Consumes a pending ring, waiting up to `timeout` (`None`: no limit)
    /// for one. Returns whether a ring was consumed.
    pub fn wait(&self, timeout: Option<Duration>) -> io::Result<bool> {
        // `None` inside `Some` is a deadline too far away to represent.
        let deadline = timeout.map(|timeout| Instant::now().checked_add(timeout));
        loop {
            let millis = match deadline {
                None | Some(None) => INFINITE,
                Some(Some(deadline)) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    // Round up so a sub-millisecond remainder still waits;
                    // stay below `INFINITE` and loop for longer waits.
                    u32::try_from(remaining.as_nanos().div_ceil(1_000_000))
                        .unwrap_or(INFINITE - 1)
                        .min(INFINITE - 1)
                }
            };
            match self.wait_once(millis)? {
                true => return Ok(true),
                false if millis == 0 => return Ok(false),
                false => {}
            }
        }
    }

    /// Consumes a pending ring without waiting.
    pub fn consume(&self) -> io::Result<bool> {
        self.wait_once(0)
    }

    fn wait_once(&self, millis: u32) -> io::Result<bool> {
        // SAFETY: `event` is a live waitable handle.
        match unsafe { WaitForSingleObject(self.raw(), millis) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            WAIT_FAILED => Err(io::Error::last_os_error()),
            other => Err(io::Error::other(format!(
                "unexpected doorbell wait result {other:#x}"
            ))),
        }
    }

    fn raw(&self) -> HANDLE {
        self.event.as_raw_handle() as HANDLE
    }
}

fn owned(raw: HANDLE) -> io::Result<OwnedHandle> {
    if raw.is_null() || raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a fresh handle returned by a create call that this function
    // now owns exclusively.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw.cast()) })
}
