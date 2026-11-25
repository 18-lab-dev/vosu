//! A shared-memory region accessed only through 8-byte atomics.
//!
//! The peer process can write any byte of the region at any time. This
//! process therefore never forms a non-atomic reference into it: every load
//! and store, including the bulk byte copies, goes through one same-sized
//! `AtomicU64` access per 8-byte word. No access ever partially overlaps
//! another of a different size, so two concurrent accesses inside this
//! process are both atomic, and a hostile peer can change only the values
//! this process reads, never make an access undefined. Callers validate
//! every value they load before using it as a length or an index.

use std::io;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};

const WORD: usize = size_of::<u64>();

/// An access outside the region's trusted, locally recorded length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("shared-region access at {offset} of {len} bytes is outside the {region_len}-byte region")]
pub struct OutOfBounds {
    pub offset: usize,
    pub len: usize,
    pub region_len: usize,
}

/// What keeps a region's memory alive and releases it on drop.
pub(super) enum Backing {
    /// In-process memory: tests and same-process peers. Never shared with
    /// another process.
    Heap(#[expect(dead_code, reason = "owns the allocation `base` points into")] Box<[AtomicU64]>),
    /// A `memfd` mapping; it also owns the descriptor spawn code inherits.
    #[cfg(target_os = "linux")]
    Memfd(super::linux::Mapping),
    /// A section view; it also owns the handle spawn code inherits.
    #[cfg(windows)]
    Section(super::windows::Mapping),
}

/// A fixed-length shared byte region whose length is recorded locally when
/// it is mapped. Nothing the peer writes can change that length.
pub struct SharedRegion {
    base: NonNull<AtomicU64>,
    len: usize,
    backing: Backing,
}

// SAFETY: `base` points into memory owned by `backing` for as long as this
// value lives. That memory is touched only through `AtomicU64` operations,
// which are safe to perform from any thread.
unsafe impl Send for SharedRegion {}
// SAFETY: see `Send`; every method takes `&self` and accesses the memory
// atomically.
unsafe impl Sync for SharedRegion {}

impl std::fmt::Debug for SharedRegion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let backing = match self.backing {
            Backing::Heap(_) => "heap",
            #[cfg(target_os = "linux")]
            Backing::Memfd(_) => "memfd",
            #[cfg(windows)]
            Backing::Section(_) => "section",
        };
        formatter
            .debug_struct("SharedRegion")
            .field("len", &self.len)
            .field("backing", &backing)
            .finish()
    }
}

impl SharedRegion {
    /// A zeroed in-process region of `len` bytes (a nonzero multiple of 8).
    /// Never shared with another process; for tests and in-process peers.
    pub fn heap(len: usize) -> io::Result<Self> {
        check_len(len)?;
        let words: Box<[AtomicU64]> = (0..len / WORD).map(|_| AtomicU64::new(0)).collect();
        let base = NonNull::from(&words[0]);
        Ok(Self {
            base,
            len,
            backing: Backing::Heap(words),
        })
    }

    /// Wraps a platform mapping.
    ///
    /// # Safety
    ///
    /// `base` must be 8-byte aligned and valid for reads and writes of
    /// `len` bytes for as long as `backing` lives, and this process must
    /// access that memory only through the returned region.
    pub(super) unsafe fn from_mapping(
        base: NonNull<u8>,
        len: usize,
        backing: Backing,
    ) -> io::Result<Self> {
        check_len(len)?;
        if !(base.as_ptr() as usize).is_multiple_of(WORD) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "shared mapping is not 8-byte aligned",
            ));
        }
        Ok(Self {
            base: base.cast(),
            len,
            backing,
        })
    }

    pub(super) fn backing(&self) -> &Backing {
        &self.backing
    }

    /// The trusted length recorded when the region was mapped.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Always false: a region is at least one word long.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The atomic 8-byte word at byte `offset`, which must be 8-aligned and
    /// inside the region.
    pub fn word(&self, offset: usize) -> Result<&AtomicU64, OutOfBounds> {
        let out_of_bounds = OutOfBounds {
            offset,
            len: WORD,
            region_len: self.len,
        };
        if !offset.is_multiple_of(WORD) || offset >= self.len {
            return Err(out_of_bounds);
        }
        Ok(self.word_at(offset / WORD))
    }

    /// Copies `dst.len()` bytes starting at byte `offset` out of the region.
    pub fn read(&self, offset: usize, dst: &mut [u8]) -> Result<(), OutOfBounds> {
        self.check_range(offset, dst.len())?;
        let mut position = offset;
        let mut done = 0;
        while done < dst.len() {
            let within = position % WORD;
            let take = (WORD - within).min(dst.len() - done);
            let bytes = self
                .word_at(position / WORD)
                .load(Ordering::Relaxed)
                .to_ne_bytes();
            dst[done..done + take].copy_from_slice(&bytes[within..within + take]);
            position += take;
            done += take;
        }
        Ok(())
    }

    /// Copies `src` into the region starting at byte `offset`. Bytes of a
    /// partially covered word outside `src`'s range are preserved
    /// atomically, so a concurrent reader of those bytes is unaffected.
    pub fn write(&self, offset: usize, src: &[u8]) -> Result<(), OutOfBounds> {
        self.check_range(offset, src.len())?;
        let mut position = offset;
        let mut done = 0;
        while done < src.len() {
            let within = position % WORD;
            let take = (WORD - within).min(src.len() - done);
            let word = self.word_at(position / WORD);
            let mut value = [0_u8; WORD];
            value[within..within + take].copy_from_slice(&src[done..done + take]);
            if take == WORD {
                word.store(u64::from_ne_bytes(value), Ordering::Relaxed);
            } else {
                let mut mask = [0_u8; WORD];
                mask[within..within + take].fill(0xff);
                word.fetch_and(!u64::from_ne_bytes(mask), Ordering::Relaxed);
                word.fetch_or(u64::from_ne_bytes(value), Ordering::Relaxed);
            }
            position += take;
            done += take;
        }
        Ok(())
    }

    fn check_range(&self, offset: usize, len: usize) -> Result<(), OutOfBounds> {
        match offset.checked_add(len) {
            Some(end) if end <= self.len => Ok(()),
            _ => Err(OutOfBounds {
                offset,
                len,
                region_len: self.len,
            }),
        }
    }

    fn word_at(&self, index: usize) -> &AtomicU64 {
        debug_assert!(index < self.len / WORD);
        // SAFETY: every caller derives `index` from a byte range already
        // checked against `len`, so `index < len / 8`; `base` is 8-aligned
        // and valid for `len` bytes while `backing` (owned by `self`) lives,
        // and `AtomicU64` has the size and alignment of `u64`.
        unsafe { &*self.base.as_ptr().add(index) }
    }
}

fn check_len(len: usize) -> io::Result<()> {
    if len == 0 || !len.is_multiple_of(WORD) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "shared region length must be a nonzero multiple of 8 bytes",
        ));
    }
    Ok(())
}
