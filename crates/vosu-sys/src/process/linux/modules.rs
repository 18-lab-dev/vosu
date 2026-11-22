//! Loaded-module enumeration over `/proc/<pid>/maps`: split out of
//! `linux.rs` so that module stays under the maintained-file line limit
//! (`docs/architecture/source-map.md`).

use super::*;

/// One loaded module, as reconstructed from `/proc/<pid>/maps`'s path
/// column (there is no Linux equivalent of Windows's loader-table
/// `Module32FirstW` for an unprivileged reader). `base` is the lowest
/// mapping start seen for that file, `size` the span from there to the
/// highest mapping end; matching `/proc/<pid>/maps` is a snapshot, so a
/// caller must call [`enumerate_modules`] again to see a module that
/// loaded since the last call rather than caching one snapshot across
/// reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleEntry {
    pub base_address: usize,
    pub size: u32,
    pub name: String,
}

/// Enumerates `pid`'s loaded modules from `/proc/<pid>/maps`, grouping
/// every mapping backed by the same file name into one [`ModuleEntry`]
/// (mirrors `windows::enumerate_modules`'s per-file semantics).
pub fn enumerate_modules(pid: u32) -> Result<Vec<ModuleEntry>, MemoryError> {
    enumerate_modules_at(Path::new("/proc"), pid)
}

fn enumerate_modules_at(proc_root: &Path, pid: u32) -> Result<Vec<ModuleEntry>, MemoryError> {
    let bytes = read_bounded(&proc_root.join(pid.to_string()).join("maps"), 0)
        .map_err(|error| backend("read /proc maps for module enumeration", error))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| backend_message("parse /proc maps", "maps is not UTF-8"))?;

    let mut order: Vec<String> = Vec::new();
    let mut spans: std::collections::HashMap<String, (usize, usize)> =
        std::collections::HashMap::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 6 {
            continue; // anonymous mapping: no backing file
        }
        let pathname = fields[5..].join(" ");
        if pathname.starts_with('[') || pathname.is_empty() {
            continue;
        }
        let Some(name) = Path::new(&pathname).file_name().and_then(OsStr::to_str) else {
            continue;
        };
        let (start, end) = fields[0]
            .split_once('-')
            .and_then(|(start, end)| {
                Some((
                    usize::from_str_radix(start, 16).ok()?,
                    usize::from_str_radix(end, 16).ok()?,
                ))
            })
            .ok_or_else(|| backend_message("parse /proc maps", "invalid address range"))?;
        match spans.get_mut(name) {
            Some((base, top)) => {
                *base = (*base).min(start);
                *top = (*top).max(end);
            }
            None => {
                order.push(name.to_owned());
                spans.insert(name.to_owned(), (start, end));
            }
        }
    }

    Ok(order
        .into_iter()
        .map(|name| {
            let (base, top) = spans[&name];
            ModuleEntry {
                base_address: base,
                size: (top - base) as u32,
                name,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests;
