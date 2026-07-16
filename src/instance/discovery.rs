use std::collections::BTreeMap;
use std::ffi::OsString;
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
            for (index, process) in processes.iter().enumerate() {
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
    fn foreground_pid(&self) -> Option<Pid> {
        None
    }
    fn supports_focus(&self) -> bool {
        self.platform() == DiscoveryPlatform::Windows
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Default)]
pub struct NativeProcessProvider;

#[cfg(target_os = "linux")]
impl ProcessProvider for NativeProcessProvider {
    fn platform(&self) -> DiscoveryPlatform {
        DiscoveryPlatform::Linux
    }

    fn enumerate(&self) -> Result<Vec<ProcessDescriptor>, MemoryError> {
        Ok(crate::process::enumerate_processes()?
            .into_iter()
            .map(|process| {
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
            })
            .collect())
    }

    fn open(&self, pid: Pid) -> Result<ProcessHandle, MemoryError> {
        let bitness = crate::process::process_bitness(pid)?;
        let memory = crate::process::open_process(pid)?;
        Ok(ProcessHandle::new(Arc::new(memory), bitness))
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
