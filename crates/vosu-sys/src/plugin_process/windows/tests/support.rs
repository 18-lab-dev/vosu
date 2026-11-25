//! Shared helpers for the native Windows launch tests: the core-death
//! process tree, readiness barriers, markers, process identity and
//! handle-table inspection.

use super::*;

/// test → core (in its own job) → probe (in a nested job) → descendant.
pub(super) struct CoreTree {
    pub(super) core: PluginHostProcess,
    pub(super) probe: OwnedHandle,
    pub(super) descendant: OwnedHandle,
    pub(super) probe_marker: HashMap<String, String>,
    _dir: tempfile::TempDir,
    _rings: [RingObjects; 2],
}

impl CoreTree {
    pub(super) fn launch(mode: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let down = RingObjects::create(LEN).unwrap();
        let up = RingObjects::create(LEN).unwrap();
        let core = spawn_plugin_host(
            &std::env::current_exe().unwrap(),
            &os_args(&helper_args(&["core", dir.path().to_str().unwrap(), mode])),
            &down,
            &up,
        )
        .unwrap();
        wait_ready(&up, core.process(), dir.path());
        let probe_pid = read_marker(dir.path(), "core")["probe_pid"].clone();
        let probe_marker = read_marker(dir.path(), &format!("probe-{probe_pid}"));
        let probe = open_verified(&probe_marker["pid"], &probe_marker["created"]);
        let descendant = open_verified(
            &probe_marker["descendant_pid"],
            &probe_marker["descendant_created"],
        );
        Self {
            core,
            probe,
            descendant,
            probe_marker,
            _dir: dir,
            _rings: [down, up],
        }
    }

    /// Terminates only the core process: not its job, not the tree.
    pub(super) fn kill_core_only(&self) {
        // SAFETY: the live core process handle with terminate access.
        let ok = unsafe { TerminateProcess(self.core.process().as_raw_handle() as HANDLE, 9) };
        assert_ne!(ok, 0, "{}", io::Error::last_os_error());
        assert_eq!(self.core.wait(Some(BOUND)).unwrap(), Some(9));
    }

    /// Neither the probe nor its descendant ends within the window. The
    /// test's own job for the core (dropped with `self`) cleans them up.
    pub(super) fn assert_survives(&self) {
        let handles = [
            self.probe.as_raw_handle() as HANDLE,
            self.descendant.as_raw_handle() as HANDLE,
        ];
        // SAFETY: two live process handles with SYNCHRONIZE access.
        let result = unsafe {
            WaitForMultipleObjects(2, handles.as_ptr(), 0, SURVIVAL_WINDOW.as_millis() as u32)
        };
        assert_eq!(result, WAIT_TIMEOUT, "a negative-control process exited");
    }
}

/// Waits for `objects`' doorbell or for `process` to exit, whichever is
/// first; a helper exiting before it is ready fails the test at once. A
/// failure lists the markers in `dir` so far.
pub(super) fn wait_ready(objects: &RingObjects, process: BorrowedHandle<'_>, dir: &Path) {
    let handles = [
        objects.doorbell_handle().as_raw_handle() as HANDLE,
        process.as_raw_handle() as HANDLE,
    ];
    // SAFETY: an event and a process handle, both live and waitable.
    let result =
        unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, BOUND.as_millis() as u32) };
    if result == WAIT_OBJECT_0 {
        return;
    }
    let markers: Vec<_> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .map(|entry| entry.map(|entry| entry.file_name()))
                .collect()
        })
        .unwrap_or_default();
    match result {
        WAIT_TIMEOUT => panic!("helper not ready within {BOUND:?}; markers {markers:?}"),
        WAIT_FAILED => panic!("{}", io::Error::last_os_error()),
        _ => panic!(
            "helper exited before it was ready: {:?}; markers {markers:?}",
            wait_exit(process, Some(Duration::ZERO))
        ),
    }
}

pub(super) fn helper_args(role: &[&str]) -> Vec<String> {
    let mut args: Vec<String> = [
        "--exact",
        HELPER_TEST,
        "--nocapture",
        "--test-threads=1",
        ROLE,
    ]
    .map(str::to_owned)
    .into();
    args.extend(role.iter().map(|&arg| arg.to_owned()));
    args
}

pub(super) fn os_args(args: &[String]) -> Vec<&OsStr> {
    args.iter().map(OsStr::new).collect()
}

/// The endpoint argument the probe received (its last one).
pub(super) fn endpoint_values(args: &str) -> &str {
    args.rsplit('\u{1f}').next().unwrap()
}

pub(super) fn join(values: &[u32]) -> String {
    values
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

pub(super) fn write_marker(dir: &Path, name: &str, fields: &[(String, String)]) {
    let text: String = fields
        .iter()
        .map(|(key, value)| format!("{key}={value}\n"))
        .collect();
    let temp = dir.join(format!("{name}.tmp"));
    std::fs::write(&temp, text).unwrap();
    std::fs::rename(temp, marker_path(dir, name)).unwrap();
}

pub(super) fn read_marker(dir: &Path, name: &str) -> HashMap<String, String> {
    std::fs::read_to_string(marker_path(dir, name))
        .unwrap()
        .lines()
        .map(|line| {
            let (key, value) = line.split_once('=').unwrap();
            (key.to_owned(), value.to_owned())
        })
        .collect()
}

fn marker_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.marker"))
}

pub(super) fn current() -> BorrowedHandle<'static> {
    // SAFETY: the current-process pseudo handle is always valid.
    unsafe { BorrowedHandle::borrow_raw(GetCurrentProcess() as _) }
}

pub(super) fn creation_time(process: BorrowedHandle<'_>) -> u64 {
    let mut times = [FILETIME::default(); 4];
    let [created, exited, kernel, user] = &mut times;
    // SAFETY: a live process handle with query access; four writable
    // FILETIMEs.
    let ok = unsafe {
        GetProcessTimes(
            process.as_raw_handle() as HANDLE,
            created,
            exited,
            kernel,
            user,
        )
    };
    assert_ne!(ok, 0, "{}", io::Error::last_os_error());
    u64::from(times[0].dwHighDateTime) << 32 | u64::from(times[0].dwLowDateTime)
}

/// Opens a process a helper reported while it is known alive, and checks
/// it is the same process (creation time), not a reused PID.
pub(super) fn open_verified(pid: &str, created: &str) -> OwnedHandle {
    let pid: u32 = pid.parse().unwrap();
    // SAFETY: opens a process by ID with wait and limited query access.
    let raw = unsafe { OpenProcess(SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    assert!(!raw.is_null(), "{}", io::Error::last_os_error());
    // SAFETY: a fresh handle this function now owns.
    let process = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    assert_eq!(
        creation_time(process.as_handle()).to_string(),
        created,
        "process {pid} is the reported process"
    );
    assert_eq!(
        wait_exit(process.as_handle(), Some(Duration::ZERO)).unwrap(),
        None
    );
    process
}

pub(super) fn inheritable_event() -> OwnedHandle {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };
    // SAFETY: an unnamed manual-reset event with inheritable security.
    let raw = unsafe { CreateEventW(&attributes, 1, 0, std::ptr::null()) };
    assert!(!raw.is_null());
    // SAFETY: a fresh handle this function now owns.
    unsafe { OwnedHandle::from_raw_handle(raw.cast()) }
}

pub(super) fn duplicate(
    source: BorrowedHandle<'_>,
    access: u32,
    inherit: bool,
    options: u32,
) -> io::Result<OwnedHandle> {
    duplicate_from(
        current(),
        source.as_raw_handle() as HANDLE,
        access,
        inherit,
        options,
    )
}

fn duplicate_from(
    process: BorrowedHandle<'_>,
    source: HANDLE,
    access: u32,
    inherit: bool,
    options: u32,
) -> io::Result<OwnedHandle> {
    let mut target: HANDLE = std::ptr::null_mut();
    // SAFETY: duplicates `source` (valid in `process`) into this process.
    let ok = unsafe {
        DuplicateHandle(
            process.as_raw_handle() as HANDLE,
            source,
            GetCurrentProcess(),
            &mut target,
            access,
            i32::from(inherit),
            options,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a fresh handle this function now owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(target.cast()) })
}

/// One handle in another process's table, duplicated here for comparison.
#[derive(Debug)]
pub(super) struct ChildHandle {
    pub(super) value: usize,
    pub(super) access: u32,
    copy: Option<OwnedHandle>,
}

impl ChildHandle {
    pub(super) fn same_object(&self, other: BorrowedHandle<'_>) -> bool {
        self.copy.as_ref().is_some_and(|copy| {
            // SAFETY: two live handles of this process.
            unsafe {
                CompareObjectHandles(
                    copy.as_raw_handle() as HANDLE,
                    other.as_raw_handle() as HANDLE,
                ) != 0
            }
        })
    }
}

/// `PROCESS_HANDLE_TABLE_ENTRY_INFO`.
#[repr(C)]
#[derive(Clone, Copy)]
struct TableEntry {
    value: HANDLE,
    _handle_count: usize,
    _pointer_count: usize,
    access: u32,
    _type_index: u32,
    _attributes: u32,
    _reserved: u32,
}

/// Every handle in `process`'s table (`ProcessHandleInformation`).
pub(super) fn child_handles(process: BorrowedHandle<'_>) -> io::Result<Vec<ChildHandle>> {
    entries(process)?
        .into_iter()
        .map(|entry| {
            Ok(ChildHandle {
                value: entry.value as usize,
                access: entry.access,
                copy: duplicate_from(process, entry.value, 0, false, DUPLICATE_SAME_ACCESS).ok(),
            })
        })
        .collect()
}

fn entries(process: BorrowedHandle<'_>) -> io::Result<Vec<TableEntry>> {
    const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
    let header = 2 * size_of::<usize>();
    let mut words = 4096;
    loop {
        let mut buffer = vec![0_u64; words];
        let mut needed = 0;
        // SAFETY: a live process handle with query access and a writable,
        // 8-byte aligned buffer of the stated length.
        let status = unsafe {
            NtQueryInformationProcess(
                process.as_raw_handle() as HANDLE,
                ProcessHandleInformation,
                buffer.as_mut_ptr().cast(),
                (words * 8) as u32,
                &mut needed,
            )
        };
        if status == STATUS_INFO_LENGTH_MISMATCH {
            words = (needed as usize).div_ceil(8) + 512;
            continue;
        }
        if status < 0 {
            return Err(io::Error::other(format!(
                "NtQueryInformationProcess failed: {status:#x}"
            )));
        }
        let base = buffer.as_ptr().cast::<u8>();
        // SAFETY: the snapshot starts with its handle count.
        let count = unsafe { base.cast::<usize>().read() };
        assert!(header + count * size_of::<TableEntry>() <= words * 8);
        return Ok((0..count)
            .map(|index| {
                // SAFETY: entry `index` lies inside the filled buffer
                // (asserted above); read unaligned-safe by value.
                unsafe {
                    base.add(header + index * size_of::<TableEntry>())
                        .cast::<TableEntry>()
                        .read_unaligned()
                }
            })
            .collect());
    }
}

/// The parent holds exactly one handle to each ring object: the original.
/// Every inheritable duplicate a spawn made is closed.
pub(super) fn assert_only_originals(to_plugin: &RingObjects, to_host: &RingObjects) {
    let own = entries(current()).unwrap();
    for original in [
        to_plugin.mapping_handle(),
        to_plugin.doorbell_handle(),
        to_host.mapping_handle(),
        to_host.doorbell_handle(),
    ] {
        let holders = own
            .iter()
            .filter(|entry| {
                // SAFETY: compares a value from this process's own table
                // (possibly closed since, which only yields "different")
                // with a live handle.
                unsafe {
                    CompareObjectHandles(entry.value, original.as_raw_handle() as HANDLE) != 0
                }
            })
            .count();
        assert_eq!(holders, 1, "a duplicate of a ring object stayed open");
    }
}
