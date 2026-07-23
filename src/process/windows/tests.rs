use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

static CLOSE_CALLS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "system" fn record_close(_handle: HANDLE) -> i32 {
    CLOSE_CALLS.fetch_add(1, Ordering::Relaxed);
    1
}

#[test]
fn owned_handle_closes_exactly_once() {
    CLOSE_CALLS.store(0, Ordering::Relaxed);
    {
        let handle = OwnedHandle::with_closer(1usize as HANDLE, record_close);
        assert_eq!(handle.raw(), 1usize as HANDLE);
    }
    assert_eq!(CLOSE_CALLS.load(Ordering::Relaxed), 1);
}

#[test]
fn native_and_wow64_unicode_layouts_decode() {
    let mut native = [0u8; 16];
    native[0..2].copy_from_slice(&8u16.to_le_bytes());
    native[2..4].copy_from_slice(&10u16.to_le_bytes());
    native[8..16].copy_from_slice(&0x1234_5678_9abc_def0u64.to_le_bytes());
    let value = decode_unicode_descriptor(&native, false).unwrap();
    assert_eq!(
        (value.length, value.buffer),
        (8, 0x1234_5678_9abc_def0usize)
    );

    let mut wow64 = [0u8; 8];
    wow64[0..2].copy_from_slice(&6u16.to_le_bytes());
    wow64[2..4].copy_from_slice(&8u16.to_le_bytes());
    wow64[4..8].copy_from_slice(&0x7654_3210u32.to_le_bytes());
    let value = decode_unicode_descriptor(&wow64, true).unwrap();
    assert_eq!((value.length, value.buffer), (6, 0x7654_3210));
}

#[test]
fn exact_read_rejects_and_zeroes_partial_data() {
    let mut dst = [9u8; 4];
    let error = read_exact_impl(0x2000, &mut dst, |_address, dst| {
        dst[..2].copy_from_slice(&[1, 2]);
        (false, 2)
    })
    .unwrap_err();
    assert!(matches!(
        error,
        MemoryError::Read {
            address: 0x2000,
            len: 4,
            ..
        }
    ));
    assert_eq!(dst, [0; 4]);
}

#[test]
fn singleton_false_full_read_is_error_and_zeroes_destination() {
    let mut dst = [9u8; 4];
    let mut reads = [ReadRequest::new(0x1234, &mut dst)];
    let mut calls = 0;
    let error = read_many_impl(&mut reads, |address, dst| {
        calls += 1;
        assert_eq!((address, dst.len()), (0x1234, 4));
        dst.copy_from_slice(&[1, 2, 3, 4]);
        (false, dst.len())
    })
    .unwrap_err();
    assert!(matches!(
        error,
        MemoryError::Read {
            address: 0x1234,
            len: 4,
            ..
        }
    ));
    assert_eq!(calls, 1);
    assert_eq!(dst, [0; 4]);
}

#[test]
fn merged_false_full_read_is_error_and_zeroes_destinations() {
    let mut a = [9u8; 2];
    let mut b = [9u8; 3];
    let mut reads = [
        ReadRequest::new(0x1008, &mut a),
        ReadRequest::new(0x1010, &mut b),
    ];
    let mut calls = 0;
    let error = read_many_impl(&mut reads, |address, dst| {
        calls += 1;
        assert_eq!((address, dst.len()), (0x1008, 11));
        dst.copy_from_slice(&[1, 2, 9, 9, 9, 9, 9, 9, 3, 4, 5]);
        (false, dst.len())
    })
    .unwrap_err();
    assert!(matches!(
        error,
        MemoryError::Read {
            address: 0x1008,
            len: 11,
            ..
        }
    ));
    assert_eq!(calls, 1);
    assert_eq!(a, [0; 2]);
    assert_eq!(b, [0; 3]);
}

#[test]
fn short_coalesced_read_retries_suffix() {
    let mut a = [0u8; 2];
    let mut b = [0u8; 3];
    let mut reads = [
        ReadRequest::new(0x1000, &mut a),
        ReadRequest::new(0x1002, &mut b),
    ];
    let mut calls = 0;
    read_many_impl(&mut reads, |address, dst| {
        calls += 1;
        if calls == 1 {
            dst[..3].copy_from_slice(&[1, 2, 3]);
            (false, 3)
        } else {
            assert_eq!(address, 0x1003);
            dst.copy_from_slice(&[4, 5]);
            (true, 2)
        }
    })
    .unwrap();
    assert_eq!(a, [1, 2]);
    assert_eq!(b, [3, 4, 5]);
    assert_eq!(calls, 2);
}

#[test]
fn gapped_same_page_reads_once_and_scatters() {
    let mut a = [0u8; 2];
    let mut b = [0u8; 3];
    let mut reads = [
        ReadRequest::new(0x1008, &mut a),
        ReadRequest::new(0x1010, &mut b),
    ];
    let mut calls = 0;
    read_many_impl(&mut reads, |address, dst| {
        calls += 1;
        assert_eq!(address, 0x1008);
        assert_eq!(dst.len(), 11);
        dst.copy_from_slice(&[1, 2, 9, 9, 9, 9, 9, 9, 3, 4, 5]);
        (true, dst.len())
    })
    .unwrap();
    assert_eq!(calls, 1);
    assert_eq!(a, [1, 2]);
    assert_eq!(b, [3, 4, 5]);
}

#[test]
fn page_boundary_keeps_reads_separate() {
    let mut a = [0u8; 2];
    let mut b = [0u8; 2];
    let mut reads = [
        ReadRequest::new(0x1ffe, &mut a),
        ReadRequest::new(0x2000, &mut b),
    ];
    let mut calls = 0;
    read_many_impl(&mut reads, |address, dst| {
        calls += 1;
        match calls {
            1 => {
                assert_eq!((address, dst.len()), (0x1ffe, 2));
                dst.copy_from_slice(&[1, 2]);
            }
            2 => {
                assert_eq!((address, dst.len()), (0x2000, 2));
                dst.copy_from_slice(&[3, 4]);
            }
            _ => panic!("unexpected backend call"),
        }
        (true, dst.len())
    })
    .unwrap();
    assert_eq!(calls, 2);
    assert_eq!(a, [1, 2]);
    assert_eq!(b, [3, 4]);
}

#[test]
fn singleton_short_read_retries_only_suffix() {
    let mut dst = [0u8; 4];
    let mut reads = [ReadRequest::new(0x1234, &mut dst)];
    let mut calls = 0;
    read_many_impl(&mut reads, |address, dst| {
        calls += 1;
        match calls {
            1 => {
                assert_eq!((address, dst.len()), (0x1234, 4));
                dst[..2].copy_from_slice(&[1, 2]);
                (false, 2)
            }
            2 => {
                assert_eq!((address, dst.len()), (0x1236, 2));
                dst.copy_from_slice(&[3, 4]);
                (true, 2)
            }
            _ => panic!("unexpected backend call"),
        }
    })
    .unwrap();
    assert_eq!(calls, 2);
    assert_eq!(dst, [1, 2, 3, 4]);
}

#[test]
fn final_failure_zeroes_every_destination() {
    let mut a = [9u8; 2];
    let mut b = [9u8; 2];
    let mut reads = [
        ReadRequest::new(0x1000, &mut a),
        ReadRequest::new(0x1002, &mut b),
    ];
    assert!(
        read_many_impl(&mut reads, |_address, dst| {
            dst[0] = 7;
            (false, 1)
        })
        .is_err()
    );
    assert_eq!(a, [0, 0]);
    assert_eq!(b, [0, 0]);
}

#[cfg(target_os = "windows")]
#[test]
fn native_self_read() {
    let expected = Box::new(0x5a17_c0de_u64);
    let process = WindowsProcessMemory::open(std::process::id()).unwrap();
    let mut bytes = [0u8; 8];
    process
        .read_exact((&*expected as *const u64) as usize, &mut bytes)
        .unwrap();
    assert_eq!(u64::from_le_bytes(bytes), *expected);
    assert!(!process.command_line().unwrap().is_empty());
}
