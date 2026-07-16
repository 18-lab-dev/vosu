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
fn eperm_has_exact_privilege_guidance_and_failure_zeroes_every_output() {
    let (_fixture, denied, _) = memory_fixture(
        MockMode::Error(libc::EPERM),
        1024,
        "1000-3000 rw-p 0 00:00 0\n",
    );
    let mut first = [7; 4];
    let mut second = [8; 4];
    let mut requests = [
        ReadRequest::new(0x1000, &mut first),
        ReadRequest::new(0x2000, &mut second),
    ];
    let error = denied.read_many(&mut requests).expect_err("EPERM");
    assert_eq!(first, [0; 4]);
    assert_eq!(second, [0; 4]);
    assert!(error.to_string().contains(EPERM_GUIDANCE));
}

#[test]
fn read_many_splits_at_iov_max_and_mapping_boundaries() {
    let (_fixture, memory, calls) = memory_fixture(
        MockMode::Exact,
        2,
        concat!("1000-1004 rw-p 0 00:00 0\n", "1004-1100 rw-p 0 00:00 0\n"),
    );
    let mut crossing = [0; 8];
    let mut one = [0; 1];
    let mut two = [0; 1];
    let mut requests = [
        ReadRequest::new(0x1000, &mut crossing),
        ReadRequest::new(0x1010, &mut one),
        ReadRequest::new(0x1020, &mut two),
    ];
    memory.read_many(&mut requests).expect("split batch read");
    assert_eq!(crossing, [0, 1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(calls.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn failed_suffix_retry_zeroes_prior_successes() {
    let (_fixture, memory, _) =
        memory_fixture(MockMode::ErrorOnCall(1), 1, "1000-3000 rw-p 0 00:00 0\n");
    let mut first = [9; 4];
    let mut second = [9; 4];
    let mut requests = [
        ReadRequest::new(0x1000, &mut first),
        ReadRequest::new(0x2000, &mut second),
    ];
    memory
        .read_many(&mut requests)
        .expect_err("second batch fails");
    assert_eq!(first, [0; 4]);
    assert_eq!(second, [0; 4]);
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
