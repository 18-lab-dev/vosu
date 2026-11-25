//! Native objects behind one `vosu plugin-host` channel
//! (`docs/v2/plugin-abi.md` §7.2): per direction, one fixed-size shared
//! region and one wakeup doorbell. This module owns only the OS objects and
//! a safe, atomics-only view of the shared bytes. The ring protocol, frame
//! codec and every check on peer-written data live in `vosu-core`'s
//! `plugin::process`.
//!
//! # Endpoint contract (frozen for M5 L0/W0)
//!
//! A ring direction is one [`RingObjects`]: a [`SharedRegion`] of a trusted,
//! locally recorded length plus a [`Doorbell`].
//!
//! - **Parent.** Core calls [`RingObjects::create`] once per direction with
//!   core's `plugin::process::ring::REGION_BYTES`, initializes each region's
//!   header, and passes `&RingObjects` for both directions to the platform
//!   spawn code. It then gives the same objects to the core channel. Created
//!   objects are never inheritable by default: close-on-exec descriptors on
//!   Linux, handles created without an inheritable security descriptor on
//!   Windows.
//! - **Spawn (L0 Linux, W0 Windows).** The spawn code makes exactly these
//!   four objects available to exactly one child. On Linux it uses
//!   `RingObjects::mapping_fd` and `RingObjects::doorbell_fd` and
//!   duplicates them into the child after `fork`; on Windows it uses
//!   `RingObjects::mapping_handle` and `RingObjects::doorbell_handle` in
//!   an explicit `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`. It also records their
//!   child-side values in the launch descriptor. It never names, publishes
//!   or leaks the objects to any other process.
//! - **Child.** The entry code turns the inherited values into owned
//!   descriptors/handles (`OwnedFd`/`OwnedHandle`) and calls
//!   `RingObjects::from_inherited(mapping, doorbell, REGION_BYTES)` per
//!   direction. On Linux that refuses a mapping that is not a size-sealed
//!   regular file of exactly `REGION_BYTES`, or a doorbell that is not an
//!   `eventfd`. On Windows it refuses a section smaller than `REGION_BYTES`.
//!   The child then attaches core's channel with the directions swapped.
//!
//! Neither side ever trusts a length read from the peer's memory: the
//! mapped length is fixed when mapping. Peer-written header words and bytes
//! are only ever read atomically and validated before use.

mod region;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

pub use region::{OutOfBounds, SharedRegion};

#[cfg(target_os = "linux")]
pub use linux::{
    ChildStage, Doorbell, ENDPOINT_ARGUMENT, ENDPOINT_FDS, EndpointError, InheritedDirection,
    InheritedEndpoints, ParentDeathError, PluginHostChild, PluginHostCommand, PluginHostLauncher,
    ReadbackMismatch, SecurityCode, SecurityFailure, SecurityReadback, SecurityStep, SpawnError,
    arm_parent_death, drop_privileges,
};
#[cfg(windows)]
pub use windows::Doorbell;
/// Windows launch (W0, §7.4): strict job, suspended spawn with exactly the
/// four ring objects inherited, and the child's validated endpoint adoption.
#[cfg(windows)]
pub use windows::{
    CurrentJob, EndpointError, EndpointValues, InheritedEndpoints, JobProcessIds, KILL_EXIT_CODE,
    PLUGIN_JOB_LIMIT_FLAGS, PluginHostProcess, PluginJob, SpawnError, SpawnStep, UNWIND_EXIT_CODE,
    UNWIND_REAP_BOUND, UnwoundChild, adopt_inherited, current_job, spawn_plugin_host,
};

#[cfg(any(target_os = "linux", windows))]
mod objects {
    use std::io;

    use super::{Doorbell, SharedRegion, region::Backing};

    /// One ring direction's OS objects: its shared region and its doorbell.
    #[derive(Debug)]
    pub struct RingObjects {
        region: SharedRegion,
        doorbell: Doorbell,
    }

    impl RingObjects {
        /// Creates an anonymous, non-inheritable region of exactly `len`
        /// bytes (a nonzero multiple of 8) and a fresh doorbell.
        pub fn create(len: usize) -> io::Result<Self> {
            let region = {
                #[cfg(target_os = "linux")]
                let mapping = super::linux::create_mapping(len)?;
                #[cfg(windows)]
                let mapping = super::windows::create_mapping(len)?;
                wrap(mapping, len)?
            };
            Ok(Self {
                region,
                doorbell: Doorbell::create()?,
            })
        }

        /// The shared region.
        pub fn region(&self) -> &SharedRegion {
            &self.region
        }

        /// The doorbell.
        pub fn doorbell(&self) -> &Doorbell {
            &self.doorbell
        }

        /// Splits into the region and doorbell the core channel owns.
        pub fn into_parts(self) -> (SharedRegion, Doorbell) {
            (self.region, self.doorbell)
        }
    }

    #[cfg(target_os = "linux")]
    impl RingObjects {
        /// Adopts one inherited direction: a size-sealed `memfd` of exactly
        /// `expected_len` bytes and an `eventfd`.
        pub fn from_inherited(
            mapping: std::os::fd::OwnedFd,
            doorbell: std::os::fd::OwnedFd,
            expected_len: usize,
        ) -> io::Result<Self> {
            let mapping = super::linux::adopt_mapping(mapping, expected_len)?;
            Ok(Self {
                region: wrap(mapping, expected_len)?,
                doorbell: Doorbell::from_inherited(doorbell)?,
            })
        }

        /// The region's `memfd`, for the spawn code to duplicate into the
        /// child.
        pub fn mapping_fd(&self) -> std::os::fd::BorrowedFd<'_> {
            match self.region.backing() {
                Backing::Memfd(mapping) => mapping.fd(),
                Backing::Heap { .. } => unreachable!("RingObjects regions are always memfd-backed"),
            }
        }

        /// The doorbell's `eventfd`, for the spawn code to duplicate into
        /// the child.
        pub fn doorbell_fd(&self) -> std::os::fd::BorrowedFd<'_> {
            self.doorbell.as_fd()
        }
    }

    #[cfg(windows)]
    impl RingObjects {
        /// Adopts one inherited direction: a section of at least
        /// `expected_len` bytes, of which exactly `expected_len` are mapped,
        /// and an event.
        pub fn from_inherited(
            mapping: std::os::windows::io::OwnedHandle,
            doorbell: std::os::windows::io::OwnedHandle,
            expected_len: usize,
        ) -> io::Result<Self> {
            let mapping = super::windows::adopt_mapping(mapping, expected_len)?;
            Ok(Self {
                region: wrap(mapping, expected_len)?,
                doorbell: Doorbell::from_inherited(doorbell)?,
            })
        }

        /// The section handle, for the spawn code's inherited-handle list.
        pub fn mapping_handle(&self) -> std::os::windows::io::BorrowedHandle<'_> {
            match self.region.backing() {
                Backing::Section(mapping) => mapping.handle(),
                Backing::Heap { .. } => {
                    unreachable!("RingObjects regions are always section-backed")
                }
            }
        }

        /// The event handle, for the spawn code's inherited-handle list.
        pub fn doorbell_handle(&self) -> std::os::windows::io::BorrowedHandle<'_> {
            self.doorbell.as_handle()
        }
    }

    #[cfg(target_os = "linux")]
    fn wrap(mapping: super::linux::Mapping, len: usize) -> io::Result<SharedRegion> {
        let base = mapping.base();
        // SAFETY: `base` is a page-aligned `MAP_SHARED` mapping of exactly
        // `len` bytes whose file size is sealed, owned by `mapping`, which the
        // region keeps alive; nothing else in this process references it.
        unsafe { SharedRegion::from_mapping(base, len, Backing::Memfd(mapping)) }
    }

    #[cfg(windows)]
    fn wrap(mapping: super::windows::Mapping, len: usize) -> io::Result<SharedRegion> {
        let base = mapping.base();
        // SAFETY: `base` is a 64 KiB-aligned view of at least `len` committed
        // bytes owned by `mapping`, which the region keeps alive; nothing else
        // in this process references it.
        unsafe { SharedRegion::from_mapping(base, len, Backing::Section(mapping)) }
    }
}

#[cfg(any(target_os = "linux", windows))]
pub use objects::RingObjects;

#[cfg(test)]
mod tests;
