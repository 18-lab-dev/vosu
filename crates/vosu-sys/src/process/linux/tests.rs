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
    fs::write(
        dir.join("stat"),
        format!("{pid} (osu! tricky) S {} 41\n", "0 ".repeat(18)),
    )
    .expect("stat");
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    fs::write(
        dir.join("status"),
        format!("Uid:\t{uid}\t{uid}\t{uid}\t{uid}\n"),
    )
    .expect("status");
    executable
}

#[test]
fn discovery_rejects_a_different_real_uid_even_when_effective_uid_matches() {
    let fixture = fixture();
    add_process(fixture.path(), 42, 2, b"osu!\0", "");
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let status = fixture.path().join("42/status");
    fs::write(
        &status,
        format!("Uid:\t{}\t{uid}\t{uid}\t{uid}\n", uid.wrapping_add(1)),
    )
    .unwrap();
    assert!(
        process_metadata_matching_at(fixture.path(), 42, |_| true)
            .unwrap()
            .is_none()
    );
    assert!(enumerate_processes_at(fixture.path()).unwrap().is_empty());
    fs::write(&status, format!("Uid:\t{uid}\t{uid}\t{uid}\t{uid}\n")).unwrap();
    assert_eq!(
        process_metadata_matching_at(fixture.path(), 42, |_| true)
            .unwrap()
            .unwrap()
            .pid,
        42
    );
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

/// Stands in for a reused PID: it reports a complete read of foreign bytes after optionally
/// ending the retained process, so only the generation check can reject the result.
struct ForeignAnswer {
    /// Child PID to kill and reap inside the next read; zero for none.
    exit_during_read: std::sync::atomic::AtomicU32,
    on_read: Box<dyn Fn() + Send + Sync>,
    calls: AtomicUsize,
}

impl ForeignAnswer {
    fn new(on_read: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            exit_during_read: std::sync::atomic::AtomicU32::new(0),
            on_read: Box::new(on_read),
            calls: AtomicUsize::new(0),
        })
    }
}

impl ProcessVmReadv for ForeignAnswer {
    fn readv(
        &self,
        _pid: libc::pid_t,
        local: &mut [libc::iovec],
        _remote: &[libc::iovec],
    ) -> Result<usize, i32> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let child = self.exit_during_read.swap(0, Ordering::SeqCst) as libc::pid_t;
        if child != 0 {
            // SAFETY: kill and waitpid take plain integers; the test owns this unreaped child.
            unsafe {
                assert_eq!(libc::kill(child, libc::SIGKILL), 0);
                assert_eq!(libc::waitpid(child, std::ptr::null_mut(), 0), child);
            }
        }
        (self.on_read)();
        let mut total = 0;
        for iov in local.iter_mut() {
            // SAFETY: the backend passes borrowed destination buffers for this call.
            unsafe { std::slice::from_raw_parts_mut(iov.iov_base.cast::<u8>(), iov.iov_len) }
                .fill(0xaa);
            total += iov.iov_len;
        }
        Ok(total)
    }

    fn iov_max(&self) -> usize {
        1024
    }
}

#[cfg(feature = "linux-process-events")]
#[test]
fn pidfd_rejects_reads_answered_after_the_retained_process_exits() {
    let spawn = || {
        std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep")
    };
    let open = |child: &std::process::Child, syscall: Arc<ForeignAnswer>| {
        let pidfd = open_pidfd(child.id()).map(Arc::new);
        assert!(pidfd.is_some(), "pidfd_open must work on this Linux");
        LinuxProcessMemory::with_parts_and_pidfd(child.id(), PathBuf::from("/proc"), syscall, pidfd)
    };

    // Exit and reap before the read: the freed PID may already name another process.
    let mut exited = spawn();
    let syscall = ForeignAnswer::new(|| {});
    let memory = open(&exited, syscall.clone());
    exited.kill().expect("kill sleep");
    exited.wait().expect("reap sleep");
    let mut single = [9u8; 8];
    assert!(memory.read_exact(0x1000, &mut single).is_err());
    assert_eq!(single, [0; 8]);
    let (mut first, mut second) = ([9u8; 4], [9u8; 4]);
    assert!(
        memory
            .read_many(&mut [
                ReadRequest::new(0x1000, &mut first),
                ReadRequest::new(0x2000, &mut second),
            ])
            .is_err()
    );
    assert_eq!((first, second), ([0; 4], [0; 4]));
    assert!(syscall.calls.load(Ordering::SeqCst) >= 2);

    // Exit and reap while the read is in flight.
    let during = spawn();
    let syscall = ForeignAnswer::new(|| {});
    let memory = open(&during, syscall.clone());
    syscall
        .exit_during_read
        .store(during.id(), Ordering::SeqCst);
    let mut bytes = [9u8; 8];
    assert!(memory.read_exact(0x1000, &mut bytes).is_err());
    assert_eq!(bytes, [0; 8]);
    assert_eq!(syscall.exit_during_read.load(Ordering::SeqCst), 0);
    // The reaped child needs no further wait; dropping `Child` neither kills nor waits.
    drop(during);
}

#[test]
fn start_time_rejects_pid_reused_during_read() {
    let (fixture, _, _) = memory_fixture(MockMode::Exact, 1024, "1000-2000 rw-p 0 00:00 0\n");
    let stat = fixture.path().join("20/stat");
    let reused = format!("20 (osu!) S {} 42\n", "0 ".repeat(18));
    let syscall = ForeignAnswer::new(move || {
        fs::write(&stat, &reused).expect("reused stat");
    });
    let memory = LinuxProcessMemory::with_parts(20, fixture.path().to_path_buf(), syscall.clone());
    assert!(memory.pidfd.is_none());
    assert!(memory.is_alive());
    let mut bytes = [9u8; 8];
    let error = memory
        .read_exact(0x1000, &mut bytes)
        .expect_err("bytes answered by a reused PID");
    assert!(matches!(error, MemoryError::Read { .. }), "{error:?}");
    assert_eq!(bytes, [0; 8]);
    assert!(syscall.calls.load(Ordering::SeqCst) >= 1);
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

/// Records each remote range and answers with address-derived bytes.
struct RangeRecorder {
    ranges: parking_lot::Mutex<Vec<(usize, usize)>>,
}

impl ProcessVmReadv for RangeRecorder {
    fn readv(
        &self,
        _pid: libc::pid_t,
        local: &mut [libc::iovec],
        remote: &[libc::iovec],
    ) -> Result<usize, i32> {
        assert_eq!(local.len(), remote.len());
        let mut total = 0;
        for (local, remote) in local.iter_mut().zip(remote) {
            assert_eq!(local.iov_len, remote.iov_len);
            let start = remote.iov_base as usize;
            self.ranges.lock().push((start, remote.iov_len));
            // SAFETY: the backend passes borrowed destination buffers for this call.
            let dst = unsafe {
                std::slice::from_raw_parts_mut(local.iov_base.cast::<u8>(), local.iov_len)
            };
            for (index, byte) in dst.iter_mut().enumerate() {
                *byte = (start + index) as u8;
            }
            total += remote.iov_len;
        }
        Ok(total)
    }

    fn iov_max(&self) -> usize {
        1024
    }
}

#[test]
fn batched_reads_deliver_each_destination_and_touch_only_requested_pages() {
    const PAGE: usize = 4096;
    let fixture = fixture();
    add_process(fixture.path(), 20, 2, b"osu!\0", "");
    let syscall = Arc::new(RangeRecorder {
        ranges: parking_lot::Mutex::new(Vec::new()),
    });
    let memory = LinuxProcessMemory::with_parts(20, fixture.path().to_path_buf(), syscall.clone());
    // Same-page neighbors, neighbors across a page boundary, a page-crossing request, an
    // overlapping duplicate, and ranges whose gaps span pages no request touches.
    let requested = [
        (0x1ff8, 8),
        (0x2010, 4),
        (0x1000, 4),
        (0x9000, 2),
        (0x2010, 4),
        (0x2ffe, 4),
        (0x3100, 16),
        (0x1004, 1),
        (0x7ff0, 0x20),
    ];
    let mut buffers: Vec<Vec<u8>> = requested.iter().map(|(_, len)| vec![0xee; *len]).collect();
    let mut requests: Vec<_> = requested
        .iter()
        .zip(&mut buffers)
        .map(|((address, _), dst)| ReadRequest::new(*address, dst))
        .collect();
    memory.read_many(&mut requests).expect("batched read");
    for ((address, _), bytes) in requested.iter().zip(&buffers) {
        let expected: Vec<u8> = (0..bytes.len())
            .map(|index| (address + index) as u8)
            .collect();
        assert_eq!(bytes, &expected, "destination at {address:#x}");
    }
    let touched = |start: usize, len: usize| start / PAGE..=(start + len - 1) / PAGE;
    for (start, len) in syscall.ranges.lock().iter().copied() {
        for page in touched(start, len) {
            assert!(
                requested
                    .iter()
                    .any(|(address, len)| touched(*address, *len).contains(&page)),
                "remote range {start:#x}+{len:#x} touches unrequested page {page:#x}"
            );
        }
    }
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
fn sparse_heap_batch_preserves_bytes_and_empty_destinations() {
    let (_fixture, memory, _calls) = memory_fixture(
        MockMode::ShortFirst(STACK_IOVEC_CAPACITY * 8),
        1024,
        "1000-3000 rw-p 0 00:00 0\n",
    );
    // Cross the stack limit despite an empty prefix and gaps between reads.
    let active = |index: usize| index >= 128 && (index - 128).is_multiple_of(97);
    let mut destinations = vec![[0xa5; 8]; 128 + (STACK_IOVEC_CAPACITY + 2) * 97];
    let mut requests: Vec<_> = destinations
        .iter_mut()
        .enumerate()
        .map(|(index, dst)| {
            if active(index) {
                ReadRequest::new(0x1000 + index, dst)
            } else {
                ReadRequest::new(usize::MAX, &mut dst[..0])
            }
        })
        .collect();

    memory
        .read_many(&mut requests)
        .expect("sparse read after short prefix");
    drop(requests);

    for (index, dst) in destinations.iter().enumerate() {
        let expected = if active(index) {
            std::array::from_fn(|offset| (index + offset) as u8)
        } else {
            [0xa5; 8]
        };
        assert_eq!(*dst, expected, "destination {index}");
    }
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
fn fallback_preserves_first_matching_map_for_unordered_and_overlapping_regions() {
    for (maps, expected_calls) in [
        ("1000-1004 rw-p 0 00:00 0\n1004-1100 rw-p 0 00:00 0\n", 5),
        ("1004-1100 rw-p 0 00:00 0\n1000-1004 rw-p 0 00:00 0\n", 5),
        ("1000-1004 rw-p 0 00:00 0\n1000-1100 rw-p 0 00:00 0\n", 5),
        ("1000-1100 rw-p 0 00:00 0\n1000-1004 rw-p 0 00:00 0\n", 3),
    ] {
        let (_fixture, memory, calls) = memory_fixture(MockMode::ErrorOnCall(0), 1, maps);
        let mut first = [0xa5; 8];
        let mut repeated = [0xa5; 8];
        memory
            .read_many(&mut [
                ReadRequest::new(0x1000, &mut first),
                ReadRequest::new(0x1000, &mut repeated),
            ])
            .expect("first-match mapping boundaries remain readable");
        assert_eq!(first, [0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(repeated, first);
        // One forced direct failure, then one syscall per mapping-sized chunk.
        // Reusing the later overlapping map would incorrectly remove a boundary.
        assert_eq!(calls.calls.load(Ordering::SeqCst), expected_calls);
    }
}

#[test]
fn fallback_rejects_holes_and_exclusive_ends_before_reading_any_chunks() {
    for maps in [
        "1000-1004 rw-p 0 00:00 0\n1008-1010 rw-p 0 00:00 0\n",
        "1008-1010 rw-p 0 00:00 0\n1000-1004 rw-p 0 00:00 0\n",
    ] {
        for (address, len, missing, remaining) in [
            (0x0fff, 1, 0x0fff, 1),
            (0x1000, 8, 0x1004, 4),
            (0x1010, 1, 0x1010, 1),
        ] {
            let (_fixture, memory, calls) = memory_fixture(MockMode::ErrorOnCall(0), 1, maps);
            let mut crossing = [0xa5; 8];
            let mut later = [0xbb; 1];
            let error = memory
                .read_many(&mut [
                    ReadRequest::new(address, &mut crossing[..len]),
                    ReadRequest::new(0x1008, &mut later),
                ])
                .expect_err("outside a readable mapping");
            assert!(matches!(
                error,
                MemoryError::Read { address, len, .. } if address == missing && len == remaining
            ));
            assert!(crossing[..len].iter().all(|&byte| byte == 0));
            assert!(crossing[len..].iter().all(|&byte| byte == 0xa5));
            assert_eq!(later, [0]);
            assert_eq!(calls.calls.load(Ordering::SeqCst), 1);
        }
    }
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
fn process_start_time_binds_identity_after_pid_reuse() {
    let fixture = fixture();
    let proc_root = fixture.path();
    let process_dir = proc_root.join("123");
    fs::create_dir(&process_dir).unwrap();
    let stat = |start: u64| format!("123 (odd ) comm) S {} {start}\n", vec!["1"; 18].join(" "));
    fs::write(process_dir.join("stat"), stat(41)).unwrap();
    assert_eq!(process_start_time_at(proc_root, 123), Some(41));
    let syscall = MockSyscall::new(MockMode::Exact, 1024);
    let memory = LinuxProcessMemory::with_parts(123, proc_root.to_owned(), syscall);
    fs::write(process_dir.join("stat"), stat(42)).unwrap();
    assert_ne!(process_start_time_at(proc_root, 123), Some(41));
    let mut output = [9u8; 8];
    assert!(memory.read_exact(0x1000, &mut output).is_err());
    assert_eq!(output, [0; 8]);
    assert!(!memory.is_alive());
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

#[test]
fn bounded_proc_reads_limit_streams_with_zero_reported_length() {
    use std::io::Write;

    let fixture = fixture();
    let path = fixture.path().join("maps-stream");
    let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: the CString is live and NUL-terminated; the FIFO is test-owned.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    assert_eq!(fs::metadata(&path).unwrap().len(), 0);
    let writer_path = path.clone();
    let writer = std::thread::spawn(move || {
        fs::OpenOptions::new()
            .write(true)
            .open(writer_path)
            .unwrap()
            .write_all(&vec![0x5a; MAX_PROC_FILE as usize + 1])
            .unwrap();
    });

    let error = read_bounded(&path, 4096).unwrap_err();
    writer.join().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn cloned_readers_observe_growing_and_shrinking_maps_and_reject_oversized_snapshots() {
    let (_fixture, memory, _) = memory_fixture(MockMode::Exact, 1024, "1000-2000 rw-p 0 00:00 0\n");
    assert_eq!(
        memory.readable_regions().unwrap(),
        [MemoryRegion {
            address: 0x1000,
            size: 0x1000
        }]
    );
    let cloned = memory.clone();
    let path = memory.proc_root.join(memory.pid.to_string()).join("maps");
    let expected: Vec<_> = (0..256)
        .map(|index| MemoryRegion {
            address: 0x1000 + index * 0x2000,
            size: 0x1000,
        })
        .collect();
    let maps: String = expected
        .iter()
        .map(|region| {
            format!(
                "{:x}-{:x} rw-p 0 00:00 0\n",
                region.address,
                region.address + region.size
            )
        })
        .collect();
    fs::write(&path, maps).unwrap();
    assert_eq!(cloned.readable_regions().unwrap(), expected);

    fs::write(&path, "3000-4000 rw-p 0 00:00 0\n").unwrap();
    for reader in [&memory, &cloned] {
        assert_eq!(
            reader.readable_regions().unwrap(),
            [MemoryRegion {
                address: 0x3000,
                size: 0x1000
            }]
        );
    }
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(MAX_PROC_FILE + 1)
        .unwrap();
    assert!(matches!(
        cloned.readable_regions(),
        Err(MemoryError::Backend {
            operation: "read /proc maps",
            ..
        })
    ));
}
