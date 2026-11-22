use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::*;

#[derive(Default)]
struct SparseMemory {
    bytes: BTreeMap<usize, u8>,
    exact_calls: AtomicUsize,
    many_calls: AtomicUsize,
    many_requests: AtomicUsize,
    dropped: Option<Arc<AtomicBool>>,
}

impl SparseMemory {
    fn put(&mut self, address: usize, bytes: impl AsRef<[u8]>) {
        for (offset, byte) in bytes.as_ref().iter().copied().enumerate() {
            self.bytes.insert(address + offset, byte);
        }
    }

    fn put_u32(&mut self, address: usize, value: u32) {
        self.put(address, value.to_le_bytes());
    }

    fn put_i32(&mut self, address: usize, value: i32) {
        self.put(address, value.to_le_bytes());
    }

    fn put_u64(&mut self, address: usize, value: u64) {
        self.put(address, value.to_le_bytes());
    }

    fn fill(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let len = dst.len();
        for (offset, byte) in dst.iter_mut().enumerate() {
            *byte = *self
                .bytes
                .get(&(address + offset))
                .ok_or_else(|| MemoryError::Read {
                    address,
                    len,
                    message: "sparse fixture has no mapped byte".to_owned(),
                })?;
        }
        Ok(())
    }
}

impl Drop for SparseMemory {
    fn drop(&mut self) {
        if let Some(dropped) = &self.dropped {
            dropped.store(true, Ordering::SeqCst);
        }
    }
}

impl ProcessMemory for SparseMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.exact_calls.fetch_add(1, Ordering::SeqCst);
        self.fill(address, dst)
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        self.many_calls.fetch_add(1, Ordering::SeqCst);
        self.many_requests.fetch_add(reads.len(), Ordering::SeqCst);
        for read in reads {
            self.fill(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(vec![MemoryRegion {
            address: 0x1000,
            size: 0x10_000,
        }])
    }
}

fn fixture(memory: SparseMemory, bitness: ProcessBitness) -> (Arc<SparseMemory>, ProcessHandle) {
    let memory = Arc::new(memory);
    let handle = ProcessHandle::new(memory.clone(), bitness);
    (memory, handle)
}

#[test]
fn scalar_readers_are_little_endian() {
    let mut memory = SparseMemory::default();
    memory.put(0x1000, [0xfe]);
    memory.put(0x1010, 0x1234_u16.to_le_bytes());
    memory.put(0x1020, (-12_345_i16).to_le_bytes());
    memory.put(0x1030, 0x89ab_cdef_u32.to_le_bytes());
    memory.put(0x1040, (-123_456_789_i32).to_le_bytes());
    memory.put(0x1050, 0xfedc_ba98_7654_3210_u64.to_le_bytes());
    memory.put(0x1060, (-1_234_567_890_123_i64).to_le_bytes());
    memory.put(0x1070, 123.25_f32.to_le_bytes());
    memory.put(0x1080, (-98_765.125_f64).to_le_bytes());
    memory.put(0x1090, [1]);
    let (_, handle) = fixture(memory, ProcessBitness::X64);

    assert_eq!(handle.read_u8(0x1000).unwrap(), 0xfe);
    assert_eq!(handle.read_i8(0x1000).unwrap(), -2);
    assert_eq!(handle.read_u16(0x1010).unwrap(), 0x1234);
    assert_eq!(handle.read_i16(0x1020).unwrap(), -12_345);
    assert_eq!(handle.read_u32(0x1030).unwrap(), 0x89ab_cdef);
    assert_eq!(handle.read_i32(0x1040).unwrap(), -123_456_789);
    assert_eq!(handle.read_u64(0x1050).unwrap(), 0xfedc_ba98_7654_3210);
    assert_eq!(handle.read_i64(0x1060).unwrap(), -1_234_567_890_123);
    assert_eq!(handle.read_f32(0x1070).unwrap(), 123.25);
    assert_eq!(handle.read_f64(0x1080).unwrap(), -98_765.125);
    assert!(handle.read_bool(0x1090).unwrap());
}

#[test]
fn pointer_width_is_target_bitness_and_stays_integer() {
    let mut x86_memory = SparseMemory::default();
    x86_memory.put_u32(0x1000, 0xf123_4567);
    let (_, x86) = fixture(x86_memory, ProcessBitness::X86);
    assert_eq!(x86.read_pointer(0x1000).unwrap(), 0xf123_4567);

    let mut x64_memory = SparseMemory::default();
    let pointer = 0x0020_0000_0000_0001_u64;
    x64_memory.put_u64(0x1000, pointer);
    let (_, x64) = fixture(x64_memory, ProcessBitness::X64);
    assert_eq!(x64.read_pointer(0x1000).unwrap() as u64, pointer);
    assert_ne!(pointer, (pointer as f64) as u64);
}

#[test]
fn read_many_delegates_once_without_scalar_fallback() {
    let mut backend = SparseMemory::default();
    backend.put(0x1000, [1, 2]);
    backend.put(0x2000, [3, 4, 5]);
    let (backend, handle) = fixture(backend, ProcessBitness::X64);
    let mut first = [0; 2];
    let mut second = [0; 3];

    handle
        .read_many(&mut [
            ReadRequest::new(0x1000, &mut first),
            ReadRequest::new(0x2000, &mut second),
        ])
        .unwrap();

    assert_eq!(first, [1, 2]);
    assert_eq!(second, [3, 4, 5]);
    assert_eq!(backend.many_calls.load(Ordering::SeqCst), 1);
    assert_eq!(backend.many_requests.load(Ordering::SeqCst), 2);
    assert_eq!(backend.exact_calls.load(Ordering::SeqCst), 0);
}

fn put_x64_string(memory: &mut SparseMemory, address: usize, units: &[u16]) {
    memory.put_i32(address + 8, units.len() as i32);
    for (index, unit) in units.iter().enumerate() {
        memory.put(address + 12 + index * 2, unit.to_le_bytes());
    }
}

#[test]
fn managed_string_boundaries_and_utf16_validation() {
    let mut memory = SparseMemory::default();
    put_x64_string(&mut memory, 0x1000, &[]);
    let max = vec![b'a' as u16; 4095];
    put_x64_string(&mut memory, 0x2000, &max);
    memory.put_i32(0x5000 + 8, -1);
    memory.put_i32(0x6000 + 8, 4096);
    put_x64_string(&mut memory, 0x7000, &[0xd800]);
    let (_, handle) = fixture(memory, ProcessBitness::X64);

    assert_eq!(
        handle.read_managed_string(0x1000).unwrap(),
        ReadStatus::Ready(String::new())
    );
    assert_eq!(
        handle.read_managed_string(0x2000).unwrap(),
        ReadStatus::Ready("a".repeat(4095))
    );
    assert!(matches!(
        handle.read_managed_string(0x5000),
        Err(MemoryError::Corrupt { .. })
    ));
    assert!(matches!(
        handle.read_managed_string(0x6000),
        Err(MemoryError::Corrupt { .. })
    ));
    assert!(matches!(
        handle.read_managed_string(0x7000),
        Err(MemoryError::InvalidUtf16 { address: 0x7000 })
    ));
    assert_eq!(handle.read_managed_string(0).unwrap(), ReadStatus::NotReady);
}

#[test]
fn corrupt_and_overflowing_addresses_are_contextual_errors() {
    let (_, handle) = fixture(SparseMemory::default(), ProcessBitness::X64);
    assert!(matches!(
        handle.read_u32(0xdead),
        Err(MemoryError::Read {
            address: 0xdead,
            len: 4,
            ..
        })
    ));
    assert_eq!(
        checked_address_add(usize::MAX, 1),
        Err(MemoryError::AddressOverflow {
            base: usize::MAX,
            offset: 1
        })
    );
    assert!(matches!(
        handle.read_managed_string(usize::MAX),
        Err(MemoryError::AddressOverflow { .. })
    ));
}

#[test]
fn managed_array_and_list_decode_values() {
    let mut memory = SparseMemory::default();
    memory.put_i32(0x1000 + 8, 3);
    memory.put_i32(0x1000 + 16, 10);
    memory.put_i32(0x1000 + 20, 20);
    memory.put_i32(0x1000 + 24, 30);

    memory.put_u64(0x2000 + 8, 0x3000);
    memory.put_i32(0x2000 + 16, 2);
    memory.put_i32(0x3000 + 8, 4);
    memory.put_u64(0x3000 + 16, 0x4000);
    memory.put_u64(0x3000 + 24, 0x5000);
    memory.put_u64(0x3000 + 32, 0x6000);
    memory.put_u64(0x3000 + 40, 0x7000);
    let (backend, handle) = fixture(memory, ProcessBitness::X64);

    assert_eq!(
        handle
            .read_managed_array(0x1000, 4, ProcessHandle::read_i32)
            .unwrap(),
        vec![10, 20, 30]
    );
    assert_eq!(
        handle
            .read_managed_list(0x2000, 8, ProcessHandle::read_pointer)
            .unwrap(),
        vec![0x4000, 0x5000]
    );
    assert_eq!(backend.many_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn managed_dictionary_and_nullable_decode_values() {
    let mut memory = SparseMemory::default();
    memory.put_u64(0x1000 + 16, 0x2000);
    memory.put_i32(0x2000 + 8, 3);
    // entry 0 is empty
    memory.put_u64(0x2000 + 16, 0);
    memory.put_i32(0x2000 + 24, 0);
    memory.put_i32(0x2000 + 32, 0);
    // entry 1
    memory.put_u64(0x2000 + 40, 0xaaaa);
    memory.put_i32(0x2000 + 48, 42);
    memory.put_i32(0x2000 + 56, 7);
    // entry 2
    memory.put_u64(0x2000 + 64, 0xbbbb);
    memory.put_i32(0x2000 + 72, 91);
    memory.put_i32(0x2000 + 80, -3);
    memory.put(0x3000, [0]);
    memory.put(0x3010, [1]);
    memory.put_i32(0x3014, -123);
    let (_, handle) = fixture(memory, ProcessBitness::X64);

    assert_eq!(
        handle.read_managed_dictionary_i32_to_ref(0x1000).unwrap(),
        vec![
            ManagedDictionaryEntry {
                key: 7,
                value: 0xaaaa
            },
            ManagedDictionaryEntry {
                key: -3,
                value: 0xbbbb
            }
        ]
    );
    assert_eq!(handle.read_nullable_i32(0x3000).unwrap(), None);
    assert_eq!(handle.read_nullable_i32(0x3010).unwrap(), Some(-123));
}

#[test]
fn dotnet_datetime_and_decimal_known_vectors() {
    let mut memory = SparseMemory::default();
    let expected = Utc.with_ymd_and_hms(2024, 1, 2, 3, 4, 5).unwrap();
    let ticks =
        DOTNET_UNIX_EPOCH_TICKS + expected.timestamp() as u64 * DOTNET_TICKS_PER_SECOND + 6_789_000;
    memory.put_u64(0x1000, ticks | (1_u64 << 62));
    memory.put_u64(0x1010, 0);

    // decimal 123.45m: flags(scale=2), hi=0, lo=12345, mid=0.
    memory.put_u32(0x2000, 2 << 16);
    memory.put_u32(0x2004, 0);
    memory.put_u32(0x2008, 12_345);
    memory.put_u32(0x200c, 0);
    // decimal -1.5m
    memory.put_u32(0x3000, 0x8001_0000);
    memory.put_u32(0x3004, 0);
    memory.put_u32(0x3008, 15);
    memory.put_u32(0x300c, 0);
    let (_, handle) = fixture(memory, ProcessBitness::X64);

    assert_eq!(
        handle.read_dotnet_datetime(0x1000).unwrap(),
        expected + chrono::Duration::milliseconds(678) + chrono::Duration::microseconds(900)
    );
    assert_eq!(
        handle.read_dotnet_datetime(0x1010).unwrap(),
        Utc.with_ymd_and_hms(1, 1, 1, 0, 0, 0).unwrap()
    );
    assert_eq!(handle.read_dotnet_decimal(0x2000).unwrap(), 123.45);
    assert_eq!(handle.read_dotnet_decimal(0x3000).unwrap(), -1.5);
}

#[test]
fn backend_is_dropped_after_last_handle_owner() {
    let dropped = Arc::new(AtomicBool::new(false));
    let mut backend = SparseMemory::default();
    backend.dropped = Some(dropped.clone());
    let backend: Arc<dyn ProcessMemory> = Arc::new(backend);
    let first = ProcessHandle::new(backend.clone(), ProcessBitness::X64);
    let second = first.clone();
    drop(backend);
    drop(first);
    assert!(!dropped.load(Ordering::SeqCst));
    drop(second);
    assert!(dropped.load(Ordering::SeqCst));
}
