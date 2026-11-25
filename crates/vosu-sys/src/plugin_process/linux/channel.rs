//! Ownership of one plugin host's inherited endpoint descriptors
//! (`docs/v2/plugin-abi.md` §7.2 "Channel setup", §7.6 "Launch").
//!
//! The spawn code places exactly four descriptors at fixed numbers in the
//! child: per direction (host → plugin first) the region's `memfd`, then its
//! `eventfd` doorbell. Their numbers also travel on the command line as one
//! argument, [`ENDPOINT_ARGUMENT`]. The child adopts them once with
//! [`InheritedEndpoints::adopt`], which refuses anything else, and marks
//! them close-on-exec at once so no process the plugin starts inherits the
//! channel.

use std::ffi::OsStr;
use std::io;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};

use super::super::RingObjects;

/// Child descriptor numbers, in argument order: host → plugin mapping and
/// doorbell, then plugin → host mapping and doorbell. 0–2 are the null
/// device.
pub const ENDPOINT_FDS: [RawFd; 4] = [3, 4, 5, 6];

/// The endpoint argument the spawn code passes: [`ENDPOINT_FDS`] in
/// canonical decimal, comma-separated.
pub const ENDPOINT_ARGUMENT: &str = "3,4,5,6";

/// Why the inherited endpoints were refused (startup stage `endpoints`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EndpointError {
    #[error("endpoint argument is not four canonical comma-separated descriptors")]
    Malformed,
    #[error("endpoint descriptors are not exactly the launch layout {ENDPOINT_ARGUMENT}")]
    Layout,
    #[error("endpoints were already adopted by this process")]
    AlreadyAdopted,
    #[error("endpoint descriptor {fd} is not open (errno {errno})")]
    NotOpen { fd: RawFd, errno: i32 },
    #[error("endpoint descriptor {fd} is close-on-exec, so it was not inherited")]
    NotInherited { fd: RawFd },
    #[error("cannot mark endpoint descriptor {fd} close-on-exec (errno {errno})")]
    Cloexec { fd: RawFd, errno: i32 },
}

/// One inherited direction's descriptors.
#[derive(Debug)]
pub struct InheritedDirection {
    pub mapping: OwnedFd,
    pub doorbell: OwnedFd,
}

impl InheritedDirection {
    /// Validates the descriptors' kind and the mapping's sealed size, then
    /// maps it (`RingObjects::from_inherited`).
    pub fn into_ring_objects(self, expected_len: usize) -> io::Result<RingObjects> {
        RingObjects::from_inherited(self.mapping, self.doorbell, expected_len)
    }
}

/// The four descriptors core handed this plugin host.
#[derive(Debug)]
pub struct InheritedEndpoints {
    pub host_to_plugin: InheritedDirection,
    pub plugin_to_host: InheritedDirection,
}

/// Set by the one successful adoption; a second call is refused.
static ADOPTED: AtomicBool = AtomicBool::new(false);

impl InheritedEndpoints {
    /// Takes ownership of the endpoint descriptors `argument` names.
    ///
    /// Refuses a malformed or noncanonical argument, any layout other than
    /// [`ENDPOINT_FDS`], a closed descriptor, and a descriptor that is
    /// close-on-exec (it cannot have crossed `execve`, so something in this
    /// process opened it). Marks each adopted descriptor close-on-exec.
    /// Succeeds at most once per process. Kind and size are checked by
    /// [`InheritedDirection::into_ring_objects`] before anything is mapped.
    pub fn adopt(argument: &OsStr) -> Result<Self, EndpointError> {
        let fds = parse_argument(argument)?;
        if fds != ENDPOINT_FDS {
            return Err(EndpointError::Layout);
        }
        for &fd in &fds {
            // SAFETY: `F_GETFD` only reads the descriptor's flags.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            if flags < 0 {
                return Err(EndpointError::NotOpen { fd, errno: errno() });
            }
            if flags & libc::FD_CLOEXEC != 0 {
                return Err(EndpointError::NotInherited { fd });
            }
        }
        if ADOPTED.swap(true, Ordering::AcqRel) {
            return Err(EndpointError::AlreadyAdopted);
        }
        for &fd in &fds {
            // SAFETY: sets the close-on-exec flag of an open descriptor.
            if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
                return Err(EndpointError::Cloexec { fd, errno: errno() });
            }
        }
        // SAFETY: each descriptor is open and was inherited across `execve`
        // (it was not close-on-exec), so no object this process created owns
        // it, and `ADOPTED` guarantees this is the only adoption. Every
        // inherited descriptor in a plugin host is one of these four (the
        // spawn code closes the rest), so nothing else claims them.
        let [h2p_mapping, h2p_doorbell, p2h_mapping, p2h_doorbell] =
            fds.map(|fd| unsafe { OwnedFd::from_raw_fd(fd) });
        Ok(Self {
            host_to_plugin: InheritedDirection {
                mapping: h2p_mapping,
                doorbell: h2p_doorbell,
            },
            plugin_to_host: InheritedDirection {
                mapping: p2h_mapping,
                doorbell: p2h_doorbell,
            },
        })
    }
}

/// Parses exactly four canonical decimal descriptors (no sign, no leading
/// zero, no whitespace), each above 2 and all distinct.
pub(super) fn parse_argument(argument: &OsStr) -> Result<[RawFd; 4], EndpointError> {
    let text = argument.to_str().ok_or(EndpointError::Malformed)?;
    let mut fds = [0 as RawFd; 4];
    let mut parts = text.split(',');
    for slot in &mut fds {
        let part = parts.next().ok_or(EndpointError::Malformed)?;
        let canonical = !part.is_empty()
            && part.bytes().all(|byte| byte.is_ascii_digit())
            && (part == "0" || !part.starts_with('0'));
        if !canonical {
            return Err(EndpointError::Malformed);
        }
        *slot = part.parse().map_err(|_| EndpointError::Malformed)?;
        if *slot <= 2 {
            return Err(EndpointError::Malformed);
        }
    }
    if parts.next().is_some() {
        return Err(EndpointError::Malformed);
    }
    for (index, fd) in fds.iter().enumerate() {
        if fds[index + 1..].contains(fd) {
            return Err(EndpointError::Malformed);
        }
    }
    Ok(fds)
}

fn errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}
