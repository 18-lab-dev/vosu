//! The transport census: every frame each ring of this plugin host's own
//! endpoint carried between two instants, counted from the rings
//! themselves rather than inferred from the protocol.
//!
//! Both rings are mapped read-only, exactly as any library loaded into a
//! plugin host could (the B0 broker fixture does the same in C). The
//! constants mirror `crates/vosu-core/src/plugin/process/ring.rs` and
//! `frame.rs` (plugin-abi.md §7.2); [`Rings::map`] checks the capacity and
//! magic words, and [`Rings::since`] fails unless the bytes between two
//! write cursors split into whole frames, so a drift fails the bench.

use vosu_plugin_api::{VOSU_OPCODE_PRIVATE_BIT, vosu_opcode};

const CAPACITY: u64 = 2 * 1024 * 1024;
const HEADER_BYTES: usize = 256;
const REGION_BYTES: usize = HEADER_BYTES + CAPACITY as usize;
const CAPACITY_WORD: usize = 0;
const MAGIC_WORD: usize = 8;
const WRITE_CURSOR: usize = 64;
const MAGIC: u64 = u64::from_le_bytes(*b"vosuRNG1");
/// `total_len` (excluding itself) of a frame head with an empty payload.
const HEADER_LEN: u64 = 20;

/// One ring's read-only view.
struct Ring {
    base: *const u8,
}

impl Ring {
    fn word(&self, offset: usize) -> u64 {
        // SAFETY: `offset` is an 8-aligned header word inside the
        // `REGION_BYTES` view, mapped for this process's whole life; the
        // volatile read never caches a value the peer changes.
        let value = unsafe { self.base.add(offset).cast::<u64>().read_volatile() };
        std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
        value
    }

    fn byte(&self, cursor: u64) -> u8 {
        let index = (cursor & (CAPACITY - 1)) as usize;
        // SAFETY: `HEADER_BYTES + index` lies inside the data area of the
        // mapped view.
        unsafe { self.base.add(HEADER_BYTES + index).read_volatile() }
    }

    fn u32_at(&self, cursor: u64) -> u32 {
        u32::from_le_bytes([0, 1, 2, 3].map(|at| self.byte(cursor + at)))
    }
}

/// Both rings' write cursors at one instant.
#[derive(Clone, Copy, Debug)]
pub struct Cursors {
    plugin_to_host: u64,
    host_to_plugin: u64,
}

/// Frames counted over a window.
#[derive(Clone, Copy, Debug, Default)]
pub struct Census {
    pub plugin_to_host: u64,
    pub host_to_plugin: u64,
    /// Private (A5.2 control) frames, both directions.
    pub private: u64,
    pub submits: u64,
    pub results: u64,
}

impl Census {
    pub fn add(&mut self, other: Self) {
        self.plugin_to_host += other.plugin_to_host;
        self.host_to_plugin += other.host_to_plugin;
        self.private += other.private;
        self.submits += other.submits;
        self.results += other.results;
    }

    pub fn frames(&self) -> u64 {
        self.plugin_to_host + self.host_to_plugin
    }
}

pub struct Rings {
    plugin_to_host: Ring,
    host_to_plugin: Ring,
}

// SAFETY: the views are read-only and stay mapped for the process's life;
// every access is a volatile read.
unsafe impl Send for Rings {}
// SAFETY: as for `Send`.
unsafe impl Sync for Rings {}

impl Rings {
    /// Maps this plugin host's two rings from its endpoint argument.
    pub fn map() -> Result<Self, String> {
        let (host_to_plugin, plugin_to_host) = platform::map()?;
        let rings = Self {
            plugin_to_host: Ring {
                base: plugin_to_host,
            },
            host_to_plugin: Ring {
                base: host_to_plugin,
            },
        };
        for ring in [&rings.plugin_to_host, &rings.host_to_plugin] {
            if ring.word(CAPACITY_WORD) != CAPACITY || ring.word(MAGIC_WORD) != MAGIC {
                return Err("a mapped ring does not hold the expected layout".to_owned());
            }
        }
        Ok(rings)
    }

    pub fn cursors(&self) -> Cursors {
        Cursors {
            plugin_to_host: self.plugin_to_host.word(WRITE_CURSOR),
            host_to_plugin: self.host_to_plugin.word(WRITE_CURSOR),
        }
    }

    /// Every frame written since `before`.
    pub fn since(&self, before: Cursors) -> Result<Census, String> {
        self.between(before, self.cursors())
    }

    pub fn between(&self, before: Cursors, after: Cursors) -> Result<Census, String> {
        let mut census = Census::default();
        census.plugin_to_host = count(
            &self.plugin_to_host,
            before.plugin_to_host,
            after.plugin_to_host,
            &mut census,
        )?;
        census.host_to_plugin = count(
            &self.host_to_plugin,
            before.host_to_plugin,
            after.host_to_plugin,
            &mut census,
        )?;
        Ok(census)
    }
}

/// Splits `ring`'s bytes between the cursors `from` and `to` into frames.
fn count(ring: &Ring, from: u64, to: u64, census: &mut Census) -> Result<u64, String> {
    if to < from || to - from > CAPACITY {
        return Err(format!(
            "ring window {from}..{to} is not within one capacity"
        ));
    }
    let mut cursor = from;
    let mut frames = 0;
    while cursor < to {
        let total = u64::from(ring.u32_at(cursor));
        let opcode = ring.u32_at(cursor + 4);
        let payload = u64::from(ring.u32_at(cursor + 20));
        if total != HEADER_LEN + payload {
            return Err(format!("malformed frame head at cursor {cursor}"));
        }
        cursor += 4 + total;
        frames += 1;
        if opcode & VOSU_OPCODE_PRIVATE_BIT != 0 {
            census.private += 1;
        } else if opcode == vosu_opcode::VOSU_OP_READ_PLAN_SUBMIT as u32 {
            census.submits += 1;
        } else if opcode == vosu_opcode::VOSU_OP_READ_PLAN_RESULT as u32 {
            census.results += 1;
        }
    }
    if cursor != to {
        return Err(format!("ring window {from}..{to} ends inside a frame"));
    }
    Ok(frames)
}

#[cfg(target_os = "linux")]
mod platform {
    use super::REGION_BYTES;

    /// The plugin host's last argument names its four descriptors: host →
    /// plugin mapping and doorbell, then plugin → host.
    pub fn map() -> Result<(*const u8, *const u8), String> {
        let cmdline = std::fs::read("/proc/self/cmdline").map_err(|error| error.to_string())?;
        let argument = cmdline
            .split(|byte| *byte == 0)
            .rfind(|part| !part.is_empty())
            .ok_or("empty command line")?;
        let argument = std::str::from_utf8(argument).map_err(|error| error.to_string())?;
        let fds = argument
            .split(',')
            .map(str::parse::<i32>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| format!("endpoint argument {argument:?} is not four descriptors"))?;
        let [host_to_plugin, _, plugin_to_host, _] = fds[..] else {
            return Err(format!(
                "endpoint argument {argument:?} is not four descriptors"
            ));
        };
        Ok((view(host_to_plugin)?, view(plugin_to_host)?))
    }

    fn view(fd: i32) -> Result<*const u8, String> {
        // SAFETY: a fresh shared read-only mapping of the ring's memfd;
        // failure is `MAP_FAILED`, checked below. The mapping is never
        // unmapped.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                REGION_BYTES,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(format!(
                "mapping ring descriptor {fd}: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(base.cast_const().cast())
    }
}

#[cfg(windows)]
mod platform {
    use windows_sys::Win32::System::Memory::{FILE_MAP_READ, MapViewOfFile};

    use super::REGION_BYTES;

    /// The plugin host's endpoint argument,
    /// `h2p=<section>:<event>,p2h=<section>:<event>` in hex.
    pub fn map() -> Result<(*const u8, *const u8), String> {
        let argument = std::env::args()
            .rev()
            .find(|arg| arg.starts_with("h2p="))
            .ok_or("no endpoint argument")?;
        let section = |name: &str| -> Result<usize, String> {
            let value = argument
                .split(',')
                .find_map(|part| part.strip_prefix(name))
                .and_then(|handles| handles.split_once(':'))
                .ok_or_else(|| format!("endpoint argument {argument:?} lacks {name}"))?
                .0;
            usize::from_str_radix(value, 16).map_err(|error| error.to_string())
        };
        Ok((view(section("h2p=")?)?, view(section("p2h=")?)?))
    }

    fn view(section: usize) -> Result<*const u8, String> {
        // SAFETY: `section` is the plugin host's live section handle, open
        // for its whole life; a failed map returns null, checked below. The
        // view is never unmapped.
        let view = unsafe {
            MapViewOfFile(
                section as *mut core::ffi::c_void,
                FILE_MAP_READ,
                0,
                0,
                REGION_BYTES,
            )
        };
        if view.Value.is_null() {
            return Err(format!(
                "mapping section {section:#x}: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(view.Value.cast_const().cast())
    }
}
