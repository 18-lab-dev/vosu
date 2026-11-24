#[cfg(all(target_os = "linux", feature = "linux-process-events"))]
use tokio::sync::Notify;

use std::collections::BTreeMap;
use std::ffi::OsString;
#[cfg(all(target_os = "linux", feature = "linux-process-events"))]
use std::io;
#[cfg(any(target_os = "linux", windows))]
use std::path::Path;
use std::path::PathBuf;
#[cfg(any(target_os = "linux", windows))]
use std::sync::Arc;
#[cfg(windows)]
use std::sync::{Mutex, OnceLock};

use vosu_osu_model::enums::ClientType;
use vosu_osu_reader::discovery::{
    StartupArgs, is_linux_candidate_name, is_linux_named_name, windows_client,
};
use vosu_osu_reader::process::ProcessHandle;
use vosu_sys::process::{MemoryError, ProcessBitness};

use super::Pid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryPlatform {
    Windows,
    Linux,
}

#[derive(Debug, Clone)]
pub struct ProcessDescriptor {
    pub pid: Pid,
    pub name: OsString,
    pub bitness: ProcessBitness,
    pub arguments: StartupArgs,
    pub root: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassifiedCandidate {
    pub index: usize,
    pub pid: Pid,
    pub client: ClientType,
}

/// Pure source-compatible candidate reduction. Linux performs a named first pass;
/// only an empty first pass enables the broad `osu!` lazer fallback.
#[must_use]
pub fn classify_candidates(
    platform: DiscoveryPlatform,
    processes: &[ProcessDescriptor],
) -> Vec<ClassifiedCandidate> {
    match platform {
        DiscoveryPlatform::Windows => processes
            .iter()
            .enumerate()
            .filter_map(|(index, process)| {
                let name = process.name.to_string_lossy();
                (name.eq_ignore_ascii_case("osu!.exe") || name.eq_ignore_ascii_case("osulazer.exe"))
                    .then_some(ClassifiedCandidate {
                        index,
                        pid: process.pid,
                        client: windows_client(process.bitness),
                    })
            })
            .collect(),
        DiscoveryPlatform::Linux => {
            let named: BTreeMap<_, _> = processes
                .iter()
                .enumerate()
                .filter(|(_, process)| is_linux_candidate_name(&process.name))
                .filter_map(|(index, process)| {
                    is_linux_named_name(&process.name).then_some((
                        process.pid,
                        ClassifiedCandidate {
                            index,
                            pid: process.pid,
                            client: ClientType::Stable,
                        },
                    ))
                })
                .collect();
            let fallback = named.is_empty();
            let mut candidates = named;
            for (index, process) in processes
                .iter()
                .enumerate()
                .filter(|(_, process)| is_linux_candidate_name(&process.name))
            {
                if process.name.to_string_lossy().contains("osu!") {
                    candidates
                        .entry(process.pid)
                        .or_insert(ClassifiedCandidate {
                            index,
                            pid: process.pid,
                            client: if fallback {
                                ClientType::Lazer
                            } else {
                                ClientType::Stable
                            },
                        });
                }
            }
            candidates.into_values().collect()
        }
    }
}

pub trait ProcessProvider: Send + Sync {
    fn platform(&self) -> DiscoveryPlatform;
    fn enumerate(&self) -> Result<Vec<ProcessDescriptor>, MemoryError>;
    fn open(&self, pid: Pid) -> Result<ProcessHandle, MemoryError>;
    /// Inspects one PID. Generic providers retain source compatibility through
    /// an enumeration-based default; native Linux overrides this without scanning.
    fn inspect(&self, pid: Pid) -> Result<Option<ProcessDescriptor>, MemoryError> {
        Ok(self
            .enumerate()?
            .into_iter()
            .find(|process| process.pid == pid))
    }
    fn foreground_pid(&self) -> Option<Pid> {
        None
    }
    fn supports_focus(&self) -> bool {
        self.platform() == DiscoveryPlatform::Windows
    }
    #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
    fn discovery_notifier(&self) -> Option<&Notify> {
        None
    }
    #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
    fn discovery_events_healthy(&self) -> bool {
        false
    }
    #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
    fn discovery_event_count(&self) -> u64 {
        0
    }
    #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
    fn discovery_reconciliation_request_count(&self) -> u64 {
        0
    }
    #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
    fn drain_discovery_events(&self) -> (Vec<Pid>, Vec<Pid>, bool) {
        (Vec::new(), Vec::new(), false)
    }
    #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
    fn replace_tracked_tgids(&self, _tgids: &[Pid]) {}
}

#[cfg(all(target_os = "linux", feature = "linux-process-events"))]
#[derive(Debug)]
pub struct NativeProcessProvider {
    events: Option<super::LinuxProcessEvents>,
}

#[cfg(all(target_os = "linux", feature = "linux-process-events"))]
fn default_comm_hints() -> super::CommHints {
    // Byte-identical to the former hard-coded `osu!`/`osulazer.exe` filter.
    // Wiring this to enabled plugins' `comm_hints` (plugin-abi.md Sec 6.1,
    // D18) lands in a later unit.
    super::CommHints::new(["osu!", "osulazer.exe"])
        .expect("hardcoded default comm hints satisfy CommHints::new's bounds")
}

#[cfg(all(target_os = "linux", feature = "linux-process-events"))]
impl NativeProcessProvider {
    pub fn open_process_events(mode: Option<super::ProcessEventMode>) -> io::Result<Self> {
        Ok(Self {
            events: mode
                .map(|mode| super::LinuxProcessEvents::open(mode, &default_comm_hints()))
                .transpose()?,
        })
    }

    /// Enumeration-only provider for short CLI actions that must not open eBPF/CN_PROC.
    #[must_use]
    pub fn polling_only() -> Self {
        Self { events: None }
    }
}

#[cfg(all(target_os = "linux", feature = "linux-process-events"))]
impl Default for NativeProcessProvider {
    fn default() -> Self {
        Self {
            events: super::LinuxProcessEvents::from_environment(&default_comm_hints()),
        }
    }
}

#[cfg(all(target_os = "linux", not(feature = "linux-process-events")))]
#[derive(Debug, Default)]
pub struct NativeProcessProvider;

#[cfg(target_os = "linux")]
fn linux_descriptor(process: vosu_sys::process::LinuxProcessMetadata) -> ProcessDescriptor {
    let root = vosu_osu_reader::discovery::resolve_osu_path(process.pid).unwrap_or_else(|_| {
        process
            .executable
            .parent()
            .unwrap_or(Path::new("/"))
            .to_path_buf()
    });
    ProcessDescriptor {
        pid: process.pid,
        name: process.name,
        bitness: process.bitness,
        arguments: StartupArgs::parse(process.command_line),
        root,
    }
}

#[cfg(target_os = "linux")]
impl ProcessProvider for NativeProcessProvider {
    fn platform(&self) -> DiscoveryPlatform {
        DiscoveryPlatform::Linux
    }

    fn enumerate(&self) -> Result<Vec<ProcessDescriptor>, MemoryError> {
        Ok(
            vosu_sys::process::enumerate_processes_matching(is_linux_candidate_name)?
                .into_iter()
                .map(linux_descriptor)
                .collect(),
        )
    }

    fn inspect(&self, pid: Pid) -> Result<Option<ProcessDescriptor>, MemoryError> {
        Ok(
            vosu_sys::process::process_metadata_matching(pid, is_linux_candidate_name)?
                .map(linux_descriptor),
        )
    }

    fn open(&self, pid: Pid) -> Result<ProcessHandle, MemoryError> {
        let bitness = vosu_sys::process::process_bitness(pid)?;
        let memory = vosu_sys::process::open_process(pid)?;
        Ok(ProcessHandle::new(Arc::new(memory), bitness))
    }

    #[cfg(feature = "linux-process-events")]
    fn discovery_notifier(&self) -> Option<&Notify> {
        self.events.as_ref().map(|events| events.notifier())
    }

    #[cfg(feature = "linux-process-events")]
    fn discovery_events_healthy(&self) -> bool {
        self.events
            .as_ref()
            .is_some_and(super::LinuxProcessEvents::healthy)
    }

    #[cfg(feature = "linux-process-events")]
    fn discovery_event_count(&self) -> u64 {
        self.events
            .as_ref()
            .map_or(0, super::LinuxProcessEvents::event_count)
    }

    #[cfg(feature = "linux-process-events")]
    fn discovery_reconciliation_request_count(&self) -> u64 {
        self.events
            .as_ref()
            .map_or(0, super::LinuxProcessEvents::reconciliation_request_count)
    }

    #[cfg(feature = "linux-process-events")]
    fn drain_discovery_events(&self) -> (Vec<Pid>, Vec<Pid>, bool) {
        let Some(events) = self.events.as_ref() else {
            return (Vec::new(), Vec::new(), false);
        };
        let batch = events.drain();
        (batch.inspect, batch.exit, batch.needs_reconcile)
    }

    #[cfg(feature = "linux-process-events")]
    fn replace_tracked_tgids(&self, tgids: &[Pid]) {
        if let Some(events) = self.events.as_ref() {
            events.replace_tracked(tgids.iter().copied());
        }
    }
}

#[cfg(windows)]
fn is_windows_candidate_name(name: &[u16]) -> bool {
    fn equals_ascii_case_insensitive(wide: &[u16], ascii: &[u8]) -> bool {
        wide.len() == ascii.len()
            && wide.iter().zip(ascii).all(|(&wide, &ascii)| {
                u8::try_from(wide).is_ok_and(|unit| unit.eq_ignore_ascii_case(&ascii))
            })
    }

    equals_ascii_case_insensitive(name, b"osu!.exe")
        || equals_ascii_case_insensitive(name, b"osulazer.exe")
}

#[cfg(windows)]
fn windows_instance_root(command_line: &str) -> PathBuf {
    if let Some(path) = std::env::var_os("VOSU_OSU_PATH").filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }
    let command_line = command_line.trim_start();
    let executable = if let Some(quoted) = command_line.strip_prefix('"') {
        quoted.split_once('"').map_or(quoted, |(path, _)| path)
    } else {
        command_line.split_whitespace().next().unwrap_or_default()
    };
    Path::new(executable)
        .parent()
        .unwrap_or(Path::new(""))
        .to_path_buf()
}

#[cfg(windows)]
#[derive(Debug, Clone)]
struct CachedWindowsMetadata {
    creation_time: u64,
    bitness: ProcessBitness,
    command_line: String,
}

#[cfg(windows)]
fn windows_metadata_cache() -> &'static Mutex<BTreeMap<Pid, CachedWindowsMetadata>> {
    static CACHE: OnceLock<Mutex<BTreeMap<Pid, CachedWindowsMetadata>>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

#[cfg(windows)]
#[derive(Debug, Default)]
pub struct NativeProcessProvider;

#[cfg(any(target_os = "linux", windows))]
impl NativeProcessProvider {
    pub fn new() -> Self {
        #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
        {
            Self::default()
        }
        #[cfg(any(
            windows,
            all(target_os = "linux", not(feature = "linux-process-events"))
        ))]
        {
            Self
        }
    }

    pub(crate) fn process_event_backend_name(&self) -> &'static str {
        #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
        {
            self.events
                .as_ref()
                .map_or("polling", super::LinuxProcessEvents::backend_name)
        }
        #[cfg(any(
            windows,
            all(target_os = "linux", not(feature = "linux-process-events"))
        ))]
        {
            "polling"
        }
    }
}

#[cfg(windows)]
impl ProcessProvider for NativeProcessProvider {
    fn platform(&self) -> DiscoveryPlatform {
        DiscoveryPlatform::Windows
    }

    fn enumerate(&self) -> Result<Vec<ProcessDescriptor>, MemoryError> {
        let processes =
            vosu_sys::process::windows::enumerate_processes_matching(is_windows_candidate_name)?;
        let mut metadata = windows_metadata_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        metadata.retain(|pid, _| processes.iter().any(|process| process.pid == *pid));

        let mut result = Vec::with_capacity(processes.len());
        for process in processes {
            let Ok(memory) = vosu_sys::process::WindowsProcessMemory::open(process.pid) else {
                continue;
            };
            let Ok(creation_time) = memory.creation_time() else {
                continue;
            };
            let cached = metadata
                .get(&process.pid)
                .filter(|cached| cached.creation_time == creation_time)
                .cloned();
            let process_metadata = if let Some(cached) = cached {
                cached
            } else {
                let Ok(bitness) = memory.bitness() else {
                    continue;
                };
                let Ok(command_line) = memory.command_line_for_bitness(bitness) else {
                    continue;
                };
                let cached = CachedWindowsMetadata {
                    creation_time,
                    bitness,
                    command_line,
                };
                metadata.insert(process.pid, cached.clone());
                cached
            };
            result.push(ProcessDescriptor {
                pid: process.pid,
                name: process.executable.into(),
                bitness: process_metadata.bitness,
                arguments: StartupArgs::parse_command_line(&process_metadata.command_line),
                root: windows_instance_root(&process_metadata.command_line),
            });
        }
        Ok(result)
    }

    fn open(&self, pid: Pid) -> Result<ProcessHandle, MemoryError> {
        let memory = vosu_sys::process::WindowsProcessMemory::open(pid)?;
        let bitness = memory.bitness()?;
        Ok(ProcessHandle::new(Arc::new(memory), bitness))
    }
    fn foreground_pid(&self) -> Option<Pid> {
        vosu_sys::process::foreground_process_id()
    }
}
