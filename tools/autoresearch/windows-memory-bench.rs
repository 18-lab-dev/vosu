#![allow(dead_code)]

use std::alloc::{alloc_zeroed, dealloc, handle_alloc_error, Layout};
use std::ffi::c_void;
use std::hint::black_box;
use std::process;
use std::slice;
use std::time::Instant;

#[derive(Debug)]
enum MemoryError {
    AddressOverflow { base: usize, offset: usize },
    Read {
        address: usize,
        len: usize,
        message: String,
    },
}

struct ReadRequest<'a> {
    address: usize,
    dst: &'a mut [u8],
}

#[path = "../../src/process/windows/read_impl.rs"]
mod read_impl;

use read_impl::{read_exact_impl, read_many_impl};

const PAGE_SIZE: usize = 4096;
const SAMPLE_COUNT: usize = 31;
const OPS_PER_SAMPLE: usize = 16_384;
const READ_SIZE: usize = 64;
const REQUEST_COUNT: usize = 8;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcess() -> *mut c_void;
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtReadVirtualMemory(
        process: *mut c_void,
        base_address: *const c_void,
        buffer: *mut c_void,
        size: usize,
        bytes_read: *mut usize,
    ) -> i32;
}

struct AlignedPage {
    ptr: *mut u8,
    layout: Layout,
}

impl AlignedPage {
    fn new() -> Self {
        let layout = Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).unwrap();
        // SAFETY: `layout` has a non-zero size and a valid power-of-two alignment.
        let ptr = unsafe { alloc_zeroed(layout) };
        if ptr.is_null() {
            handle_alloc_error(layout);
        }
        Self { ptr, layout }
    }

    fn as_slice(&mut self) -> &mut [u8] {
        // SAFETY: `ptr` points to the live allocation described by `layout`.
        unsafe { slice::from_raw_parts_mut(self.ptr, self.layout.size()) }
    }

    fn address(&self) -> usize {
        self.ptr as usize
    }
}

impl Drop for AlignedPage {
    fn drop(&mut self) {
        // SAFETY: `ptr` was allocated with exactly `layout` in `new` and is dropped once.
        unsafe { dealloc(self.ptr, self.layout) };
    }
}

fn raw_read(process: *mut c_void, address: usize, dst: &mut [u8]) -> (bool, usize) {
    if dst.is_empty() {
        return (true, 0);
    }
    let mut bytes_read = 0usize;
    // SAFETY: the process handle is the current-process pseudo-handle; the source points
    // into this process's live page and the destination is the valid mutable slice supplied
    // by the caller.
    let status = unsafe {
        NtReadVirtualMemory(
            process,
            address as *const c_void,
            dst.as_mut_ptr() as *mut c_void,
            dst.len(),
            &mut bytes_read,
        )
    };
    (status >= 0, bytes_read)
}

fn checked_read_exact(
    process: *mut c_void,
    address: usize,
    dst: &mut [u8],
) -> Result<(), MemoryError> {
    read_exact_impl(address, dst, |source, destination| {
        raw_read(process, source, destination)
    })
}

fn checked_read_many(
    process: *mut c_void,
    reads: &mut [ReadRequest<'_>],
) -> Result<(), MemoryError> {
    read_many_impl(reads, |source, destination| {
        raw_read(process, source, destination)
    })
}

fn expected_byte(index: usize) -> u8 {
    (index as u8).wrapping_mul(37).wrapping_add(11)
}

fn median_ns_per_operation(samples: &mut [u128]) -> f64 {
    samples.sort_unstable();
    let middle = samples.len() / 2;
    let median = if samples.len() % 2 == 1 {
        samples[middle] as f64
    } else {
        (samples[middle - 1] as f64 + samples[middle] as f64) * 0.5
    };
    median / OPS_PER_SAMPLE as f64
}

fn verify_bytes(actual: &[u8], expected: &[u8], label: &str) -> Result<(), String> {
    if actual != expected {
        return Err(format!(
            "{label} returned unexpected bytes ({} bytes)",
            actual.len()
        ));
    }
    Ok(())
}

fn bench_read_exact(process: *mut c_void, address: usize, expected: &[u8]) -> Result<f64, String> {
    let mut samples = [0u128; SAMPLE_COUNT];
    let mut destination = [0u8; READ_SIZE];
    for _ in 0..32 {
        checked_read_exact(process, address, &mut destination).map_err(|error| format!("warmup read_exact failed: {error:?}"))?;
    }
    for sample in &mut samples {
        let started = Instant::now();
        for _ in 0..OPS_PER_SAMPLE {
            checked_read_exact(process, address, &mut destination)
                .map_err(|error| format!("read_exact failed: {error:?}"))?;
        }
        *sample = started.elapsed().as_nanos();
        verify_bytes(&destination, expected, "read_exact")?;
        black_box(&destination);
    }
    Ok(median_ns_per_operation(&mut samples))
}

fn bench_read_many_singleton(
    process: *mut c_void,
    address: usize,
    expected: &[u8],
) -> Result<f64, String> {
    let mut samples = [0u128; SAMPLE_COUNT];
    let mut destination = [0u8; READ_SIZE];
    for _ in 0..32 {
        let mut reads = [ReadRequest {
            address,
            dst: &mut destination,
        }];
        checked_read_many(process, &mut reads)
            .map_err(|error| format!("warmup read_many singleton failed: {error:?}"))?;
    }
    for sample in &mut samples {
        let started = Instant::now();
        for _ in 0..OPS_PER_SAMPLE {
            let mut reads = [ReadRequest {
                address,
                dst: &mut destination,
            }];
            checked_read_many(process, &mut reads)
                .map_err(|error| format!("read_many singleton failed: {error:?}"))?;
        }
        *sample = started.elapsed().as_nanos();
        verify_bytes(&destination, expected, "read_many singleton")?;
        black_box(&destination);
    }
    Ok(median_ns_per_operation(&mut samples))
}

fn make_group_reads<'a>(
    addresses: &[usize; REQUEST_COUNT],
    destinations: &'a mut [[u8; 8]; REQUEST_COUNT],
) -> [ReadRequest<'a>; REQUEST_COUNT] {
    let pointer = destinations.as_mut_ptr();
    // SAFETY: each pointer is in bounds and points to a distinct array element.
    unsafe {
        [
            ReadRequest { address: addresses[0], dst: &mut *pointer.add(0) },
            ReadRequest { address: addresses[1], dst: &mut *pointer.add(1) },
            ReadRequest { address: addresses[2], dst: &mut *pointer.add(2) },
            ReadRequest { address: addresses[3], dst: &mut *pointer.add(3) },
            ReadRequest { address: addresses[4], dst: &mut *pointer.add(4) },
            ReadRequest { address: addresses[5], dst: &mut *pointer.add(5) },
            ReadRequest { address: addresses[6], dst: &mut *pointer.add(6) },
            ReadRequest { address: addresses[7], dst: &mut *pointer.add(7) },
        ]
    }
}

fn bench_read_many_grouped(
    process: *mut c_void,
    addresses: &[usize; REQUEST_COUNT],
    expected: &[&[u8]; REQUEST_COUNT],
    label: &str,
) -> Result<f64, String> {
    let mut samples = [0u128; SAMPLE_COUNT];
    let mut destinations = [[0u8; 8]; REQUEST_COUNT];
    for _ in 0..32 {
        let mut reads = make_group_reads(addresses, &mut destinations);
        checked_read_many(process, &mut reads)
            .map_err(|error| format!("warmup {label} failed: {error:?}"))?;
    }
    for sample in &mut samples {
        let started = Instant::now();
        for _ in 0..OPS_PER_SAMPLE {
            let mut reads = make_group_reads(addresses, &mut destinations);
            checked_read_many(process, &mut reads)
                .map_err(|error| format!("{label} failed: {error:?}"))?;
        }
        *sample = started.elapsed().as_nanos();
        for (index, destination) in destinations.iter().enumerate() {
            verify_bytes(destination, expected[index], label)?;
        }
        black_box(&destinations);
    }
    Ok(median_ns_per_operation(&mut samples))
}

fn run() -> Result<(), String> {
    let process = unsafe { GetCurrentProcess() };
    if process.is_null() {
        return Err("GetCurrentProcess returned a null handle".to_owned());
    }
    let mut page = AlignedPage::new();
    let base = page.address();
    let page_bytes = page.as_slice();
    for (index, byte) in page_bytes.iter_mut().enumerate() {
        *byte = expected_byte(index);
    }
    if base & (PAGE_SIZE - 1) != 0 {
        return Err(format!("allocation is not {PAGE_SIZE}-byte aligned"));
    }

    let exact_address = base + 128;
    let exact_expected = &page_bytes[128..128 + READ_SIZE];
    let singleton_ns = bench_read_many_singleton(process, exact_address, exact_expected)?;
    let exact_ns = bench_read_exact(process, exact_address, exact_expected)?;

    let contiguous_addresses = std::array::from_fn(|index| base + 512 + index * 8);
    let contiguous_expected: [&[u8]; REQUEST_COUNT] =
        std::array::from_fn(|index| &page_bytes[512 + index * 8..512 + index * 8 + 8]);
    let contiguous_ns = bench_read_many_grouped(
        process,
        &contiguous_addresses,
        &contiguous_expected,
        "read_many contiguous",
    )?;

    let gapped_addresses = std::array::from_fn(|index| base + 768 + index * 64);
    let gapped_expected: [&[u8]; REQUEST_COUNT] =
        std::array::from_fn(|index| &page_bytes[768 + index * 64..768 + index * 64 + 8]);
    let gapped_ns = bench_read_many_grouped(
        process,
        &gapped_addresses,
        &gapped_expected,
        "read_many gapped",
    )?;

    println!("METRIC windows_read_many_gapped_ns={gapped_ns:.2}");
    println!("METRIC windows_read_exact_ns={exact_ns:.2}");
    println!("METRIC windows_read_many_single_ns={singleton_ns:.2}");
    println!("METRIC windows_read_many_contiguous_ns={contiguous_ns:.2}");
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("windows memory benchmark failed: {error}");
        process::exit(1);
    }
}
