//! Signature-scan region walk: applies a parsed
//! [`Pattern`](vosu_plugin_api::memory::Pattern) to every readable region a
//! [`ProcessMemory`] backend reports, in bounded windows. Pattern parsing and
//! per-candidate matching are platform-neutral SDK vocabulary; this walk
//! stays at the native boundary with region enumeration.

use memchr::{memchr_iter, memmem};
use vosu_plugin_api::memory::{BatchScanResult, MemoryError, MemoryRegion, Pattern, ProcessMemory};

/// Number of new region bytes read per scanner window.
pub const SCAN_CHUNK_SIZE: usize = 1024 * 1024;

/// Returns the first match in region order, then ascending offset order.
pub fn scan_first(
    memory: &dyn ProcessMemory,
    pattern: &Pattern,
) -> Result<Option<usize>, MemoryError> {
    let regions = memory.readable_regions()?;
    let mut scratch = ScanScratch::new(pattern.bytes().len(), max_region_size(&regions))?;

    for region in &regions {
        let mut first = None;
        if scan_region(
            memory,
            *region,
            pattern.bytes().len(),
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
            if pattern.bytes().is_empty() {
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
    memory: &dyn ProcessMemory,
    pattern: &Pattern,
    mut validate: impl FnMut(usize) -> Result<bool, MemoryError>,
) -> Result<Option<usize>, MemoryError> {
    if pattern.bytes().is_empty() {
        return Ok(None);
    }
    let regions = memory.readable_regions()?;
    let mut scratch = ScanScratch::new(pattern.bytes().len(), max_region_size(&regions))?;
    for region in regions {
        let mut accepted = None;
        let outcome = scan_region_until(
            memory,
            region,
            pattern.bytes().len(),
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
pub fn scan_all(memory: &dyn ProcessMemory, pattern: &Pattern) -> Result<Vec<usize>, MemoryError> {
    if pattern.bytes().is_empty() {
        return Ok(Vec::new());
    }

    let regions = memory.readable_regions()?;
    let mut scratch = ScanScratch::new(pattern.bytes().len(), max_region_size(&regions))?;
    let mut matches = Vec::new();

    for region in regions {
        let checkpoint = matches.len();
        let readable = scan_region(
            memory,
            region,
            pattern.bytes().len(),
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
pub fn scan_batch(
    memory: &dyn ProcessMemory,
    patterns: &[Pattern],
) -> Result<Vec<BatchScanResult>, MemoryError> {
    if patterns.is_empty() {
        return Ok(Vec::new());
    }

    let regions = memory.readable_regions()?;
    let max_len = patterns
        .iter()
        .map(|pattern| pattern.bytes().len())
        .max()
        .unwrap_or(0);
    let mut scratch = ScanScratch::new(max_len, max_region_size(&regions))?;
    let mut found = vec![false; patterns.len()];
    let mut results = Vec::with_capacity(patterns.len());
    let mut region_matches = if regions.is_empty() {
        Vec::new()
    } else {
        vec![None; patterns.len()]
    };

    for region in regions {
        region_matches.fill(None);
        let readable = scan_region(memory, region, max_len, &mut scratch, |window, base, _| {
            for (index, pattern) in patterns.iter().enumerate() {
                if found[index] || region_matches[index].is_some() || pattern.bytes().is_empty() {
                    continue;
                }
                region_matches[index] = find_first_candidate(
                    pattern,
                    window,
                    valid_starts(window.len(), pattern.bytes().len(), base),
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
            let offset = if pattern.bytes().is_empty() {
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

fn max_region_size(regions: &[MemoryRegion]) -> usize {
    regions.iter().map(|region| region.size).max().unwrap_or(0)
}

struct ScanScratch {
    bytes: Box<[u8]>,
}

impl ScanScratch {
    fn new(max_pattern_len: usize, max_region_size: usize) -> Result<Self, MemoryError> {
        let overlap = max_pattern_len.saturating_sub(1);
        let capacity = SCAN_CHUNK_SIZE
            .checked_add(overlap)
            .ok_or(MemoryError::AddressOverflow {
                base: SCAN_CHUNK_SIZE,
                offset: overlap,
            })?
            .min(max_region_size);
        Ok(Self {
            bytes: vec![0; capacity].into_boxed_slice(),
        })
    }
}

/// Calls `visit` for each window in a region. A failed window discards the
/// whole region, matching the upstream behavior for an unreadable region.
fn scan_region(
    memory: &dyn ProcessMemory,
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
        match memory.read_scanner_region_exact(address, window) {
            Ok(()) => {}
            Err(error @ MemoryError::PermissionDenied { .. }) => return Err(error),
            Err(_) => return Ok(false),
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
    memory: &dyn ProcessMemory,
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
        match memory.read_scanner_region_exact(address, window) {
            Ok(()) => {}
            Err(error @ MemoryError::PermissionDenied { .. }) => return Err(error),
            Err(_) => return Ok(ScanRegionOutcome::Unreadable),
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
    let (anchor_offset, anchor_len) = pattern.fixed_anchor();
    if anchor_len == 0 {
        return (0..starts).find(|&offset| pattern.matches_at(window, offset));
    }

    let needle = &pattern.bytes()[anchor_offset..anchor_offset + anchor_len];
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
    let (anchor_offset, anchor_len) = pattern.fixed_anchor();
    if anchor_len == 0 {
        for offset in 0..starts {
            if pattern.matches_at(window, offset) {
                visit(offset)?;
            }
        }
        return Ok(());
    }

    let needle = &pattern.bytes()[anchor_offset..anchor_offset + anchor_len];
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
    let (anchor_offset, anchor_len) = pattern.fixed_anchor();
    if anchor_len == 0 {
        for offset in 0..starts {
            if pattern.matches_at(window, offset) && visit(offset)? {
                return Ok(true);
            }
        }
        return Ok(false);
    }

    let needle = &pattern.bytes()[anchor_offset..anchor_offset + anchor_len];
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

fn region_address(region: MemoryRegion, offset: usize) -> Result<usize, MemoryError> {
    region
        .address
        .checked_add(offset)
        .ok_or(MemoryError::AddressOverflow {
            base: region.address,
            offset,
        })
}

#[cfg(test)]
mod tests;
