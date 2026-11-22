//! Loaded-module enumeration over `/proc/<pid>/maps`: split out of
//! `linux.rs` so that module stays under the maintained-file line limit
//! (`docs/architecture/source-map.md`).

use super::*;

/// One loaded module, as reconstructed from `/proc/<pid>/maps`'s path
/// column (there is no Linux equivalent of Windows's loader-table
/// `Module32FirstW` for an unprivileged reader). `base` is the start of
/// the mapping whose file offset is zero — the file's own base, not
/// necessarily the lowest-addressed mapping — and `size` the span from
/// there to the highest mapping end grouped under the same full path
/// (F4: two different files that merely share a basename, or two
/// far-apart mappings of the same file, never collapse into one
/// `ModuleEntry`); matching `/proc/<pid>/maps` is a snapshot, so a caller
/// must call [`enumerate_modules`] again to see a module that loaded
/// since the last call rather than caching one snapshot across reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleEntry {
    pub base_address: usize,
    pub size: u32,
    pub name: String,
}

/// One file's mappings, accumulated while walking `/proc/<pid>/maps` in
/// order; `anchor_base` is `Some` once a mapping with file offset 0 has
/// been seen, and wins over `min_start` once set (F4).
struct ModuleSpan {
    anchor_base: Option<usize>,
    min_start: usize,
    max_end: usize,
    name: String,
}

/// Enumerates `pid`'s loaded modules from `/proc/<pid>/maps`, grouping
/// every mapping backed by the same full path into one [`ModuleEntry`]
/// (mirrors `windows::enumerate_modules`'s per-file semantics).
pub fn enumerate_modules(pid: u32) -> Result<Vec<ModuleEntry>, MemoryError> {
    enumerate_modules_at(Path::new("/proc"), pid)
}

/// Splits one `/proc/<pid>/maps` line into its address range, file
/// offset, and verbatim pathname (everything from the first
/// non-whitespace byte of the sixth field to the end of the line,
/// preserving any single spaces inside the path itself — F5), or `None`
/// for a line with no backing file (an anonymous mapping, or a pseudo-path
/// like `[heap]`).
fn parse_maps_line(line: &str) -> Option<(&str, &str, &str)> {
    let mut fields = line.split_whitespace();
    let addr_range = fields.next()?;
    let _perms = fields.next()?;
    let offset_field = fields.next()?;
    let _dev = fields.next()?;
    let _inode = fields.next()?;
    let first_path_token = fields.next()?;
    // `first_path_token` borrows from `line`, so this recovers its own
    // byte offset within `line` — the start of the path field, after
    // whatever whitespace padding separates it from `inode` — without
    // re-splitting (and thereby collapsing) the path's own internal
    // spaces.
    let path_start = first_path_token.as_ptr() as usize - line.as_ptr() as usize;
    let pathname = &line[path_start..];
    if pathname.starts_with('[') || pathname.is_empty() {
        return None;
    }
    Some((addr_range, offset_field, pathname))
}

fn enumerate_modules_at(proc_root: &Path, pid: u32) -> Result<Vec<ModuleEntry>, MemoryError> {
    let bytes = read_bounded(&proc_root.join(pid.to_string()).join("maps"), 0)
        .map_err(|error| backend("read /proc maps for module enumeration", error))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| backend_message("parse /proc maps", "maps is not UTF-8"))?;

    let mut order: Vec<String> = Vec::new();
    let mut spans: std::collections::HashMap<String, ModuleSpan> = std::collections::HashMap::new();

    for line in text.lines() {
        let Some((addr_range, offset_field, pathname)) = parse_maps_line(line) else {
            continue;
        };
        // A replaced binary's mapping keeps its old dentry, so the kernel
        // appends this marker to the path rather than the path itself
        // changing; stripped here so the module still resolves by its
        // real name (F5) instead of being permanently `NotReady`.
        let path = pathname.strip_suffix(" (deleted)").unwrap_or(pathname);
        let Some(name) = Path::new(path).file_name().and_then(OsStr::to_str) else {
            continue;
        };

        let (start, end) = addr_range
            .split_once('-')
            .and_then(|(start, end)| {
                Some((
                    usize::from_str_radix(start, 16).ok()?,
                    usize::from_str_radix(end, 16).ok()?,
                ))
            })
            .ok_or_else(|| backend_message("parse /proc maps", "invalid address range"))?;
        let file_offset = u64::from_str_radix(offset_field, 16)
            .map_err(|_| backend_message("parse /proc maps", "invalid file offset"))?;

        match spans.get_mut(path) {
            Some(span) => {
                span.min_start = span.min_start.min(start);
                span.max_end = span.max_end.max(end);
                if file_offset == 0 {
                    span.anchor_base = Some(span.anchor_base.map_or(start, |base| base.min(start)));
                }
            }
            None => {
                order.push(path.to_owned());
                spans.insert(
                    path.to_owned(),
                    ModuleSpan {
                        anchor_base: (file_offset == 0).then_some(start),
                        min_start: start,
                        max_end: end,
                        name: name.to_owned(),
                    },
                );
            }
        }
    }

    order
        .into_iter()
        .map(|path| {
            let span = &spans[&path];
            // The file-offset-0 mapping's own start, when one was seen —
            // the file's real base, not necessarily this group's lowest
            // address (F4) — falling back to the lowest start seen for a
            // file with no offset-0 mapping in this snapshot at all.
            let base = span.anchor_base.unwrap_or(span.min_start);
            let size = u32::try_from(span.max_end - base)
                .map_err(|_| backend_message("parse /proc maps", "module span exceeds u32::MAX"))?;
            Ok(ModuleEntry {
                base_address: base,
                size,
                name: span.name.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
