use std::fmt;

use memchr::{memchr_iter, memmem};
use thiserror::Error;

use super::{MemoryError, MemoryRegion, ProcessHandle};

/// Number of new region bytes read per scanner window.
pub const SCAN_CHUNK_SIZE: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    bytes: Vec<u8>,
    mask: Vec<u8>,
    non_zero_mask: bool,
    anchor_offset: usize,
    anchor_len: usize,
}

impl Pattern {
    /// Parses space-separated bytes. A token is either exactly two hexadecimal
    /// digits or the wildcard `??`.
    pub fn parse(value: &str, non_zero_mask: bool) -> Result<Self, PatternParseError> {
        let mut bytes = Vec::new();
        let mut mask = Vec::new();

        for (index, token) in value.split_whitespace().enumerate() {
            if token == "??" {
                bytes.push(0);
                mask.push(0);
                continue;
            }

            if token.len() != 2 {
                return Err(PatternParseError::InvalidToken {
                    index,
                    token: token.to_owned(),
                });
            }

            let byte =
                u8::from_str_radix(token, 16).map_err(|_| PatternParseError::InvalidToken {
                    index,
                    token: token.to_owned(),
                })?;
            bytes.push(byte);
            mask.push(1);
        }

        let (anchor_offset, anchor_len) = longest_fixed_run(&mask);
        Ok(Self {
            bytes,
            mask,
            non_zero_mask,
            anchor_offset,
            anchor_len,
        })
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn mask(&self) -> &[u8] {
        &self.mask
    }

    #[must_use]
    pub const fn non_zero_mask(&self) -> bool {
        self.non_zero_mask
    }

    fn matches_at(&self, bytes: &[u8], offset: usize) -> bool {
        self.bytes
            .iter()
            .zip(&self.mask)
            .enumerate()
            .all(|(index, (&expected, &mask))| {
                let actual = bytes[offset + index];
                if mask != 0 {
                    actual == expected
                } else if self.non_zero_mask {
                    actual != 0
                } else {
                    true
                }
            })
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PatternParseError {
    #[error(
        "invalid signature token {token:?} at index {index}; expected two hexadecimal digits or ??"
    )]
    InvalidToken { index: usize, token: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchScanResult {
    pub index: usize,
    pub address: usize,
}

impl ProcessHandle {
    /// Returns the first match in region order, then ascending offset order.
    pub fn scan_first(&self, pattern: &Pattern) -> Result<Option<usize>, MemoryError> {
        let regions = self.memory.readable_regions()?;
        let mut scratch = ScanScratch::new(pattern.bytes.len())?;

        for region in &regions {
            let mut first = None;
            if scan_region(
                self,
                *region,
                pattern.bytes.len(),
                &mut scratch,
                |window, base, starts| {
                    if first.is_none() {
                        first = find_first_candidate(pattern, window, starts)
                            .map(|offset| checked_offset_add(base, offset))
                            .transpose()?;
                    }
                    Ok(())
                },
            )? {
                if pattern.bytes.is_empty() {
                    return Ok(Some(region.address));
                }
                if let Some(offset) = first {
                    return Ok(Some(region_address(*region, offset)?));
                }
            }
        }

        Ok(None)
    }

    /// Scans candidates in address order and stops at the first candidate
    /// accepted by `validate`, without materializing every signature match.
    pub fn scan_first_valid(
        &self,
        pattern: &Pattern,
        mut validate: impl FnMut(usize) -> Result<bool, MemoryError>,
    ) -> Result<Option<usize>, MemoryError> {
        if pattern.bytes.is_empty() {
            return Ok(None);
        }
        let regions = self.memory.readable_regions()?;
        let mut scratch = ScanScratch::new(pattern.bytes.len())?;
        for region in regions {
            let mut accepted = None;
            let outcome = scan_region_until(
                self,
                region,
                pattern.bytes.len(),
                &mut scratch,
                |window, base, starts| {
                    visit_candidates_until(pattern, window, starts, |offset| {
                        let relative = checked_offset_add(base, offset)?;
                        let address = region_address(region, relative)?;
                        if validate(address)? {
                            accepted = Some(address);
                            Ok(true)
                        } else {
                            Ok(false)
                        }
                    })
                },
            )?;
            if outcome == ScanRegionOutcome::Stopped {
                return Ok(accepted);
            }
        }
        Ok(None)
    }

    /// Returns every match in region order, then ascending offset order.
    /// As in the upstream scanner, an empty signature has no `scan_all` matches.
    pub fn scan_all(&self, pattern: &Pattern) -> Result<Vec<usize>, MemoryError> {
        if pattern.bytes.is_empty() {
            return Ok(Vec::new());
        }

        let regions = self.memory.readable_regions()?;
        let mut scratch = ScanScratch::new(pattern.bytes.len())?;
        let mut matches = Vec::new();

        for region in regions {
            let checkpoint = matches.len();
            let readable = scan_region(
                self,
                region,
                pattern.bytes.len(),
                &mut scratch,
                |window, base, starts| {
                    visit_candidates(pattern, window, starts, |offset| {
                        matches.push(checked_offset_add(base, offset)?);
                        Ok(())
                    })
                },
            )?;
            if !readable {
                matches.truncate(checkpoint);
                continue;
            }
            for offset in &mut matches[checkpoint..] {
                *offset = region_address(region, *offset)?;
            }
        }

        Ok(matches)
    }

    /// Finds at most one match per pattern. Regions are traversed first and
    /// unresolved patterns are emitted in caller-index order within a region.
    pub fn scan_batch(&self, patterns: &[Pattern]) -> Result<Vec<BatchScanResult>, MemoryError> {
        if patterns.is_empty() {
            return Ok(Vec::new());
        }

        let regions = self.memory.readable_regions()?;
        let max_len = patterns
            .iter()
            .map(|pattern| pattern.bytes.len())
            .max()
            .unwrap_or(0);
        let mut scratch = ScanScratch::new(max_len)?;
        let mut found = vec![false; patterns.len()];
        let mut results = Vec::with_capacity(patterns.len());

        for region in regions {
            let mut region_matches = vec![None; patterns.len()];
            let readable = scan_region(self, region, max_len, &mut scratch, |window, base, _| {
                for (index, pattern) in patterns.iter().enumerate() {
                    if found[index] || region_matches[index].is_some() || pattern.bytes.is_empty() {
                        continue;
                    }
                    region_matches[index] = find_first_candidate(
                        pattern,
                        window,
                        valid_starts(window.len(), pattern.bytes.len(), base),
                    )
                    .map(|offset| checked_offset_add(base, offset))
                    .transpose()?;
                }
                Ok(())
            })?;
            if !readable {
                continue;
            }

            for (index, pattern) in patterns.iter().enumerate() {
                if found[index] {
                    continue;
                }
                let offset = if pattern.bytes.is_empty() {
                    Some(0)
                } else {
                    region_matches[index]
                };
                if let Some(offset) = offset {
                    results.push(BatchScanResult {
                        index,
                        address: region_address(region, offset)?,
                    });
                    found[index] = true;
                }
            }
            if results.len() == patterns.len() {
                break;
            }
        }

        Ok(results)
    }
}

struct ScanScratch {
    bytes: Box<[u8]>,
}

impl ScanScratch {
    fn new(max_pattern_len: usize) -> Result<Self, MemoryError> {
        let overlap = max_pattern_len.saturating_sub(1);
        let capacity =
            SCAN_CHUNK_SIZE
                .checked_add(overlap)
                .ok_or(MemoryError::AddressOverflow {
                    base: SCAN_CHUNK_SIZE,
                    offset: overlap,
                })?;
        Ok(Self {
            bytes: vec![0; capacity].into_boxed_slice(),
        })
    }
}

/// Calls `visit` for each window in a region. A failed window discards the
/// whole region, matching the upstream behavior for an unreadable region.
fn scan_region(
    handle: &ProcessHandle,
    region: MemoryRegion,
    max_pattern_len: usize,
    scratch: &mut ScanScratch,
    mut visit: impl FnMut(&[u8], usize, usize) -> Result<(), MemoryError>,
) -> Result<bool, MemoryError> {
    let capacity = scratch.bytes.len();
    let mut base = 0usize;

    loop {
        let remaining = region.size.saturating_sub(base);
        let read_len = remaining.min(capacity);
        let window = &mut scratch.bytes[..read_len];
        let address = region_address(region, base)?;
        if handle
            .memory
            .read_scanner_region_exact(address, window)
            .is_err()
        {
            return Ok(false);
        }

        let starts = if max_pattern_len == 0 {
            0
        } else {
            valid_starts(read_len, max_pattern_len, base)
        };
        visit(window, base, starts)?;

        if remaining <= SCAN_CHUNK_SIZE || region.size == 0 {
            return Ok(true);
        }
        base = base
            .checked_add(SCAN_CHUNK_SIZE)
            .ok_or(MemoryError::AddressOverflow {
                base,
                offset: SCAN_CHUNK_SIZE,
            })?;

        debug_assert_eq!(scratch.bytes.len(), capacity);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanRegionOutcome {
    Complete,
    Unreadable,
    Stopped,
}

fn scan_region_until(
    handle: &ProcessHandle,
    region: MemoryRegion,
    max_pattern_len: usize,
    scratch: &mut ScanScratch,
    mut visit: impl FnMut(&[u8], usize, usize) -> Result<bool, MemoryError>,
) -> Result<ScanRegionOutcome, MemoryError> {
    let capacity = scratch.bytes.len();
    let mut base = 0usize;
    loop {
        let remaining = region.size.saturating_sub(base);
        let read_len = remaining.min(capacity);
        let window = &mut scratch.bytes[..read_len];
        let address = region_address(region, base)?;
        if handle
            .memory
            .read_scanner_region_exact(address, window)
            .is_err()
        {
            return Ok(ScanRegionOutcome::Unreadable);
        }
        let starts = if max_pattern_len == 0 {
            0
        } else {
            valid_starts(read_len, max_pattern_len, base)
        };
        if visit(window, base, starts)? {
            return Ok(ScanRegionOutcome::Stopped);
        }
        if remaining <= SCAN_CHUNK_SIZE || region.size == 0 {
            return Ok(ScanRegionOutcome::Complete);
        }
        base = base
            .checked_add(SCAN_CHUNK_SIZE)
            .ok_or(MemoryError::AddressOverflow {
                base,
                offset: SCAN_CHUNK_SIZE,
            })?;
        debug_assert_eq!(scratch.bytes.len(), capacity);
    }
}

fn valid_starts(window_len: usize, pattern_len: usize, _base: usize) -> usize {
    if pattern_len == 0 || window_len < pattern_len {
        return 0;
    }
    (window_len - pattern_len + 1).min(SCAN_CHUNK_SIZE)
}

fn find_first_candidate(pattern: &Pattern, window: &[u8], starts: usize) -> Option<usize> {
    let anchor_offset = pattern.anchor_offset;
    let anchor_len = pattern.anchor_len;
    if anchor_len == 0 {
        return (0..starts).find(|&offset| pattern.matches_at(window, offset));
    }

    let needle = &pattern.bytes[anchor_offset..anchor_offset + anchor_len];
    let candidate = |position: usize| {
        position.checked_sub(anchor_offset).and_then(|offset| {
            (offset < starts && pattern.matches_at(window, offset)).then_some(offset)
        })
    };
    if anchor_len == 1 {
        memchr_iter(needle[0], window).find_map(candidate)
    } else {
        memmem::find_iter(window, needle).find_map(candidate)
    }
}

fn visit_candidates(
    pattern: &Pattern,
    window: &[u8],
    starts: usize,
    mut visit: impl FnMut(usize) -> Result<(), MemoryError>,
) -> Result<(), MemoryError> {
    let anchor_offset = pattern.anchor_offset;
    let anchor_len = pattern.anchor_len;
    if anchor_len == 0 {
        for offset in 0..starts {
            if pattern.matches_at(window, offset) {
                visit(offset)?;
            }
        }
        return Ok(());
    }

    let needle = &pattern.bytes[anchor_offset..anchor_offset + anchor_len];
    if anchor_len == 1 {
        for position in memchr_iter(needle[0], window) {
            if position >= anchor_offset {
                let offset = position - anchor_offset;
                if offset < starts && pattern.matches_at(window, offset) {
                    visit(offset)?;
                }
            }
        }
    } else {
        for position in memmem::find_iter(window, needle) {
            if position >= anchor_offset {
                let offset = position - anchor_offset;
                if offset < starts && pattern.matches_at(window, offset) {
                    visit(offset)?;
                }
            }
        }
    }
    Ok(())
}

fn visit_candidates_until(
    pattern: &Pattern,
    window: &[u8],
    starts: usize,
    mut visit: impl FnMut(usize) -> Result<bool, MemoryError>,
) -> Result<bool, MemoryError> {
    let anchor_offset = pattern.anchor_offset;
    let anchor_len = pattern.anchor_len;
    if anchor_len == 0 {
        for offset in 0..starts {
            if pattern.matches_at(window, offset) && visit(offset)? {
                return Ok(true);
            }
        }
        return Ok(false);
    }

    let needle = &pattern.bytes[anchor_offset..anchor_offset + anchor_len];
    if anchor_len == 1 {
        for position in memchr_iter(needle[0], window) {
            if let Some(offset) = position.checked_sub(anchor_offset)
                && offset < starts
                && pattern.matches_at(window, offset)
                && visit(offset)?
            {
                return Ok(true);
            }
        }
    } else {
        for position in memmem::find_iter(window, needle) {
            if let Some(offset) = position.checked_sub(anchor_offset)
                && offset < starts
                && pattern.matches_at(window, offset)
                && visit(offset)?
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn checked_offset_add(base: usize, offset: usize) -> Result<usize, MemoryError> {
    base.checked_add(offset)
        .ok_or(MemoryError::AddressOverflow { base, offset })
}

fn longest_fixed_run(mask: &[u8]) -> (usize, usize) {
    let mut best = (0, 0);
    let mut start = 0;
    while start < mask.len() {
        while start < mask.len() && mask[start] == 0 {
            start += 1;
        }
        let mut end = start;
        while end < mask.len() && mask[end] != 0 {
            end += 1;
        }
        if end - start > best.1 {
            best = (start, end - start);
        }
        start = end;
    }
    best
}

fn region_address(region: MemoryRegion, offset: usize) -> Result<usize, MemoryError> {
    region
        .address
        .checked_add(offset)
        .ok_or(MemoryError::AddressOverflow {
            base: region.address,
            offset,
        })
}

impl fmt::Display for Pattern {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, (&byte, &mask)) in self.bytes.iter().zip(&self.mask).enumerate() {
            if index != 0 {
                formatter.write_str(" ")?;
            }
            if mask == 0 {
                formatter.write_str("??")?;
            } else {
                write!(formatter, "{byte:02X}")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
