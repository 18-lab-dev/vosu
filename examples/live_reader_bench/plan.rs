//! Read-only bundled plugin used by the live bench and native memory contracts.
use std::{
    collections::HashMap,
    sync::{
        Arc, LazyLock, Mutex,
        mpsc::{SyncSender, sync_channel},
    },
    time::Duration,
};
use vosu_core::{
    logging::{ConsoleSink, Logger, LoggerOptions},
    plugin::{self, PluginHost, PluginInstanceId, ProcessRecord},
};
use vosu_osu::{
    memory::PlanProcessMemory,
    scan::{PlanScanBackend, ScanWaiters},
};
use vosu_osu_reader::process::ProcessHandle;
use vosu_plugin_api::{HostServices, Init, Message, Plugin, PluginError, VtableHostServices};
use vosu_sys::process::ProcessBitness;

type Attached = (VtableHostServices, u64, Arc<ScanWaiters>);
static ATTACH: LazyLock<Mutex<HashMap<String, SyncSender<Attached>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
struct ReaderPlugin {
    host: VtableHostServices,
    sender: SyncSender<Attached>,
    scans: Arc<ScanWaiters>,
}
impl Plugin for ReaderPlugin {
    fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        let sender = ATTACH
            .lock()
            .unwrap()
            .remove(&init.plugin_id)
            .expect("registered bench plugin");
        Ok(Self {
            host,
            sender,
            scans: ScanWaiters::new(),
        })
    }
    fn on_message(&mut self, message: Message, correlation: u64) -> Result<(), PluginError> {
        match message {
            Message::SessionAttached(session) => {
                let status =
                    self.host
                        .identify_session(session.handle, "osu", "bench", "bench", "", false);
                if status != vosu_plugin_api::vosu_status::VOSU_OK {
                    return Err(PluginError(status));
                }
                let _ = self
                    .sender
                    .try_send((self.host, session.handle, self.scans.clone()));
            }
            Message::ScanResult(result) => self.scans.deliver(correlation, result),
            Message::SessionDetached(session) => self.scans.detach(session.handle),
            _ => {}
        }
        Ok(())
    }
}
impl Drop for ReaderPlugin {
    fn drop(&mut self) {
        self.scans.shutdown();
    }
}
struct Silent;
impl ConsoleSink for Silent {
    fn is_tty(&self) -> bool {
        false
    }
    fn write_log(&self, _: &str) -> std::io::Result<()> {
        Ok(())
    }
    fn clear_progress(&self, _: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn write_progress(&self, _: &str) -> std::io::Result<()> {
        Ok(())
    }
}
/// Owns host/runtime/directories for at least as long as its process handle.
pub struct Attachment {
    pub process: ProcessHandle,
    pub submissions: Arc<std::sync::atomic::AtomicU64>,
    host: Arc<PluginHost>,
    pid: u32,
    submitter: (VtableHostServices, u64),
    _runtime: tokio::runtime::Runtime,
    _directories: Vec<tempfile::TempDir>,
}
impl AsRef<(VtableHostServices, u64)> for Attachment {
    fn as_ref(&self) -> &(VtableHostServices, u64) {
        &self.submitter
    }
}
impl Attachment {
    pub fn new(record: ProcessRecord, id: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let runtime = tokio::runtime::Runtime::new()?;
        let entered = runtime.enter();
        let logs = tempfile::tempdir()?;
        let config_dir = tempfile::tempdir()?;
        let data = tempfile::tempdir()?;
        let logger = Logger::new(
            logs.path(),
            LoggerOptions {
                version: "bench".into(),
                debug_log: false,
                queue_capacity: 64,
            },
            Arc::new(Silent),
        )?;
        let config = runtime.block_on(vosu_core::config::ConfigStore::initialize(
            &config_dir.path().join("vosu.toml"),
        ))?;
        let storage = plugin::settings::ConfigSettingsBridgeB2::new(Arc::new(config));
        let settings = Arc::new(plugin::settings::PluginSettingsService::new(Arc::new(
            storage,
        )));
        let host = PluginHost::new(logger, settings, runtime.handle().clone());
        let (tx, rx) = sync_channel(1);
        let manifest = format!(
            "kind='plugin'\nmanifest_version=1\nid='{id}'\nname='Reader benchmark'\nversion='0.1.0'\nhost='>=1.0.0, <2.0.0'\nabi_version=1\nruntime='in-process'\n[match]\nprocess_names=[{}]\n",
            serde_json::to_string(&record.name)?
        );
        let loaded = plugin::load_bundled(&[plugin::BundledPlugin {
            manifest: &manifest,
            settings_schema: None,
            vtable: || vosu_plugin_api::__vtable_for_serialized::<ReaderPlugin>(),
        }])?;
        assert!(ATTACH.lock().unwrap().insert(id.into(), tx).is_none());
        let created = host.create_instance(
            PluginInstanceId::new(1),
            &loaded[0],
            data.path().to_path_buf(),
            None,
        );
        ATTACH.lock().unwrap().remove(id);
        created?;
        let pid = record.pid;
        let bitness = match record.bitness {
            vosu_plugin_api::Bitness::X86 => ProcessBitness::X86,
            _ => ProcessBitness::X64,
        };
        host.process_appeared(record);
        let (services, handle, scans) = rx.recv_timeout(Duration::from_secs(5))?;
        let memory = PlanProcessMemory::new(services, handle, pid, bitness);
        let submissions = memory.plan_counter();
        let scan = PlanScanBackend::new(services, handle, scans);
        let process = ProcessHandle::with_scan_backend(Arc::new(memory), bitness, Arc::new(scan));
        drop(entered);
        Ok(Self {
            process,
            submissions,
            host,
            pid,
            submitter: (services, handle),
            _runtime: runtime,
            _directories: vec![logs, config_dir, data],
        })
    }
}
impl Drop for Attachment {
    fn drop(&mut self) {
        self.host.process_exited(self.pid);
        self.host.destroy_all(Duration::from_secs(5));
    }
}

pub fn native_record(pid: u32) -> Result<ProcessRecord, Box<dyn std::error::Error>> {
    #[cfg(target_os = "linux")]
    let (name, path) = (
        vosu_sys::process::process_name(pid)?
            .to_string_lossy()
            .into_owned(),
        vosu_sys::process::process_executable(pid)?
            .to_string_lossy()
            .into_owned(),
    );
    #[cfg(windows)]
    let (name, path) = {
        let path = vosu_sys::process::process_image_path(pid)?;
        let name = std::path::Path::new(&path)
            .file_name()
            .ok_or("missing executable name")?
            .to_string_lossy()
            .into_owned();
        (name, path)
    };
    let bitness = match vosu_sys::process::process_bitness(pid)? {
        ProcessBitness::X86 => vosu_plugin_api::Bitness::X86,
        ProcessBitness::X64 => vosu_plugin_api::Bitness::X64,
    };
    Ok(ProcessRecord {
        pid,
        name,
        path,
        command_line: String::new(),
        argv: None,
        bitness,
        start_time: 1,
    })
}
