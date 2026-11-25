//! Synthetic target process for `benches/brokered_reads_bench.rs`.
//!
//! Lays out two pointer-chain buffers (a "regular" shape and a "precise" shape,
//! both derived in `brokered_reads_bench.rs`'s module doc comment) at addresses
//! fixed relative to one heap allocation, then reports that allocation's base
//! address and blocks until the parent benchmark closes stdin. The parent reads
//! this process's memory natively (`process_vm_readv` / `ReadProcessMemory`),
//! exactly as a 1.x stable/lazer reader reads a game client; no IPC happens
//! inside this binary.
//!
//! It also reports, per plan shape, the SHA-256 of the exact response a
//! correct broker must return (`EXPECT <shape> <sha256>`). It computes that
//! by walking the same plans over its own buffer in-process, with no remote
//! read and no transport, so it independently checks every variant's
//! `result_sha256`. Output: `BASE <hex>`, one `EXPECT` line per shape, then
//! `READY`.
use std::io::{self, Read, Write};

use sha2::{Digest, Sha256};

#[path = "../benches/support/brokered/plans.rs"]
mod plans;

use plans::{Plan, Source};

/// Matches `brokered_reads_bench::TARGET_SIZE`; kept independent on purpose so
/// a mismatch fails loudly (out-of-bounds offset) rather than silently.
const TARGET_SIZE: usize = 1 << 20;
const KEY_COUNT: usize = 18;

fn put_u64(buffer: &mut [u8], offset: usize, value: u64) {
    buffer[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn put_i32(buffer: &mut [u8], offset: usize, value: i32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_f64(buffer: &mut [u8], offset: usize, value: f64) {
    buffer[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// Writes the precise-plan pointer chain: game base -> overlay -> controller ->
/// trigger list -> `KEY_COUNT` per-key trigger objects, plus a clock chain,
/// mirroring `Fixture::active_lazer` in `benches/memory_reader_bench.rs`.
fn write_precise_chain(buffer: &mut [u8], base: usize, origin: u64) {
    let overlay = 0x10_000;
    let controller = 0x11_000;
    let trigger_list = 0x12_000;
    let trigger_array = 0x13_000;
    let clock_source = 0x14_000;

    put_u64(buffer, base, origin + overlay as u64);
    put_u64(buffer, overlay, origin + controller as u64);
    put_u64(buffer, controller, origin + trigger_list as u64);
    put_i32(buffer, trigger_list + 0x10, KEY_COUNT as i32);
    put_u64(buffer, trigger_list + 8, origin + trigger_array as u64);
    for index in 0..KEY_COUNT {
        let trigger = 0x20_000 + index * 0x400;
        put_u64(
            buffer,
            trigger_array + 0x10 + index * 8,
            origin + trigger as u64,
        );
        put_i32(buffer, trigger + 0x14, (index * 10) as i32);
        buffer[trigger + 0x1c] = (index % 2) as u8;
    }
    put_u64(buffer, base + 8, origin + clock_source as u64);
    put_f64(buffer, clock_source, 321.5);
}

/// Writes the regular-plan pointer chain: a wider, shallower fan-out (global ->
/// beatmap/status/hash/settings/mp3 fields) matching the batch sizes
/// `src/memory/lazer/global_batch.rs` issues per independent field.
fn write_regular_chain(buffer: &mut [u8], base: usize, origin: u64) {
    let beatmap = 0x30_000;
    let settings = 0x31_000;
    let menu = 0x32_000;
    let mp3 = 0x33_000;

    put_u64(buffer, base, origin + beatmap as u64);
    put_u64(buffer, base + 8, origin + settings as u64);
    put_u64(buffer, base + 16, origin + menu as u64);
    put_u64(buffer, base + 24, origin + mp3 as u64);
    put_i32(buffer, beatmap + 0x20, 42);
    put_i32(buffer, beatmap + 0x24, 7);
    for (index, offset) in (0..16).map(|index| (index, 0x40 + index * 4)) {
        put_i32(buffer, settings + offset, index as i32);
    }
    put_f64(buffer, menu + 0x10, 123_456.0);
    put_i32(buffer, mp3 + 0x8, 180_000);
}

/// The response a correct broker returns for `plan`: every read's bytes in
/// stage/read order, read here straight out of `buffer` (whose first byte
/// lives at `origin`). Panics on an address outside the buffer.
fn evaluate_local(plan: &Plan, origin: u64, buffer: &[u8]) -> Vec<u8> {
    let slice = |address: u64, len: usize| {
        let start = usize::try_from(address.checked_sub(origin).expect("address above origin"))
            .expect("offset fits usize");
        &buffer[start..start + len]
    };
    let mut resolved: Vec<Vec<u64>> = Vec::with_capacity(plan.stages.len());
    let mut response = Vec::with_capacity(plan.bytes());
    for stage in &plan.stages {
        let mut pointers = Vec::with_capacity(stage.len());
        for op in stage {
            let base = match op.source {
                Source::Root => origin,
                Source::Prev(stage_index, op_index) => resolved[stage_index][op_index],
            };
            let bytes = slice(base.wrapping_add_signed(op.offset), op.len as usize);
            pointers.push(if op.as_pointer {
                u64::from_le_bytes(bytes[..8].try_into().expect("pointer read is 8 bytes"))
            } else {
                0
            });
            response.extend_from_slice(bytes);
        }
        resolved.push(pointers);
    }
    response
}

fn main() {
    let mut buffer = vec![0u8; TARGET_SIZE];
    let origin = buffer.as_ptr() as u64;
    write_precise_chain(&mut buffer, 0x0, origin);
    write_regular_chain(&mut buffer, 0x40, origin);

    println!("BASE {origin:#x}");
    for (shape, plan) in plans::shapes() {
        let response = evaluate_local(&plan, origin, &buffer);
        assert_eq!(response.len(), plan.bytes(), "{shape} response length");
        assert!(plan.reads() > 0, "{shape} plan has reads");
        let sha: String = Sha256::digest(&response)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        println!("EXPECT {shape} {sha}");
    }
    println!("READY");
    io::stdout().flush().expect("flush target report");

    // Block until the parent closes our stdin (end of benchmark run); keeps
    // `buffer` alive and this process addressable via `process_vm_readv` the
    // whole time without any busy loop.
    let mut discard = [0u8; 64];
    loop {
        match io::stdin().read(&mut discard) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
    std::hint::black_box(&buffer);
}
