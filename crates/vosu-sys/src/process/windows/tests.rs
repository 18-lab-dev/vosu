use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn module_snapshot_retries_only_transient_errors() {
    for flags in [TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32] {
        let mut errors = [Err(ERROR_BAD_LENGTH), Err(ERROR_PARTIAL_COPY), Ok(17)].into_iter();
        let mut yields = 0;
        assert_eq!(
            snapshot_with_retry(flags, || errors.next().unwrap(), || yields += 1),
            Ok(17)
        );
        assert_eq!(yields, 2);
    }
    for (flags, code) in [
        (TH32CS_SNAPTHREAD, ERROR_BAD_LENGTH),
        (TH32CS_SNAPPROCESS, ERROR_PARTIAL_COPY),
        (TH32CS_SNAPMODULE, ERROR_ACCESS_DENIED),
    ] {
        let mut calls = 0;
        assert_eq!(
            snapshot_with_retry::<()>(flags, || { calls += 1; Err(code) }, || panic!("unexpected retry")),
            Err(code)
        );
        assert_eq!(calls, 1);
    }
}

#[test]
fn module_snapshot_retry_is_bounded_and_preserves_final_error() {
    let (mut calls, mut yields) = (0, 0);
    assert_eq!(
        snapshot_with_retry::<()>(
            TH32CS_SNAPMODULE,
            || { calls += 1; Err(ERROR_PARTIAL_COPY) },
            || yields += 1,
        ),
        Err(ERROR_PARTIAL_COPY)
    );
    assert_eq!(calls, 8);
    assert_eq!(yields, 7);
}

#[test]
fn same_account_process_open_retains_native_memory_read_access() {
    let expected = 0x7135_a9bd_2468_cdef_u64;
    let memory = WindowsProcessMemory::open(std::process::id()).unwrap();
    let mut output = [0u8; 8];
    memory
        .read_exact((&expected as *const u64) as usize, &mut output)
        .unwrap();
    assert_eq!(u64::from_ne_bytes(output), expected);
}
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
fn wide_same_page_reads_once_and_scatters() {
    let mut a = [0u8; 2];
    let mut b = [0u8; 2];
    let mut reads = [
        ReadRequest::new(0x1000, &mut a),
        ReadRequest::new(0x1400, &mut b),
    ];
    let mut calls = 0;
    read_many_impl(&mut reads, |address, dst| {
        calls += 1;
        assert_eq!((address, dst.len()), (0x1000, 0x402));
        dst[..2].copy_from_slice(&[1, 2]);
        dst[0x400..].copy_from_slice(&[3, 4]);
        (true, dst.len())
    })
    .unwrap();
    assert_eq!(calls, 1);
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

#[cfg(target_os = "windows")]
#[test]
fn create_time_from_the_process_list_matches_get_process_times_for_self_and_a_child() {
    let self_pid = std::process::id();
    let self_create_time = WindowsProcessMemory::open(self_pid)
        .unwrap()
        .creation_time()
        .unwrap();

    let mut child = std::process::Command::new("cmd.exe")
        .args(["/C", "ping", "-n", "5", "127.0.0.1"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let child_pid = child.id();
    let child_create_time = WindowsProcessMemory::open(child_pid)
        .unwrap()
        .creation_time()
        .unwrap();

    let entries = enumerate_processes().unwrap();
    let self_entry = entries
        .iter()
        .find(|entry| entry.pid == self_pid)
        .expect("self must be in the process list");
    let child_entry = entries
        .iter()
        .find(|entry| entry.pid == child_pid)
        .expect("the spawned child must be in the process list");

    assert_eq!(
        self_entry.create_time, self_create_time,
        "SYSTEM_PROCESS_INFORMATION.CreateTime must match GetProcessTimes for self"
    );
    assert_eq!(
        child_entry.create_time, child_create_time,
        "SYSTEM_PROCESS_INFORMATION.CreateTime must match GetProcessTimes for a spawned child"
    );

    child.kill().unwrap();
    child.wait().unwrap();
}

#[cfg(target_os = "windows")]
#[test]
fn process_image_path_returns_the_full_path_of_a_spawned_child_and_matches_current_exe_for_self() {
    let self_path = process_image_path(std::process::id()).unwrap();
    let current_exe = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        self_path.to_ascii_lowercase(),
        current_exe.to_ascii_lowercase(),
        "process_image_path for self must equal std::env::current_exe"
    );

    let mut child = std::process::Command::new("cmd.exe")
        .args(["/C", "ping", "-n", "5", "127.0.0.1"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let child_pid = child.id();
    let child_path = process_image_path(child_pid).unwrap();
    let expected_comspec = std::env::var_os("ComSpec")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows\System32\cmd.exe"));
    let expected_name = expected_comspec
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase());
    let actual_name = std::path::Path::new(&child_path)
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase());
    assert_eq!(
        actual_name, expected_name,
        "process_image_path must return the child's own executable path"
    );
    assert!(
        child_path.contains(':') && child_path.len() > child_path.rfind(['/', '\\']).unwrap_or(0),
        "the returned path must be a full Win32 path, not a bare name: {child_path}"
    );

    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn is_process_alive_reports_the_running_self_process() {
    assert!(is_process_alive(std::process::id()));
}

#[cfg(target_os = "windows")]
#[test]
fn is_process_alive_reports_a_recycled_pid_as_dead() {
    // A freshly spawned-then-reaped child's pid is very unlikely to be
    // reused by the time this check runs.
    let mut child = std::process::Command::new("cmd.exe")
        .args(["/C", "exit"])
        .spawn()
        .unwrap();
    let pid = child.id();
    child.wait().unwrap();
    assert!(!is_process_alive(pid));
}

#[cfg(target_os = "windows")]
#[test]
fn is_process_alive_reports_a_protected_process_light_target_as_alive() {
    // `csrss.exe`/`smss.exe` are Protected-Process-Light: `OpenProcess`
    // with even `PROCESS_QUERY_LIMITED_INFORMATION` fails with
    // `ERROR_ACCESS_DENIED` for them, an error only a live process can
    // produce. Pick whichever of the two is present (both always are on
    // a real Windows host, but this stays robust to either).
    let entries = enumerate_processes().unwrap();
    let ppl = entries
        .iter()
        .find(|entry| {
            entry.executable.eq_ignore_ascii_case("csrss.exe")
                || entry.executable.eq_ignore_ascii_case("smss.exe")
        })
        .expect("csrss.exe or smss.exe must be present on a real Windows host");
    assert!(
        is_process_alive(ppl.pid),
        "a PPL target denying PROCESS_QUERY_LIMITED_INFORMATION must still be reported alive"
    );
}

#[cfg(target_os = "windows")]
#[test]
fn is_process_alive_tracks_a_spawned_child_until_it_is_reaped() {
    let mut child = std::process::Command::new("cmd.exe")
        .args(["/C", "ping", "-n", "5", "127.0.0.1"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    assert!(
        is_process_alive(pid),
        "the spawned child must be alive while it runs"
    );

    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        !is_process_alive(pid),
        "the child must no longer be alive once it has been killed and reaped"
    );
}

#[cfg(target_os = "windows")]
#[test]
fn is_process_alive_reports_a_pid_absent_from_the_process_list_as_dead() {
    let entries = enumerate_processes().unwrap();
    let free_pid = (1u32..)
        .step_by(4)
        .find(|candidate| entries.iter().all(|entry| entry.pid != *candidate))
        .expect("some low pid must be unused");
    assert!(!is_process_alive(free_pid));
}
