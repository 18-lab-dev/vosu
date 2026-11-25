//! `memfd` mapping and `eventfd` doorbell primitives.
//!
//! The peer holds the same open file descriptions, so it can change their
//! status flags (for example clear `O_NONBLOCK`) or saturate an `eventfd`
//! counter. Nothing here relies on those flags: reads use `RWF_NOWAIT`,
//! waits use `poll`, and a doorbell is written only after `poll` reports it
//! writable. The mapping's size is sealed, so the peer cannot shrink the
//! file under this process's mapping and turn an in-bounds access into
//! `SIGBUS`.

use std::ffi::CStr;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::ptr::NonNull;
use std::time::{Duration, Instant};

const MEMFD_NAME: &CStr = c"vosu-plugin-ring";
/// Seals that make the mapping's size immutable for every holder.
const REQUIRED_SEALS: libc::c_int = libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;
const EVENTFD_LINK: &[u8] = b"anon_inode:[eventfd]";

/// A `MAP_SHARED` view of a sealed `memfd`, unmapped on drop.
pub(in crate::plugin_process) struct Mapping {
    fd: OwnedFd,
    base: NonNull<u8>,
    len: usize,
}

impl Mapping {
    pub(in crate::plugin_process) fn base(&self) -> NonNull<u8> {
        self.base
    }

    pub(in crate::plugin_process) fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

// SAFETY: the mapping is plain shared memory; `SharedRegion` accesses it only
// atomically, and `munmap` in `Drop` runs once, from whichever thread drops it.
unsafe impl Send for Mapping {}
// SAFETY: see `Send`; `Mapping` exposes no interior mutation itself.
unsafe impl Sync for Mapping {}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: `base`/`len` are exactly the range `map` obtained from
        // `mmap`; the owning `SharedRegion` (and so every reference into it)
        // is being dropped.
        unsafe {
            libc::munmap(self.base.as_ptr().cast(), self.len);
        }
    }
}

/// Creates a close-on-exec `memfd` of exactly `len` bytes, seals its size,
/// and maps it.
pub(in crate::plugin_process) fn create_mapping(len: usize) -> io::Result<Mapping> {
    let size = libc::off_t::try_from(len)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "ring region too large"))?;
    // SAFETY: `MEMFD_NAME` is a NUL-terminated string; the flags are valid.
    let raw = unsafe {
        libc::memfd_create(
            MEMFD_NAME.as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `memfd_create` returned a new descriptor this function owns.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: `fd` is a valid descriptor; `size` is nonnegative.
    if unsafe { libc::ftruncate(fd.as_raw_fd(), size) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a valid memfd created with `MFD_ALLOW_SEALING`.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_ADD_SEALS, REQUIRED_SEALS) } != 0 {
        return Err(io::Error::last_os_error());
    }
    map(fd, len)
}

/// Maps an inherited `memfd`, refusing it unless it is a regular file of
/// exactly `expected_len` bytes whose size is sealed.
pub(in crate::plugin_process) fn adopt_mapping(
    fd: OwnedFd,
    expected_len: usize,
) -> io::Result<Mapping> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `fd` is valid; `stat` is writable storage for one `stat`.
    if unsafe { libc::fstat(fd.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fstat` succeeded, so it initialized `stat`.
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(invalid("inherited ring mapping is not a regular memfd"));
    }
    if usize::try_from(stat.st_size).ok() != Some(expected_len) {
        return Err(invalid("inherited ring mapping has an unexpected size"));
    }
    // SAFETY: `fd` is valid; `F_GET_SEALS` takes no argument.
    let seals = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GET_SEALS) };
    if seals < 0 {
        return Err(io::Error::last_os_error());
    }
    if seals & REQUIRED_SEALS != REQUIRED_SEALS {
        return Err(invalid("inherited ring mapping's size is not sealed"));
    }
    map(fd, expected_len)
}

fn map(fd: OwnedFd, len: usize) -> io::Result<Mapping> {
    // SAFETY: a fresh `MAP_SHARED` read/write mapping of `len` bytes of a
    // file whose size is sealed at exactly `len`; the kernel chooses the
    // (page-aligned) address.
    let base = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd.as_raw_fd(),
            0,
        )
    };
    if base == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    let base = NonNull::new(base.cast::<u8>()).ok_or_else(|| invalid("mmap returned null"))?;
    Ok(Mapping { fd, base, len })
}

/// One direction's wakeup primitive: an `eventfd` the peer rings after
/// publishing and this side waits on. A ring is a hint, never a message:
/// waiters always recheck shared state after waking, so a stale or extra
/// ring costs one empty check and a lost one is impossible while the
/// counter stays nonzero until consumed.
#[derive(Debug)]
pub struct Doorbell {
    fd: OwnedFd,
}

impl Doorbell {
    /// A new close-on-exec, nonblocking `eventfd` with a zero counter.
    pub fn create() -> io::Result<Self> {
        // SAFETY: plain `eventfd` call with valid flags.
        let raw = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `eventfd` returned a new descriptor this function owns.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        Ok(Self { fd })
    }

    /// Adopts an inherited descriptor after checking it is an `eventfd`.
    pub fn from_inherited(fd: OwnedFd) -> io::Result<Self> {
        let link = std::fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd()))?;
        if link.as_os_str().as_encoded_bytes() != EVENTFD_LINK {
            return Err(invalid("inherited doorbell is not an eventfd"));
        }
        Ok(Self { fd })
    }

    /// The descriptor the spawn code duplicates into the child.
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// Wakes the waiter. Never waits for the counter: a counter the peer has
    /// saturated already guarantees a pending wake, so it is left as is.
    pub fn ring(&self) -> io::Result<()> {
        if !self.poll(libc::POLLOUT, Some(Duration::ZERO))? {
            return Ok(());
        }
        let one = 1_u64.to_ne_bytes();
        loop {
            // SAFETY: writes 8 bytes from a live local buffer to a valid fd.
            let written = unsafe { libc::write(self.fd.as_raw_fd(), one.as_ptr().cast(), 8) };
            if written == 8 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => return Ok(()),
                _ => return Err(error),
            }
        }
    }

    /// Consumes a pending ring, waiting up to `timeout` (`None`: no limit)
    /// for one. Returns whether a ring was consumed.
    pub fn wait(&self, timeout: Option<Duration>) -> io::Result<bool> {
        // `None` inside `Some` is a deadline too far away to represent.
        let deadline = timeout.map(|timeout| Instant::now().checked_add(timeout));
        loop {
            // Consumed before every check of the deadline, so a ring that
            // raced the timeout is taken rather than left behind.
            if self.consume()? {
                return Ok(true);
            }
            let remaining = match deadline {
                None | Some(None) => None,
                Some(Some(deadline)) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Ok(false);
                    }
                    Some(remaining)
                }
            };
            self.poll(libc::POLLIN, remaining)?;
        }
    }

    /// Consumes a pending ring without waiting. Also releases a local
    /// writer the peer has made block on a saturated counter.
    pub fn consume(&self) -> io::Result<bool> {
        let mut counter = [0_u8; 8];
        let iov = libc::iovec {
            iov_base: counter.as_mut_ptr().cast(),
            iov_len: counter.len(),
        };
        loop {
            // SAFETY: one iovec over a live 8-byte local buffer; offset -1
            // means the current position, as for a non-seekable fd.
            let read = unsafe { libc::preadv2(self.fd.as_raw_fd(), &iov, 1, -1, libc::RWF_NOWAIT) };
            if read == 8 {
                return Ok(true);
            }
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::EINTR) => continue,
                Some(libc::EAGAIN) => return Ok(false),
                // Kernels before eventfd's `read_iter` lack `RWF_NOWAIT`
                // there; fall back to a readiness check plus plain read.
                Some(libc::EOPNOTSUPP | libc::EINVAL) => return self.consume_fallback(),
                _ => return Err(error),
            }
        }
    }

    fn consume_fallback(&self) -> io::Result<bool> {
        if !self.poll(libc::POLLIN, Some(Duration::ZERO))? {
            return Ok(false);
        }
        let mut counter = [0_u8; 8];
        loop {
            // SAFETY: reads at most 8 bytes into a live local buffer.
            let read = unsafe { libc::read(self.fd.as_raw_fd(), counter.as_mut_ptr().cast(), 8) };
            if read == 8 {
                return Ok(true);
            }
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => return Ok(false),
                _ => return Err(error),
            }
        }
    }

    /// One `poll` for `events`, waiting up to `timeout` (`None`: no limit).
    /// Returns whether `events` became ready; an interrupted wait reports
    /// not ready, and callers recheck their own deadline.
    fn poll(&self, events: libc::c_short, timeout: Option<Duration>) -> io::Result<bool> {
        let millis = timeout.map_or(-1, |timeout| {
            // Round up so a sub-millisecond remainder still waits.
            libc::c_int::try_from(timeout.as_nanos().div_ceil(1_000_000))
                .unwrap_or(libc::c_int::MAX)
        });
        let mut poll = libc::pollfd {
            fd: self.fd.as_raw_fd(),
            events,
            revents: 0,
        };
        // SAFETY: one valid `pollfd` on a live descriptor.
        let ready = unsafe { libc::poll(&mut poll, 1, millis) };
        if ready > 0 {
            if poll.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
                return Err(invalid("doorbell descriptor reported an error"));
            }
            return Ok(poll.revents & events != 0);
        }
        if ready == 0 {
            return Ok(false);
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(false);
        }
        Err(error)
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
