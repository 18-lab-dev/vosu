use std::fs;
use std::os::unix::fs::symlink;
use std::sync::atomic::{AtomicUsize, Ordering};

use tempfile::TempDir;

use super::*;

#[derive(Debug, Clone, Copy)]
enum MockMode {
    Exact,
    ShortFirst(usize),
    Error(i32),
    ErrorOnCall(usize),
    ErrorFromCall(usize),
}

#[derive(Debug)]
struct MockSyscall {
    mode: MockMode,
    calls: AtomicUsize,
    max: usize,
}

impl MockSyscall {
    fn new(mode: MockMode, max: usize) -> Arc<Self> {
        Arc::new(Self {
            mode,
            calls: AtomicUsize::new(0),
            max,
        })
    }
}

impl ProcessVmReadv for MockSyscall {
    fn readv(
        &self,
        _pid: libc::pid_t,
        local: &mut [libc::iovec],
        remote: &[libc::iovec],
    ) -> Result<usize, i32> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if let MockMode::Error(errno) = self.mode {
            return Err(errno);
        }
        if matches!(self.mode, MockMode::ErrorOnCall(target) if target == call) {
            return Err(libc::EFAULT);
        }
        if matches!(self.mode, MockMode::ErrorFromCall(target) if call >= target) {
            return Err(libc::EFAULT);
        }
        let total: usize = local.iter().map(|iov| iov.iov_len).sum();
        let wanted = match self.mode {
            MockMode::ShortFirst(length) if call == 0 => length.min(total),
            _ => total,
        };
        let mut remaining = wanted;
        for (local, remote) in local.iter_mut().zip(remote) {
            let take = remaining.min(local.iov_len);
            // SAFETY: the test adapter receives borrowed destination buffers.
            let destination =
                unsafe { std::slice::from_raw_parts_mut(local.iov_base.cast::<u8>(), take) };
            for (index, byte) in destination.iter_mut().enumerate() {
                *byte = (remote.iov_base as usize + index) as u8;
            }
            remaining -= take;
            if remaining == 0 {
                break;
            }
        }
        Ok(wanted)
    }

    fn iov_max(&self) -> usize {
        self.max
    }
}

fn fixture() -> TempDir {
    tempfile::tempdir().expect("temporary proc fixture")
}

fn add_process(root: &Path, pid: u32, class: u8, cmdline: &[u8], maps: &str) -> PathBuf {
    let dir = root.join(pid.to_string());
    fs::create_dir_all(&dir).expect("process directory");
    let executable = root.join(format!("elf-{pid}"));
    fs::write(&executable, [0x7f, b'E', b'L', b'F', class]).expect("ELF fixture");
    symlink(&executable, dir.join("exe")).expect("exe symlink");
    symlink(root, dir.join("cwd")).expect("cwd symlink");
    fs::write(dir.join("cmdline"), cmdline).expect("cmdline");
    fs::write(dir.join("comm"), b"osu!\n").expect("comm");
    fs::write(dir.join("maps"), maps).expect("maps");
    fs::write(dir.join("stat"), format!("{pid} (osu! tricky) S 1 2 3")).expect("stat");
    executable
}

#[test]
fn enumerates_numeric_proc_entries_and_preserves_nul_arguments() {
    let fixture = fixture();
    fs::create_dir(fixture.path().join("self")).expect("noise directory");
    fs::create_dir(fixture.path().join("12x")).expect("numeric noise");
    add_process(
        fixture.path(),
        42,
        2,
        b"osu!\0--devserver=true\0non-utf8-\xff\0",
        "1000-2000 rw-p 0 00:00 0\n",
    );
    add_process(
        fixture.path(),
        7,
        1,
        b"wine\0Z:\\\\games\\\\osu!\\\\osu!.exe\0",
        "2000-3000 rw-p 0 00:00 0\n",
    );

    let found = enumerate_processes_at(fixture.path()).expect("enumeration");
    assert_eq!(
        found.iter().map(|process| process.pid).collect::<Vec<_>>(),
        [7, 42]
    );
    assert_eq!(found[0].name, "osu!");
    assert_eq!(found[0].bitness, ProcessBitness::X86);
    assert_eq!(found[1].bitness, ProcessBitness::X64);
    assert_eq!(found[1].command_line[1], "--devserver=true");
    assert_eq!(found[1].command_line[2].as_bytes(), b"non-utf8-\xff");
}

#[test]
fn matching_enumeration_filters_on_comm_before_reading_expensive_metadata() {
    let fixture = fixture();
    add_process(
        fixture.path(),
        7,
        2,
        b"osu!\0",
        "1000-2000 rw-p 0 00:00 0\n",
    );
    let helper = fixture.path().join("42");
    fs::create_dir(&helper).expect("helper process directory");
    fs::write(helper.join("comm"), b"helper\n").expect("helper comm");
    fs::write(fixture.path().join("7/comm"), b"osu!.exe\n").expect("candidate comm");

    let mut saw_helper = false;
    let found = enumerate_processes_matching_at(fixture.path(), |name| {
        saw_helper |= name == "helper";
        name.to_string_lossy().contains("osu!")
    })
    .expect("filtered enumeration");

    assert!(
        saw_helper,
        "the filter must inspect comm before other metadata"
    );
    assert_eq!(
        found.iter().map(|process| process.pid).collect::<Vec<_>>(),
        [7]
    );
}

#[test]
fn matching_single_pid_reads_comm_before_expensive_metadata() {
    let fixture = fixture();
    let helper = fixture.path().join("42");
    fs::create_dir(&helper).expect("helper process directory");
    fs::write(helper.join("comm"), b"helper\n").expect("helper comm");

    let found = process_metadata_matching_at(fixture.path(), 42, |name| name == "osu!")
        .expect("filtered inspection");
    assert!(found.is_none());

    add_process(
        fixture.path(),
        7,
        2,
        b"osu!\0",
        "1000-2000 rw-p 0 00:00 0\n",
    );
    let found = process_metadata_matching_at(fixture.path(), 7, |name| name == "osu!")
        .expect("targeted inspection")
        .expect("candidate metadata");
    assert_eq!(found.pid, 7);
}

#[test]
fn matching_single_pid_returns_none_when_comm_disappeared() {
    let fixture = fixture();

    let found = process_metadata_matching_at(fixture.path(), 42, |_| true)
        .expect("raced-away process inspection");

    assert!(found.is_none());
}

#[test]
fn matching_single_pid_returns_none_when_cmdline_disappeared() {
    let fixture = fixture();
    add_process(
        fixture.path(),
        42,
        2,
        b"osu!\0",
        "1000-2000 rw-p 0 00:00 0\n",
    );
    let cmdline = fixture.path().join("42/cmdline");

    let found = process_metadata_matching_at(fixture.path(), 42, |_| {
        fs::remove_file(&cmdline).expect("remove raced-away cmdline");
        true
    })
    .expect("raced-away process inspection");

    assert!(found.is_none());
}

#[test]
fn matching_single_pid_returns_none_when_exe_link_disappeared() {
    let fixture = fixture();
    add_process(
        fixture.path(),
        42,
        2,
        b"osu!\0",
        "1000-2000 rw-p 0 00:00 0\n",
    );
    let exe = fixture.path().join("42/exe");

    let found = process_metadata_matching_at(fixture.path(), 42, |_| {
        fs::remove_file(&exe).expect("remove raced-away exe link");
        true
    })
    .expect("raced-away process inspection");

    assert!(found.is_none());
}

#[test]
fn matching_single_pid_returns_none_when_exe_target_disappeared() {
    let fixture = fixture();
    let executable = add_process(
        fixture.path(),
        42,
        2,
        b"osu!\0",
        "1000-2000 rw-p 0 00:00 0\n",
    );

    let found = process_metadata_matching_at(fixture.path(), 42, |_| {
        fs::remove_file(&executable).expect("remove raced-away executable");
        true
    })
    .expect("raced-away process inspection");

    assert!(found.is_none());
}

#[test]
fn comm_is_trimmed_and_bounded() {
    let fixture = fixture();
    add_process(
        fixture.path(),
        5,
        2,
        b"osu!\0",
        "1000-2000 rw-p 0 00:00 0\n",
    );
    let dir = fixture.path().join("5");
    fs::write(dir.join("comm"), b"osu!.exe\r\n").expect("comm");
    assert_eq!(process_name_at(&dir).unwrap(), "osu!.exe");
    fs::write(dir.join("comm"), vec![b'x'; MAX_COMM_FILE as usize + 1]).expect("large comm");
    assert!(process_name_at(&dir).is_err());
}

#[test]
fn maps_keep_only_writable_readable_scan_regions() {
    let fixture = fixture();
    add_process(
        fixture.path(),
        9,
        2,
        b"osu!\0",
        concat!(
            "1000-1800 r--p 0 00:00 0\n",
            "2000-2800 rw-p 0 00:00 0\n",
            "3000-3800 r-xp 0 00:00 0\n",
            "4000-4800 rw-s 0 00:00 0\n"
        ),
    );
    let memory = LinuxProcessMemory::with_parts(
        9,
        fixture.path().to_path_buf(),
        MockSyscall::new(MockMode::Exact, 1024),
    );
    assert_eq!(
        memory.readable_regions().expect("regions"),
        [
            MemoryRegion {
                address: 0x2000,
                size: 0x800
            },
            MemoryRegion {
                address: 0x4000,
                size: 0x800
            },
        ]
    );
}

#[test]
fn liveness_rejects_missing_and_zombie_processes() {
    let fixture = fixture();
    add_process(
        fixture.path(),
        10,
        2,
        b"osu!\0",
        "1000-2000 rw-p 0 00:00 0\n",
    );
    assert!(is_process_alive_at(fixture.path(), 10));
    fs::write(
        fixture.path().join("10/stat"),
        "10 (name with ) chars) Z 1 2",
    )
    .expect("zombie stat");
    assert!(!is_process_alive_at(fixture.path(), 10));
    assert!(!is_process_alive_at(fixture.path(), 11));
}

#[cfg(feature = "linux-process-events")]
#[test]
fn pidfd_poll_treats_only_exit_readiness_as_dead() {
    assert!(pidfd_poll_reports_exit(1, libc::POLLIN));
    assert!(!pidfd_poll_reports_exit(0, 0));
    assert!(!pidfd_poll_reports_exit(-1, 0));
    assert!(!pidfd_poll_reports_exit(1, libc::POLLERR));
    assert!(!pidfd_poll_reports_exit(1, libc::POLLHUP));
}

#[cfg(feature = "linux-process-events")]
#[test]
fn pidfd_liveness_changes_only_when_retained_process_exits() {
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep");
    let memory = LinuxProcessMemory::new(child.id());
    assert!(memory.pidfd.is_some(), "pidfd_open must work on this Linux");
    assert!(memory.is_alive());

    child.kill().expect("kill sleep");
    child.wait().expect("reap sleep");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while memory.is_alive() && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(!memory.is_alive());
}

fn memory_fixture(
    mode: MockMode,
    max: usize,
    maps: &str,
) -> (TempDir, LinuxProcessMemory, Arc<MockSyscall>) {
    let fixture = fixture();
    add_process(fixture.path(), 20, 2, b"osu!\0", maps);
    let syscall = MockSyscall::new(mode, max);
    let memory = LinuxProcessMemory::with_parts(20, fixture.path().to_path_buf(), syscall.clone());
    (fixture, memory, syscall)
}

#[test]
fn numeric_liveness_fallback_keeps_proc_fixtures_working() {
    let (fixture, memory, _) = memory_fixture(MockMode::Exact, 1024, "1000-2000 rw-p 0 00:00 0\n");
    assert!(memory.pidfd.is_none());
    assert!(memory.is_alive());
    fs::write(fixture.path().join("20/stat"), "20 (osu!) Z 1 2").expect("zombie stat");
    assert!(!memory.is_alive());
}

#[test]
fn exact_and_short_prefix_reads_complete_suffixes() {
    let (_fixture, exact, _) = memory_fixture(MockMode::Exact, 1024, "1000-2000 rw-p 0 00:00 0\n");
    let mut bytes = [0; 8];
    exact.read_exact(0x1010, &mut bytes).expect("exact read");
    assert_eq!(bytes, [0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17]);

    let (_fixture, short, calls) =
        memory_fixture(MockMode::ShortFirst(3), 1024, "1000-2000 rw-p 0 00:00 0\n");
    short.read_exact(0x1020, &mut bytes).expect("short retry");
    assert_eq!(bytes, [0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27]);
    assert_eq!(calls.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn singleton_batch_retries_and_respects_fallback_mapping_boundaries() {
    let maps = "1000-1004 rw-p 0 00:00 0\n1004-1100 rw-p 0 00:00 0\n";
    for (mode, expected_calls) in [(MockMode::ShortFirst(3), 2), (MockMode::ErrorOnCall(0), 3)] {
        let (_fixture, memory, calls) = memory_fixture(mode, 1, maps);
        let mut bytes = [0xa5; 8];
        memory
            .read_many(&mut [ReadRequest::new(0x1000, &mut bytes)])
            .expect("complete singleton read");
        assert_eq!(bytes, [0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(calls.calls.load(Ordering::SeqCst), expected_calls);
    }

    let (_fixture, memory, _) = memory_fixture(MockMode::ErrorOnCall(0), 1, maps);
    let mut bytes = [0xa5; 8];
    let error = memory
        .read_many(&mut [ReadRequest::new(0x10fc, &mut bytes)])
        .expect_err("singleton crosses the readable mapping boundary");
    assert!(matches!(
        error,
        MemoryError::Read {
            address: 0x1100,
            len: 4,
            ..
        }
    ));
    assert_eq!(bytes, [0; 8]);
}
#[test]
fn ordinary_reads_do_not_require_maps_after_opening() {
    let (fixture, memory, calls) =
        memory_fixture(MockMode::Exact, 1024, "1000-3000 rw-p 0 00:00 0\n");
    fs::remove_file(fixture.path().join("20/maps")).expect("remove maps");

    let mut exact = [0; 4];
    memory
        .read_exact(0x1010, &mut exact)
        .expect("direct exact read");
    let mut first = [0; 2];
    let mut second = [0; 2];
    let mut requests = [
        ReadRequest::new(0x2020, &mut first),
        ReadRequest::new(0x2030, &mut second),
    ];
    memory
        .read_many(&mut requests)
        .expect("direct batched read");

    assert_eq!(exact, [0x10, 0x11, 0x12, 0x13]);
    assert_eq!(first, [0x20, 0x21]);
    assert_eq!(second, [0x30, 0x31]);
    assert_eq!(calls.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn batch_short_prefix_resumes_at_boundaries_and_inside_destinations() {
    for prefix in [4, 6] {
        let (_fixture, memory, calls) = memory_fixture(
            MockMode::ShortFirst(prefix),
            1024,
            "1000-3000 rw-p 0 00:00 0\n",
        );
        let mut first = [0xa5; 4];
        let mut second = [0xa5; 4];
        let mut third = [0xa5; 4];
        memory
            .read_many(&mut [
                ReadRequest::new(0x1010, &mut first),
                ReadRequest::new(0x2020, &mut second),
                ReadRequest::new(0x1030, &mut third),
            ])
            .expect("complete remaining destinations after short prefix");
        assert_eq!(first, [0x10, 0x11, 0x12, 0x13]);
        assert_eq!(second, [0x20, 0x21, 0x22, 0x23]);
        assert_eq!(third, [0x30, 0x31, 0x32, 0x33]);
        assert_eq!(calls.calls.load(Ordering::SeqCst), 3);
    }
}

#[test]
fn direct_batch_at_stack_capacity_preserves_request_order() {
    let (_fixture, memory, calls) =
        memory_fixture(MockMode::Exact, 1024, "1000-3000 rw-p 0 00:00 0\n");
    let mut destinations = [[0; 1]; STACK_IOVEC_CAPACITY];
    let mut requests: Vec<_> = destinations
        .iter_mut()
        .enumerate()
        .map(|(index, dst)| ReadRequest::new(0x1000 + index, dst))
        .collect();

    memory
        .read_many(&mut requests)
        .expect("capacity-sized direct batch");

    for (index, destination) in destinations.iter().enumerate() {
        assert_eq!(*destination, [index as u8]);
    }
    assert_eq!(calls.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn direct_batch_skips_empty_entries() {
    let (_fixture, memory, calls) =
        memory_fixture(MockMode::Exact, 1024, "1000-3000 rw-p 0 00:00 0\n");
    let mut first = [0; 1];
    let mut empty = [];
    let mut second = [0; 1];
    let mut requests = [
        ReadRequest::new(0x1010, &mut first),
        ReadRequest::new(usize::MAX, &mut empty),
        ReadRequest::new(0x1020, &mut second),
    ];

    memory
        .read_many(&mut requests)
        .expect("direct batch with empty entry");

    assert_eq!(first, [0x10]);
    assert_eq!(second, [0x20]);
    assert_eq!(calls.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn direct_batch_above_stack_capacity_uses_exact_fallback_path() {
    let (_fixture, memory, calls) =
        memory_fixture(MockMode::Exact, 1024, "1000-3000 rw-p 0 00:00 0\n");
    let mut destinations = [[0; 1]; STACK_IOVEC_CAPACITY + 1];
    let mut requests: Vec<_> = destinations
        .iter_mut()
        .enumerate()
        .map(|(index, dst)| ReadRequest::new(0x1000 + index, dst))
        .collect();

    memory
        .read_many(&mut requests)
        .expect("above-capacity direct batch");

    for (index, destination) in destinations.iter().enumerate() {
        assert_eq!(*destination, [index as u8]);
    }
    assert_eq!(calls.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn scanner_region_read_uses_direct_native_io() {
    let (fixture, memory, calls) =
        memory_fixture(MockMode::Exact, 1024, "1000-2000 rw-p 0 00:00 0\n");
    assert_eq!(
        memory.readable_regions().expect("advertised regions"),
        [MemoryRegion {
            address: 0x1000,
            size: 0x1000,
        }]
    );
    fs::remove_file(fixture.path().join("20/maps")).expect("remove maps");
    let mut bytes = [0; 8];
    memory
        .read_scanner_region_exact(0x1010, &mut bytes)
        .expect("scanner read");
    assert_eq!(bytes, [0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17]);
    assert_eq!(calls.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn permission_errors_are_typed_and_failed_reads_zero_every_output() {
    for errno in [libc::EPERM, libc::EACCES] {
        let (_fixture, denied, calls) =
            memory_fixture(MockMode::Error(errno), 1024, "1000-3000 rw-p 0 00:00 0\n");
        let mut first = [7; 4];
        let mut second = [8; 4];
        let mut requests = [
            ReadRequest::new(0x1000, &mut first),
            ReadRequest::new(0x2000, &mut second),
        ];
        let error = denied
            .read_many(&mut requests)
            .expect_err("permission denial");

        assert!(matches!(
            &error,
            MemoryError::PermissionDenied {
                address: 0x1000,
                len: 8,
                ..
            }
        ));
        assert_eq!(first, [0; 4]);
        assert_eq!(second, [0; 4]);
        assert!(error.to_string().contains(PERMISSION_GUIDANCE));
        assert_eq!(calls.calls.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn address_overflow_skips_syscalls_and_zeroes_every_output() {
    let (_fixture, memory, calls) =
        memory_fixture(MockMode::Exact, 1024, "1000-3000 rw-p 0 00:00 0\n");
    let mut overflowing = [7; 2];
    let mut ordinary = [8; 4];
    let error = {
        let mut requests = [
            ReadRequest::new(usize::MAX, &mut overflowing),
            ReadRequest::new(0x1000, &mut ordinary),
        ];
        memory
            .read_many(&mut requests)
            .expect_err("address overflow")
    };

    assert_eq!(
        error,
        MemoryError::AddressOverflow {
            base: usize::MAX,
            offset: 2,
        }
    );
    assert_eq!(overflowing, [0; 2]);
    assert_eq!(ordinary, [0; 4]);
    overflowing = [7; 2];
    let exact_error = memory
        .read_exact(usize::MAX, &mut overflowing)
        .expect_err("single-read address overflow");
    assert_eq!(
        exact_error,
        MemoryError::AddressOverflow {
            base: usize::MAX,
            offset: 2,
        }
    );
    assert_eq!(overflowing, [0; 2]);
    assert_eq!(calls.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn ordinary_syscall_fault_remains_an_ordinary_read_error() {
    let (_fixture, fault, _) = memory_fixture(
        MockMode::Error(libc::EFAULT),
        1024,
        "1000-2000 rw-p 0 00:00 0\n",
    );
    let mut bytes = [7; 4];
    let error = fault
        .read_exact(0x1000, &mut bytes)
        .expect_err("read fault");

    assert!(matches!(error, MemoryError::Read { .. }));
    assert_eq!(bytes, [0; 4]);
}

#[test]
fn fallback_splits_at_iov_max_and_current_mapping_boundaries() {
    let (fixture, memory, calls) =
        memory_fixture(MockMode::ErrorOnCall(0), 2, "2000-3000 rw-p 0 00:00 0\n");
    fs::write(
        fixture.path().join("20/maps"),
        concat!("1000-1004 rw-p 0 00:00 0\n", "1004-1100 rw-p 0 00:00 0\n"),
    )
    .expect("replace maps before fallback");
    let mut crossing = [0; 8];
    let mut one = [0; 1];
    let mut two = [0; 1];
    let mut requests = [
        ReadRequest::new(0x1000, &mut crossing),
        ReadRequest::new(0x1010, &mut one),
        ReadRequest::new(0x1020, &mut two),
    ];
    memory
        .read_many(&mut requests)
        .expect("fallback split batch read");
    assert_eq!(crossing, [0, 1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(one, [0x10]);
    assert_eq!(two, [0x20]);
    assert_eq!(calls.calls.load(Ordering::SeqCst), 3);
}

#[test]
fn persistent_fallback_failure_zeroes_prior_direct_successes() {
    let (_fixture, memory, calls) =
        memory_fixture(MockMode::ErrorFromCall(1), 1, "1000-3000 rw-p 0 00:00 0\n");
    let mut first = [9; 4];
    let mut second = [9; 4];
    let mut requests = [
        ReadRequest::new(0x1000, &mut first),
        ReadRequest::new(0x2000, &mut second),
    ];
    memory
        .read_many(&mut requests)
        .expect_err("direct and fallback reads fail");
    assert_eq!(first, [0; 4]);
    assert_eq!(second, [0; 4]);
    assert_eq!(calls.calls.load(Ordering::SeqCst), 3);
}

#[test]
fn resolves_override_wine_config_command_line_and_appimage_root() {
    let fixture = fixture();
    let executable = add_process(
        fixture.path(),
        30,
        2,
        b"wine-preloader\0Z:\\\\games\\\\osu!\\\\osu!.exe\0",
        "1000-2000 rw-p 0 00:00 0\n",
    );
    fs::remove_file(fixture.path().join("30/exe")).expect("replace executable link");
    symlink("/usr/bin/wine-preloader", fixture.path().join("30/exe")).expect("wine exe link");

    let override_env = [(
        OsString::from("VOSU_OSU_PATH"),
        OsString::from("/override/osu"),
    )];
    assert_eq!(
        resolve_osu_path_at(fixture.path(), 30, &override_env).expect("override"),
        Path::new("/override/osu"),
    );

    let data = fixture.path().join("data");
    fs::create_dir_all(data.join("osuconfig")).expect("osuconfig");
    fs::write(data.join("osuconfig/osupath"), b" /configured/osu \n").expect("osupath");
    let config_env = [(OsString::from("XDG_DATA_HOME"), data.into_os_string())];
    assert_eq!(
        resolve_osu_path_at(fixture.path(), 30, &config_env).expect("wine config"),
        Path::new("/configured/osu"),
    );
    assert_eq!(
        resolve_osu_path_at(fixture.path(), 30, &[]).expect("wine command line"),
        Path::new("/games/osu!"),
    );

    assert_eq!(
        root_for(
            fixture.path(),
            30,
            Path::new("/tmp/.mount_osu/usr/bin/osu!")
        ),
        fixture.path().join("30/root/tmp/.mount_osu/usr/bin"),
    );
    assert!(executable.exists());
}

#[test]
fn reads_native_self_process_memory() {
    let value = Box::new(0x1234_5678_9abc_def0_u64);
    let memory = LinuxProcessMemory::new(std::process::id());
    let mut bytes = [0; 8];
    memory
        .read_exact((&*value as *const u64) as usize, &mut bytes)
        .expect("read current process");
    assert_eq!(u64::from_ne_bytes(bytes), *value);
}

#[test]
fn bounded_proc_reads_preserve_limit_with_initial_capacity() {
    let fixture = fixture();
    let path = fixture.path().join("maps");
    let expected = vec![0x5a; MAX_PROC_FILE as usize];
    fs::write(&path, &expected).unwrap();
    assert_eq!(read_bounded(&path, 4096).unwrap(), expected);

    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(MAX_PROC_FILE + 1)
        .unwrap();
    assert_eq!(
        read_bounded(&path, 4096).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}
