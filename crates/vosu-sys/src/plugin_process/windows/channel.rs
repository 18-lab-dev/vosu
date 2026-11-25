//! The plugin host's inherited endpoints on Windows (`docs/v2/plugin-abi.md`
//! §7.2, §7.6): the one command-line argument naming the child's four
//! inherited handle values, and their validated adoption.
//!
//! The argument is `h2p=<section>:<event>,p2h=<section>:<event>`, each value
//! lowercase hexadecimal without a prefix or leading zeros: the host → plugin
//! direction first, then plugin → host. It names handles only; no object
//! has a name, path or port anyone else could open.
//!
//! [`adopt_inherited`] refuses, before it maps anything, a malformed
//! argument, a duplicate or tag-bit value, a value that is not an open
//! handle of the expected object type, and a section whose size is not
//! the trusted region length rounded up to whole pages.

use std::fmt::Write as _;
use std::io;
use std::os::windows::io::{FromRawHandle, OwnedHandle, RawHandle};

use windows_sys::Wdk::Foundation::{NtQueryObject, ObjectTypeInformation};
use windows_sys::Win32::Foundation::{HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation};
use windows_sys::Win32::System::Memory::{
    FILE_MAP_READ, MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    UnmapViewOfFile, VirtualQuery,
};
use windows_sys::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};
use windows_sys::Win32::System::WindowsProgramming::PUBLIC_OBJECT_TYPE_INFORMATION;

use crate::plugin_process::RingObjects;

/// The child-side values of the four inherited handles, as
/// `[section, event]` per direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointValues {
    pub host_to_plugin: [usize; 2],
    pub plugin_to_host: [usize; 2],
}

/// Why an endpoint argument was refused. The child exits with the §7.6
/// `endpoints` stage before mapping anything.
#[derive(Debug, thiserror::Error)]
pub enum EndpointError {
    #[error("malformed endpoint argument")]
    Malformed,
    #[error("endpoint handle values must be distinct, nonzero and untagged")]
    InvalidValue,
    #[error("endpoint handle {value:#x} is not an open {expected} handle")]
    WrongKind {
        value: usize,
        expected: &'static str,
    },
    #[error("endpoint section {value:#x} is {actual} bytes, expected {expected}")]
    WrongSize {
        value: usize,
        actual: usize,
        expected: usize,
    },
    #[error("adopting endpoint objects failed: {0}")]
    Os(#[from] io::Error),
}

impl EndpointValues {
    /// The command-line argument for these values.
    pub fn to_arg(&self) -> String {
        let mut arg = String::with_capacity(80);
        let [h2p_section, h2p_event] = self.host_to_plugin;
        let [p2h_section, p2h_event] = self.plugin_to_host;
        // Writing into a String cannot fail.
        let _ = write!(
            arg,
            "h2p={h2p_section:x}:{h2p_event:x},p2h={p2h_section:x}:{p2h_event:x}"
        );
        arg
    }

    /// Parses exactly the [`Self::to_arg`] form, then checks the values are
    /// four distinct, nonzero handle values with clear tag bits (the
    /// kernel ignores a handle's low two bits, so two values differing only
    /// there would alias one handle).
    pub fn parse(arg: &str) -> Result<Self, EndpointError> {
        let (h2p, p2h) = arg.split_once(',').ok_or(EndpointError::Malformed)?;
        let host_to_plugin = pair(h2p.strip_prefix("h2p=").ok_or(EndpointError::Malformed)?)?;
        let plugin_to_host = pair(p2h.strip_prefix("p2h=").ok_or(EndpointError::Malformed)?)?;
        let values = Self {
            host_to_plugin,
            plugin_to_host,
        };
        let all = values.all();
        for (index, &value) in all.iter().enumerate() {
            if value == 0 || value & 3 != 0 || all[..index].contains(&value) {
                return Err(EndpointError::InvalidValue);
            }
        }
        Ok(values)
    }

    fn all(&self) -> [usize; 4] {
        let [a, b] = self.host_to_plugin;
        let [c, d] = self.plugin_to_host;
        [a, b, c, d]
    }
}

fn pair(text: &str) -> Result<[usize; 2], EndpointError> {
    let (section, event) = text.split_once(':').ok_or(EndpointError::Malformed)?;
    Ok([hex(section)?, hex(event)?])
}

fn hex(text: &str) -> Result<usize, EndpointError> {
    let canonical = !text.is_empty()
        && text.len() <= 2 * size_of::<usize>()
        && !(text.len() > 1 && text.starts_with('0'))
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
    if !canonical {
        return Err(EndpointError::Malformed);
    }
    usize::from_str_radix(text, 16).map_err(|_| EndpointError::Malformed)
}

/// Both inherited ring directions, adopted and mapped.
#[derive(Debug)]
pub struct InheritedEndpoints {
    pub host_to_plugin: RingObjects,
    pub plugin_to_host: RingObjects,
}

/// Validates the endpoint argument and the four handles it names, then
/// adopts them as ring objects of exactly `region_len` mapped bytes.
///
/// Adopted handles stop being inheritable, so a process the plugin itself
/// starts with handle inheritance never receives core's channel.
///
/// Call once, from the plugin host's entry, before anything else in the
/// process could open handles of its own.
pub fn adopt_inherited(arg: &str, region_len: usize) -> Result<InheritedEndpoints, EndpointError> {
    let values = EndpointValues::parse(arg)?;
    for direction in [values.host_to_plugin, values.plugin_to_host] {
        let [section, event] = direction;
        check_kind(section, "Section")?;
        check_kind(event, "Event")?;
        check_section_size(section, region_len)?;
    }
    for value in values.all() {
        // SAFETY: `value` was checked above to be an open handle of this
        // process; this only clears its inheritance flag.
        if unsafe { SetHandleInformation(value as HANDLE, HANDLE_FLAG_INHERIT, 0) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
    }
    let adopt = |[section, event]: [usize; 2]| {
        // SAFETY: each value names an open handle of the checked type that
        // this process inherited for exactly this purpose (§7.4); the four
        // values are distinct, so each handle gets exactly one owner.
        let (section, event) = unsafe {
            (
                OwnedHandle::from_raw_handle(section as RawHandle),
                OwnedHandle::from_raw_handle(event as RawHandle),
            )
        };
        RingObjects::from_inherited(section, event, region_len)
    };
    Ok(InheritedEndpoints {
        host_to_plugin: adopt(values.host_to_plugin)?,
        plugin_to_host: adopt(values.plugin_to_host)?,
    })
}

/// Checks `value` names an open handle whose object type is `expected`.
fn check_kind(value: usize, expected: &'static str) -> Result<(), EndpointError> {
    let wrong = EndpointError::WrongKind { value, expected };
    // Room for the type-information header plus a short type name.
    let mut buffer = [0_u64; 64];
    let mut returned = 0;
    // SAFETY: queries the type of whatever `value` names; an invalid value
    // fails with an NTSTATUS. `buffer` is writable, 8-byte aligned and its
    // byte length is passed.
    let status = unsafe {
        NtQueryObject(
            value as HANDLE,
            ObjectTypeInformation,
            buffer.as_mut_ptr().cast(),
            size_of_val(&buffer) as u32,
            &mut returned,
        )
    };
    if status < 0 {
        return Err(wrong);
    }
    // SAFETY: on success the buffer starts with this structure.
    let information = unsafe { &*buffer.as_ptr().cast::<PUBLIC_OBJECT_TYPE_INFORMATION>() };
    let name = information.TypeName;
    let units = usize::from(name.Length) / 2;
    let start = buffer.as_ptr() as usize;
    let end = start + size_of_val(&buffer);
    let name_start = name.Buffer as usize;
    // The name must lie inside the buffer the system filled.
    if name.Buffer.is_null()
        || name_start < start
        || name_start
            .checked_add(units * 2)
            .is_none_or(|name_end| name_end > end)
    {
        return Err(wrong);
    }
    // SAFETY: `units` UTF-16 units at `name.Buffer`, checked above to lie
    // inside `buffer`.
    let name = unsafe { std::slice::from_raw_parts(name.Buffer, units) };
    if !name.iter().copied().eq(expected.encode_utf16()) {
        return Err(wrong);
    }
    Ok(())
}

/// Checks the section is exactly `region_len` bytes rounded up to whole
/// pages: a whole-section view spans that size.
fn check_section_size(value: usize, region_len: usize) -> Result<(), EndpointError> {
    // SAFETY: `value` was checked to be a section handle; a zero length maps
    // the whole section read-only.
    let view = unsafe { MapViewOfFile(value as HANDLE, FILE_MAP_READ, 0, 0, 0) };
    if view.Value.is_null() {
        return Err(io::Error::last_os_error().into());
    }
    let unmap = Unmap(view);
    // SAFETY: all-zero is a valid value of this plain C struct.
    let mut basic: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: queries this process's own mapped view into a writable struct.
    let written = unsafe {
        VirtualQuery(
            unmap.0.Value,
            &mut basic,
            size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };
    drop(unmap);
    if written == 0 {
        return Err(io::Error::last_os_error().into());
    }
    let expected = region_len.next_multiple_of(page_size());
    if basic.RegionSize != expected {
        return Err(EndpointError::WrongSize {
            value,
            actual: basic.RegionSize,
            expected,
        });
    }
    Ok(())
}

struct Unmap(MEMORY_MAPPED_VIEW_ADDRESS);

impl Drop for Unmap {
    fn drop(&mut self) {
        // SAFETY: the view `MapViewOfFile` returned, unmapped exactly once;
        // nothing references it.
        unsafe { UnmapViewOfFile(self.0) };
    }
}

fn page_size() -> usize {
    // SAFETY: all-zero is a valid value of this plain C struct, which the
    // call fills.
    let mut info: SYSTEM_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is writable; the call cannot fail.
    unsafe { GetSystemInfo(&mut info) };
    info.dwPageSize as usize
}
