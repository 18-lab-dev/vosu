//! Private transport between core and one `vosu plugin-host` child
//! (`docs/v2/plugin-abi.md` §7.2, A5.2 private control frames in §7.6).
//!
//! - [`frame`]: the exact little-endian frame head
//!   (`total_len, opcode, flags, correlation, payload_len`), the 1 MiB
//!   payload cap, and the per-direction [`frame::DirectionPolicy`] for public
//!   opcodes, the private control set and its pairing rules.
//! - [`ring`]: one direction's single-producer/single-consumer byte ring
//!   over a `vosu_sys::plugin_process::SharedRegion`, validating every
//!   peer-written cursor and head before use.
//! - [`channel`]: one side's serialized producer, single consumer,
//!   doorbell-driven waits, 2-second backpressure window and termination.
//! - `linux` (Linux only): core's launch of one plugin host with exactly its
//!   channel objects, and the child's adoption of them (§7.3, §7.6).
//! - `windows_launch` (Windows only): core's launch of one confined child
//!   with both directions' objects inherited, and the child's attach (W0).
//! - [`control`]: the `CREATE`/`READY`/`FAILED` payloads, startup stages and
//!   the child's private exit statuses (§7.6, M5 E0).
//! - [`entry`]: the `vosu plugin-host` child entry `src/main.rs` dispatches
//!   to before any runtime exists; `bootstrap` (Linux/Windows) is the child
//!   after its channel exists: `CREATE`, verified library load, `create`,
//!   `READY`, controlled destroy.
//! - `launcher` (Linux/Windows): core's `ProcessLauncher` that starts one
//!   child per instance through `linux`/`windows_launch` and supervises it;
//!   its `ingress` serves the plugin → host direction off the ring reader.
//! - The child's relay (Linux/Windows, M5 T1): `relay` (the plugin's
//!   `send`, the ring reader and the dispatch workers), `waiters` (the 64
//!   shared status and `SYNC` waiters), `admission` (the per-instance caps
//!   enforced before writing) and `delivery` (the non-blocking, semantically
//!   coalescing delivery queue).
//!
//! The OS objects (mapping and doorbell per direction) come from
//! `vosu_sys::plugin_process::RingObjects`, whose module documents the
//! frozen endpoint contract L0/W0 implement. Core owns the protocol.
//!
//! **Budgets.** Each direction's region is [`ring::REGION_BYTES`]
//! (2 MiB data + 256 bytes header), so one instance maps 4 MiB + 512 bytes,
//! shared by core and its child (one set of physical pages). Each receiver
//! owns one reusable 8-byte-aligned [`ring::PayloadBuffer`], growing only to
//! the largest validated payload (≤ 1 MiB). Senders allocate nothing.
//!
//! **Copies.** Exactly two per frame, both unavoidable across the process
//! boundary: the sender copies the caller's payload into the ring, and the
//! receiver copies it out into its aligned buffer before any decoder or
//! `dispatch` sees it. Nothing ever points into the wrapping ring. Flat
//! payloads are never re-encoded. A received buffer then moves (by swap,
//! not copy) to its waiter, delivery entry or core ingress item.

#[cfg(any(target_os = "linux", windows))]
mod admission;
#[cfg(any(target_os = "linux", windows))]
mod bootstrap;
#[cfg(any(target_os = "linux", windows))]
pub mod channel;
pub mod control;
#[cfg(any(target_os = "linux", windows))]
mod delivery;
pub mod entry;
#[cfg(all(test, any(target_os = "linux", windows)))]
mod fake_plugin;
pub mod frame;
#[cfg(any(target_os = "linux", windows))]
pub mod launcher;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(any(target_os = "linux", windows))]
mod relay;
pub mod ring;
#[cfg(any(target_os = "linux", windows))]
mod waiters;
#[cfg(windows)]
pub mod windows_launch;
