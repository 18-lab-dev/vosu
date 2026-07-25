#[cfg(all(target_os = "linux", feature = "linux-process-events"))]
use tokio::sync::Notify;

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
#[cfg(all(target_os = "linux", feature = "linux-process-events"))]
use std::io;
#[cfg(any(target_os = "linux", windows))]
use std::path::Path;
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::sync::Arc;

use crate::app::StartupArgs;
use crate::model::enums::ClientType;
use crate::process::{MemoryError, ProcessBitness, ProcessHandle};

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

fn is_linux_candidate_name(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name.contains("osu!") || name.contains("osulazer.exe")
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
                        client: if process.bitness == ProcessBitness::X64 {
                            ClientType::Lazer
                        } else {
                            ClientType::Stable
                        },
                    })
            })
            .collect(),
        DiscoveryPlatform::Linux => {
            let named: BTreeMap<_, _> = processes
                .iter()
                .enumerate()
                .filter(|(_, process)| is_linux_candidate_name(&process.name))
                .filter_map(|(index, process)| {
                    let name = process.name.to_string_lossy();
                    (name.contains("osu!.exe") || name.contains("osulazer.exe")).then_some((
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

#[must_use]
pub fn should_skip(arguments: &StartupArgs) -> bool {
    arguments.get("tournament").is_some()
        || arguments
            .get("debug-client-id")
            .is_some_and(|value| value.is_truthy())
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
    events: Option<super::events::LinuxProcessEvents>,
}

#[cfg(all(target_os = "linux", feature = "linux-process-events"))]
impl NativeProcessProvider {
    pub fn open_process_events(mode: Option<super::events::ProcessEventMode>) -> io::Result<Self> {
        Ok(Self {
            events: mode
                .map(super::events::LinuxProcessEvents::open)
                .transpose()?,
        })
    }
}

#[cfg(all(target_os = "linux", feature = "linux-process-events"))]
impl Default for NativeProcessProvider {
    fn default() -> Self {
        Self {
            events: super::events::LinuxProcessEvents::from_environment(),
        }
    }
}

#[cfg(all(target_os = "linux", not(feature = "linux-process-events")))]
#[derive(Debug, Default)]
pub struct NativeProcessProvider;

#[cfg(target_os = "linux")]
fn linux_descriptor(process: crate::process::LinuxProcessMetadata) -> ProcessDescriptor {
    let root = crate::process::resolve_osu_path(process.pid).unwrap_or_else(|_| {
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
            crate::process::enumerate_processes_matching(is_linux_candidate_name)?
                .into_iter()
                .map(linux_descriptor)
                .collect(),
        )
    }

    fn inspect(&self, pid: Pid) -> Result<Option<ProcessDescriptor>, MemoryError> {
        Ok(
            crate::process::process_metadata_matching(pid, is_linux_candidate_name)?
                .map(linux_descriptor),
        )
    }

    fn open(&self, pid: Pid) -> Result<ProcessHandle, MemoryError> {
        let bitness = crate::process::process_bitness(pid)?;
        let memory = crate::process::open_process(pid)?;
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
            .is_some_and(super::events::LinuxProcessEvents::healthy)
    }

    #[cfg(feature = "linux-process-events")]
    fn discovery_event_count(&self) -> u64 {
        self.events
            .as_ref()
            .map_or(0, super::events::LinuxProcessEvents::event_count)
    }

    #[cfg(feature = "linux-process-events")]
    fn discovery_reconciliation_request_count(&self) -> u64 {
        self.events.as_ref().map_or(
            0,
            super::events::LinuxProcessEvents::reconciliation_request_count,
        )
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
}

#[cfg(windows)]
impl ProcessProvider for NativeProcessProvider {
    fn platform(&self) -> DiscoveryPlatform {
        DiscoveryPlatform::Windows
    }

    fn enumerate(&self) -> Result<Vec<ProcessDescriptor>, MemoryError> {
        let mut result = Vec::new();
        for process in crate::process::enumerate_processes()? {
            if !(process.executable.eq_ignore_ascii_case("osu!.exe")
                || process.executable.eq_ignore_ascii_case("osulazer.exe"))
            {
                continue;
            }
            let Ok(bitness) = crate::process::process_bitness(process.pid) else {
                continue;
            };
            let Ok(command_line) = crate::process::process_command_line(process.pid) else {
                continue;
            };
            result.push(ProcessDescriptor {
                pid: process.pid,
                name: process.executable.into(),
                bitness,
                arguments: StartupArgs::parse_command_line(&command_line),
                root: windows_instance_root(&command_line),
            });
        }
        Ok(result)
    }

    fn open(&self, pid: Pid) -> Result<ProcessHandle, MemoryError> {
        crate::process::open_process(pid)
    }
    fn foreground_pid(&self) -> Option<Pid> {
        crate::process::foreground_process_id()
    }
}
