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
//! payloads are never re-encoded.

#[cfg(any(target_os = "linux", windows))]
pub mod channel;
pub mod frame;
pub mod ring;
