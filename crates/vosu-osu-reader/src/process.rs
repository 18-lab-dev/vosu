//! Pointer-width managed-heap read layer over `vosu_sys::process`'s
//! object-safe syscall boundary, plus AOB pattern scanning built on it.
//!
//! This is the generic client-decoding layer shared by the stable and lazer
//! readers: it knows the .NET/Mono managed-heap layouts (strings, arrays,
//! lists, dictionaries, `DateTime`, `decimal`) and the target's pointer
//! width, but nothing about osu!'s own memory layout.

use std::fmt;
use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
pub use vosu_sys::process::scanner::{
    BatchScanResult, Pattern, PatternParseError, SCAN_CHUNK_SIZE,
};
use vosu_sys::process::{
    MemoryError, MemoryRead, MemoryRegion, ProcessBitness, ProcessMemory, ReadRequest, ReadStatus,
};

const MAX_MANAGED_STRING_LENGTH: usize = 4095;
const MAX_MANAGED_COLLECTION_LENGTH: usize = 1_000_000;
const DOTNET_UNIX_EPOCH_TICKS: u64 = 621_355_968_000_000_000;
const DOTNET_TICKS_PER_SECOND: u64 = 10_000_000;

/// Injectable AOB-scan backend: when set on a [`ProcessHandle`], every
/// `scan_*` method routes through it instead of `vosu-sys`'s generic region
/// walk. `scan_all`/`scan_batch` must return results in the region walk's
/// own order (ascending region, then ascending offset within a region; at
/// most one hit per pattern for `scan_batch`) so callers observe identical
/// behavior regardless of backend (`prop_scan_backend_matches_region_walk`).
pub trait ScanBackend: Send + Sync {
    /// Every match, in ascending region order then ascending offset.
    fn scan_all(&self, pattern: &Pattern) -> Result<Vec<usize>, MemoryError>;
    /// At most one hit per pattern, in the region walk's own order.
    fn scan_batch(&self, patterns: &[Pattern]) -> Result<Vec<BatchScanResult>, MemoryError>;
}

/// RAII owner for a process-memory backend and the target's pointer width.
#[derive(Clone)]
pub struct ProcessHandle {
    memory: Arc<dyn ProcessMemory>,
    bitness: ProcessBitness,
    scans: Option<Arc<dyn ScanBackend>>,
}

impl fmt::Debug for ProcessHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessHandle")
            .field("process_id", &self.process_id())
            .field("bitness", &self.bitness)
            .finish_non_exhaustive()
    }
}

impl ProcessHandle {
    #[must_use]
    pub fn new(memory: Arc<dyn ProcessMemory>, bitness: ProcessBitness) -> Self {
        Self {
            memory,
            bitness,
            scans: None,
        }
    }

    /// Like [`Self::new`], but every `scan_*` method routes through `scans`
    /// instead of walking `memory`'s readable regions directly.
    #[must_use]
    pub fn with_scan_backend(
        memory: Arc<dyn ProcessMemory>,
        bitness: ProcessBitness,
        scans: Arc<dyn ScanBackend>,
    ) -> Self {
        Self {
            memory,
            bitness,
            scans: Some(scans),
        }
    }

    #[must_use]
    pub const fn bitness(&self) -> ProcessBitness {
        self.bitness
    }

    #[must_use]
    pub fn process_id(&self) -> Option<u32> {
        self.memory.process_id()
    }

    #[must_use]
    pub fn is_alive(&self) -> bool {
        self.memory.is_alive()
    }

    #[must_use]
    pub const fn pointer_size(&self) -> usize {
        self.bitness.pointer_size()
    }

    pub fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.memory.read_exact(address, dst)
    }

    pub fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        self.memory.read_many(reads)
    }

    pub fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        self.memory.readable_regions()
    }

    pub fn read_bytes_into(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.read_exact(address, dst)
    }

    pub fn read_bytes(&self, address: usize, len: usize) -> Result<Vec<u8>, MemoryError> {
        let mut bytes = vec![0; len];
        self.read_exact(address, &mut bytes)?;
        Ok(bytes)
    }

    pub fn read_u8(&self, address: usize) -> Result<u8, MemoryError> {
        let mut bytes = [0; 1];
        self.read_exact(address, &mut bytes)?;
        Ok(bytes[0])
    }

    pub fn read_i8(&self, address: usize) -> Result<i8, MemoryError> {
        Ok(self.read_u8(address)? as i8)
    }

    pub fn read_u16(&self, address: usize) -> Result<u16, MemoryError> {
        Ok(u16::from_le_bytes(self.read_array(address)?))
    }

    pub fn read_i16(&self, address: usize) -> Result<i16, MemoryError> {
        Ok(i16::from_le_bytes(self.read_array(address)?))
    }

    pub fn read_u32(&self, address: usize) -> Result<u32, MemoryError> {
        Ok(u32::from_le_bytes(self.read_array(address)?))
    }

    pub fn read_i32(&self, address: usize) -> Result<i32, MemoryError> {
        Ok(i32::from_le_bytes(self.read_array(address)?))
    }

    pub fn read_u64(&self, address: usize) -> Result<u64, MemoryError> {
        Ok(u64::from_le_bytes(self.read_array(address)?))
    }

    pub fn read_i64(&self, address: usize) -> Result<i64, MemoryError> {
        Ok(i64::from_le_bytes(self.read_array(address)?))
    }

    pub fn read_f32(&self, address: usize) -> Result<f32, MemoryError> {
        Ok(f32::from_le_bytes(self.read_array(address)?))
    }

    pub fn read_f64(&self, address: usize) -> Result<f64, MemoryError> {
        Ok(f64::from_le_bytes(self.read_array(address)?))
    }

    pub fn read_bool(&self, address: usize) -> Result<bool, MemoryError> {
        Ok(self.read_u8(address)? != 0)
    }

    /// Reads a pointer without converting through a floating-point value.
    pub fn read_pointer(&self, address: usize) -> Result<usize, MemoryError> {
        match self.bitness {
            ProcessBitness::X86 => Ok(self.read_u32(address)? as usize),
            ProcessBitness::X64 => {
                usize::try_from(self.read_u64(address)?).map_err(|_| MemoryError::Corrupt {
                    context: "pointer",
                    address,
                    message: "target pointer does not fit the host address width",
                })
            }
        }
    }

    pub fn read_pointer_pointer(&self, address: usize) -> Result<usize, MemoryError> {
        self.read_pointer(self.read_pointer(address)?)
    }

    /// Reads a CLR string object. Null is not-ready; an allocated empty
    /// string is a ready empty value.
    pub fn read_managed_string(&self, address: usize) -> MemoryRead<String> {
        if address == 0 {
            return Ok(ReadStatus::NotReady);
        }

        let length_address = checked_address_add(address, self.pointer_size())?;
        let length = self.read_i32(length_address)?;
        if length < 0 {
            return Err(corrupt("managed string", address, "length is negative"));
        }
        let length = length as usize;
        if length > MAX_MANAGED_STRING_LENGTH {
            return Err(corrupt(
                "managed string",
                address,
                "length is at least 4096 UTF-16 code units",
            ));
        }
        if length == 0 {
            return Ok(ReadStatus::Ready(String::new()));
        }

        let byte_len = length.checked_mul(2).ok_or(MemoryError::AddressOverflow {
            base: length,
            offset: 2,
        })?;
        let chars_address = checked_address_add(length_address, 4)?;
        let bytes = self.read_bytes(chars_address, byte_len)?;
        let units = bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]));
        std::char::decode_utf16(units)
            .collect::<Result<String, _>>()
            .map(ReadStatus::Ready)
            .map_err(|_| MemoryError::InvalidUtf16 { address })
    }

    pub fn read_managed_string_pointer(&self, address: usize) -> MemoryRead<String> {
        self.read_managed_string(self.read_pointer(address)?)
    }

    /// Reads the legacy CLR string layout used by stable osu! (`x86`).
    ///
    /// Stable's historical `readSharpString` accepts the object address
    /// directly; its length and UTF-16 payload start at `+4` and `+8`.
    pub fn read_stable_string(&self, address: usize) -> MemoryRead<String> {
        if address == 0 {
            return Ok(ReadStatus::NotReady);
        }
        let length_address = checked_address_add(address, 4)?;
        let length = self.read_i32(length_address)?;
        if !(0..4096).contains(&length) {
            return Err(corrupt("stable string", address, "invalid UTF-16 length"));
        }
        if length == 0 {
            return Ok(ReadStatus::Ready(String::new()));
        }
        let bytes = self.read_bytes(
            checked_address_add(address, 8)?,
            (length as usize)
                .checked_mul(2)
                .ok_or(MemoryError::AddressOverflow {
                    base: length as usize,
                    offset: 2,
                })?,
        )?;
        std::char::decode_utf16(
            bytes
                .chunks_exact(2)
                .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]])),
        )
        .collect::<Result<String, _>>()
        .map(ReadStatus::Ready)
        .map_err(|_| MemoryError::InvalidUtf16 { address })
    }

    /// Returns the occupied-slot addresses of stable's legacy
    /// `SharpDictionary`. Unlike the generic CLR dictionary helper this layout
    /// stores the entries pointer at `+8`, count at `+0x1c`, and 16-byte slots.
    pub fn read_stable_dictionary_slots(&self, address: usize) -> Result<Vec<usize>, MemoryError> {
        if address == 0 {
            return Err(corrupt("stable dictionary", address, "null object pointer"));
        }
        let items = self.read_u32(checked_address_add(address, 8)?)? as usize;
        let size = self.read_i32(checked_address_add(address, 0x1c)?)?;
        let size = checked_collection_length(size, "stable dictionary", address)?;
        let mut result = Vec::with_capacity(size);
        for index in 0..size {
            let slot_offset = index
                .checked_mul(0x10)
                .ok_or(MemoryError::AddressOverflow {
                    base: index,
                    offset: 0x10,
                })?;
            result.push(checked_address_add(
                checked_address_add(items, 8)?,
                slot_offset,
            )?);
        }
        Ok(result)
    }

    pub fn read_managed_array<T, F>(
        &self,
        address: usize,
        element_size: usize,
        mut read_element: F,
    ) -> Result<Vec<T>, MemoryError>
    where
        F: FnMut(&Self, usize) -> Result<T, MemoryError>,
    {
        let header = self.read_array_header(address)?;
        let mut values = Vec::with_capacity(header.len);
        for index in 0..header.len {
            let offset = index
                .checked_mul(element_size)
                .ok_or(MemoryError::AddressOverflow {
                    base: index,
                    offset: element_size,
                })?;
            values.push(read_element(
                self,
                checked_address_add(header.data, offset)?,
            )?);
        }
        Ok(values)
    }

    pub fn read_managed_list<T, F>(
        &self,
        address: usize,
        element_size: usize,
        mut read_element: F,
    ) -> Result<Vec<T>, MemoryError>
    where
        F: FnMut(&Self, usize) -> Result<T, MemoryError>,
    {
        let header = self.read_list_header(address)?;
        let array = self.read_array_header(header.items)?;
        if header.len > array.len {
            return Err(corrupt(
                "managed list",
                address,
                "logical size exceeds backing array length",
            ));
        }

        let mut values = Vec::with_capacity(header.len);
        for index in 0..header.len {
            let offset = index
                .checked_mul(element_size)
                .ok_or(MemoryError::AddressOverflow {
                    base: index,
                    offset: element_size,
                })?;
            values.push(read_element(
                self,
                checked_address_add(array.data, offset)?,
            )?);
        }
        Ok(values)
    }

    /// Visits occupied entries in the dictionary representation used by lazer
    /// readers. The callback stays generic and monomorphized; only raw reads
    /// cross the object-safe backend boundary.
    pub fn read_managed_dictionary<T, F>(
        &self,
        address: usize,
        mut read_entry: F,
    ) -> Result<Vec<T>, MemoryError>
    where
        F: FnMut(&Self, usize, usize) -> Result<T, MemoryError>,
    {
        let header = self.read_dictionary_header(address)?;
        let hash_offset = self.pointer_size();
        let key_offset = hash_offset + 8;
        let mut result = Vec::with_capacity(header.len);
        for index in 0..header.len {
            let offset =
                index
                    .checked_mul(header.entry_size)
                    .ok_or(MemoryError::AddressOverflow {
                        base: index,
                        offset: header.entry_size,
                    })?;
            let entry = checked_address_add(header.data, offset)?;
            if self.read_i32(checked_address_add(entry, hash_offset)?)? == 0 {
                continue;
            }
            result.push(read_entry(
                self,
                checked_address_add(entry, key_offset)?,
                entry,
            )?);
        }
        Ok(result)
    }

    pub fn read_managed_dictionary_i32_to_ref(
        &self,
        address: usize,
    ) -> Result<Vec<ManagedDictionaryEntry>, MemoryError> {
        self.read_managed_dictionary(address, |handle, key, value| {
            Ok(ManagedDictionaryEntry {
                key: handle.read_i32(key)?,
                value: handle.read_pointer(value)?,
            })
        })
    }

    pub fn read_nullable<T, F>(
        &self,
        address: usize,
        value_offset: usize,
        read_value: F,
    ) -> Result<Option<T>, MemoryError>
    where
        F: FnOnce(&Self, usize) -> Result<T, MemoryError>,
    {
        if !self.read_bool(address)? {
            return Ok(None);
        }
        read_value(self, checked_address_add(address, value_offset)?).map(Some)
    }

    pub fn read_nullable_i32(&self, address: usize) -> Result<Option<i32>, MemoryError> {
        self.read_nullable(address, 4, Self::read_i32)
    }

    /// Decodes a `.NET DateTime` value to UTC. The top two kind bits are
    /// metadata and are excluded from the tick count.
    pub fn read_dotnet_datetime(&self, address: usize) -> Result<DateTime<Utc>, MemoryError> {
        let raw = self.read_u64(address)?;
        let ticks = raw & 0x3fff_ffff_ffff_ffff;
        const MAX_DOTNET_TICKS: u64 = 3_155_378_975_999_999_999;
        if ticks > MAX_DOTNET_TICKS {
            return Err(corrupt(
                ".NET DateTime",
                address,
                "tick count exceeds DateTime::MaxValue",
            ));
        }
        let unix_ticks = i128::from(ticks) - i128::from(DOTNET_UNIX_EPOCH_TICKS);
        let ticks_per_second = i128::from(DOTNET_TICKS_PER_SECOND);
        let seconds = unix_ticks.div_euclid(ticks_per_second) as i64;
        let nanos = (unix_ticks.rem_euclid(ticks_per_second) * 100) as u32;
        Utc.timestamp_opt(seconds, nanos).single().ok_or_else(|| {
            corrupt(
                ".NET DateTime",
                address,
                "value is outside the supported timestamp range",
            )
        })
    }

    /// Decodes the 16-byte CLR decimal layout (`flags`, `hi`, `lo`, `mid`) to
    /// the JavaScript-compatible `f64` used by the source reader.
    pub fn read_dotnet_decimal(&self, address: usize) -> Result<f64, MemoryError> {
        let bytes: [u8; 16] = self.read_array(address)?;
        let flags = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let hi = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let lo = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let mid = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        let scale = ((flags >> 16) & 0xff) as i32;
        let reserved = flags & 0x7f00_ffff;
        if reserved != 0 || scale > 28 {
            return Err(corrupt(".NET decimal", address, "invalid flags or scale"));
        }
        let magnitude = (lo as f64)
            + (mid as f64) * 4_294_967_296.0
            + (hi as f64) * 18_446_744_073_709_551_616.0;
        let value = magnitude / 10_f64.powi(scale);
        Ok(if flags & 0x8000_0000 != 0 {
            -value
        } else {
            value
        })
    }

    fn read_array<const N: usize>(&self, address: usize) -> Result<[u8; N], MemoryError> {
        let mut bytes = [0; N];
        self.read_exact(address, &mut bytes)?;
        Ok(bytes)
    }

    pub fn read_array_header(&self, address: usize) -> Result<ManagedArrayHeader, MemoryError> {
        if address == 0 {
            return Err(corrupt("managed array", address, "null object pointer"));
        }
        let length_address = checked_address_add(address, self.pointer_size())?;
        let len =
            checked_collection_length(self.read_i32(length_address)?, "managed array", address)?;
        Ok(ManagedArrayHeader {
            len,
            data: checked_address_add(address, self.pointer_size() * 2)?,
        })
    }

    pub fn read_list_header(&self, address: usize) -> Result<ManagedListHeader, MemoryError> {
        if address == 0 {
            return Err(corrupt("managed list", address, "null object pointer"));
        }
        let items_address = checked_address_add(address, self.pointer_size())?;
        let len_address = checked_address_add(address, self.pointer_size() * 2)?;
        let mut pointer_bytes = [0; 8];
        let mut len_bytes = [0; 4];
        let pointer_slice = &mut pointer_bytes[..self.pointer_size()];
        self.read_many(&mut [
            ReadRequest::new(items_address, pointer_slice),
            ReadRequest::new(len_address, &mut len_bytes),
        ])?;
        let items = match self.bitness {
            ProcessBitness::X86 => {
                u32::from_le_bytes(pointer_bytes[..4].try_into().unwrap()) as usize
            }
            ProcessBitness::X64 => {
                usize::try_from(u64::from_le_bytes(pointer_bytes)).map_err(|_| {
                    corrupt(
                        "managed list",
                        address,
                        "backing array pointer does not fit the host",
                    )
                })?
            }
        };
        let len =
            checked_collection_length(i32::from_le_bytes(len_bytes), "managed list", address)?;
        Ok(ManagedListHeader { items, len })
    }

    pub fn read_dictionary_header(
        &self,
        address: usize,
    ) -> Result<ManagedDictionaryHeader, MemoryError> {
        if address == 0 {
            return Err(corrupt(
                "managed dictionary",
                address,
                "null object pointer",
            ));
        }
        let entries_pointer_address = checked_address_add(address, self.pointer_size() * 2)?;
        let entries = self.read_pointer(entries_pointer_address)?;
        if entries == 0 {
            return Ok(ManagedDictionaryHeader {
                entries: 0,
                len: 0,
                data: 0,
                entry_size: if self.pointer_size() == 8 { 24 } else { 16 },
            });
        }
        let array = self.read_array_header(entries)?;
        Ok(ManagedDictionaryHeader {
            entries,
            len: array.len,
            data: array.data,
            entry_size: if self.pointer_size() == 8 { 24 } else { 16 },
        })
    }

    /// Returns the first match in region order, then ascending offset order
    /// (`vosu_sys::process::scanner`'s generic primitive over this
    /// handle's own `ProcessMemory` backend, or the injected `ScanBackend`
    /// when one was given to [`Self::with_scan_backend`]).
    pub fn scan_first(&self, pattern: &Pattern) -> Result<Option<usize>, MemoryError> {
        match &self.scans {
            Some(backend) => Ok(backend.scan_all(pattern)?.into_iter().next()),
            None => self.memory.scan_first(pattern),
        }
    }

    /// Scans candidates in address order and stops at the first candidate
    /// accepted by `validate`, without materializing every signature match
    /// (unless a `ScanBackend` is injected, which must already produce
    /// every match to answer `scan_all`).
    pub fn scan_first_valid(
        &self,
        pattern: &Pattern,
        mut validate: impl FnMut(usize) -> Result<bool, MemoryError>,
    ) -> Result<Option<usize>, MemoryError> {
        match &self.scans {
            Some(backend) => {
                for candidate in backend.scan_all(pattern)? {
                    if validate(candidate)? {
                        return Ok(Some(candidate));
                    }
                }
                Ok(None)
            }
            None => self.memory.scan_first_valid(pattern, validate),
        }
    }

    /// Returns every match in region order, then ascending offset order.
    pub fn scan_all(&self, pattern: &Pattern) -> Result<Vec<usize>, MemoryError> {
        match &self.scans {
            Some(backend) => backend.scan_all(pattern),
            None => self.memory.scan_all(pattern),
        }
    }

    /// Finds at most one match per pattern. Regions are traversed first and
    /// unresolved patterns are emitted in caller-index order within a region.
    pub fn scan_batch(&self, patterns: &[Pattern]) -> Result<Vec<BatchScanResult>, MemoryError> {
        match &self.scans {
            Some(backend) => backend.scan_batch(patterns),
            None => self.memory.scan_batch(patterns),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedArrayHeader {
    pub len: usize,
    pub data: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedListHeader {
    pub items: usize,
    pub len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedDictionaryHeader {
    pub entries: usize,
    pub len: usize,
    pub data: usize,
    pub entry_size: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedDictionaryEntry {
    pub key: i32,
    pub value: usize,
}

pub fn checked_address_add(base: usize, offset: usize) -> Result<usize, MemoryError> {
    base.checked_add(offset)
        .ok_or(MemoryError::AddressOverflow { base, offset })
}

fn checked_collection_length(
    length: i32,
    context: &'static str,
    address: usize,
) -> Result<usize, MemoryError> {
    if length < 0 {
        return Err(corrupt(context, address, "length is negative"));
    }
    let length = length as usize;
    if length > MAX_MANAGED_COLLECTION_LENGTH {
        return Err(corrupt(
            context,
            address,
            "length exceeds the managed collection limit",
        ));
    }
    Ok(length)
}

fn corrupt(context: &'static str, address: usize, message: &'static str) -> MemoryError {
    MemoryError::Corrupt {
        context,
        address,
        message,
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_prop;
