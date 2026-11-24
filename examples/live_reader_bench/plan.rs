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
/// Per plugin id: the attachment channel and the identified client variant.
type Registration = (SyncSender<Attached>, &'static str);
static ATTACH: LazyLock<Mutex<HashMap<String, Registration>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// Attachments registering the same plugin id (concurrent tests) create one
/// at a time; the registry entry lives only for that creation.
static CREATE: Mutex<()> = Mutex::new(());
struct ReaderPlugin {
    host: VtableHostServices,
    sender: SyncSender<Attached>,
    variant: &'static str,
    scans: Arc<ScanWaiters>,
}
impl Plugin for ReaderPlugin {
    fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        let (sender, variant) = ATTACH
            .lock()
            .unwrap()
            .remove(&init.plugin_id)
            .expect("registered bench plugin");
        Ok(Self {
            host,
            sender,
            variant,
            scans: ScanWaiters::new(),
        })
    }
    fn on_message(&mut self, message: Message, correlation: u64) -> Result<(), PluginError> {
        match message {
            Message::SessionAttached(session) => {
                let status = self.host.identify_session(
                    session.handle,
                    "osu",
                    self.variant,
                    "bench",
                    "",
                    false,
                );
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
    pub publication: Option<(VtableHostServices, u64)>,
    /// The bundled upstream adapter's own surface requester (`--adapter` only).
    pub adapter: Option<Arc<dyn vosu_surface::requester::BundledRequester>>,
    pub submissions: Arc<std::sync::atomic::AtomicU64>,
    host: Arc<PluginHost>,
    pid: u32,
    submitter: (VtableHostServices, u64),
    runtime: tokio::runtime::Runtime,
    _directories: Vec<tempfile::TempDir>,
}
impl AsRef<(VtableHostServices, u64)> for Attachment {
    fn as_ref(&self) -> &(VtableHostServices, u64) {
        &self.submitter
    }
}
impl Attachment {
    pub fn new(record: ProcessRecord, id: &str) -> Result<Self, Box<dyn std::error::Error>> {
        Self::new_inner(record, id, false, None)
    }
    pub fn with_bus(record: ProcessRecord) -> Result<Self, Box<dyn std::error::Error>> {
        Self::new_inner(record, "vosu.osu", true, None)
    }
    /// The production osu! grants (manifest minus actions/settings) plus the
    /// bundled upstream adapter, so adapters read through their own surface
    /// requester. `variant` is the identified client (`stable`/`lazer`).
    pub fn with_adapter(
        record: ProcessRecord,
        variant: &'static str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Self::new_inner(record, "vosu.osu", true, Some(variant))
    }
    pub fn runtime(&self) -> &tokio::runtime::Runtime {
        &self.runtime
    }
    pub fn subscribe(
        &self,
    ) -> Result<vosu_surface::state::SurfaceSubscription, Box<dyn std::error::Error>> {
        Ok(self.host.surface_host().subscribe(
            "vosu.osu",
            vosu_surface::state::SessionTarget::Focused,
            vec![vosu_plugin_api::Selector {
                ns: "vosu.osu".into(),
                pointer: "".into(),
            }],
        )?)
    }
    fn new_inner(
        record: ProcessRecord,
        id: &str,
        bus: bool,
        adapter: Option<&'static str>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
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
        let manifest = if adapter.is_some() {
            let mut production: toml::Table =
                toml::from_str(include_str!("../../crates/vosu-osu/manifest.toml"))?;
            for key in ["actions", "concurrent_dispatch", "settings_schema"] {
                production.remove(key);
            }
            if let Some(provides) = production
                .get_mut("provides")
                .and_then(toml::Value::as_array_mut)
            {
                provides.retain(|provide| {
                    !provide
                        .get("id")
                        .and_then(toml::Value::as_str)
                        .is_some_and(|id| id.starts_with("action:"))
                });
            }
            let mut matcher = toml::Table::new();
            matcher.insert(
                "process_names".into(),
                toml::Value::Array(vec![record.name.clone().into()]),
            );
            production.insert("match".into(), matcher.into());
            toml::to_string(&production)?
        } else if bus {
            let production = vosu_plugin_api::PluginManifest::parse(include_str!(
                "../../crates/vosu-osu/manifest.toml"
            ))?;
            format!(
                "kind='plugin'\nmanifest_version=1\nid='{id}'\nname='Bus benchmark'\nversion='2.0.0-alpha.0'\nhost='*'\nabi_version=1\nruntime='in-process'\ncovers={}\n[[provides]]\nid='field:vosu.osu'\nversion='1.2.0'\nschema='schema.json'\n[match]\nprocess_names=[{}]\n",
                serde_json::to_string(&production.covers)?,
                serde_json::to_string(&record.name)?
            )
        } else {
            manifest
        };
        let loaded = plugin::load_bundled(&[plugin::BundledPlugin {
            manifest: &manifest,
            settings_schema: None,
            namespace_schemas: if adapter.is_some() {
                &[vosu_osu::OSU_NAMESPACE_SCHEMA]
            } else if bus {
                &[include_str!("../../docs/v2/schemas/vosu.osu.schema.json")]
            } else {
                &[]
            },
            action_schemas: &[],
            vtable: || vosu_plugin_api::__vtable_for_serialized::<ReaderPlugin>(),
        }])?;
        let creating = CREATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            ATTACH
                .lock()
                .unwrap()
                .insert(id.into(), (tx, adapter.unwrap_or("bench")))
                .is_none()
        );
        let created = host.create_instance(
            PluginInstanceId::new(1),
            &loaded[0],
            data.path().to_path_buf(),
            None,
        );
        ATTACH.lock().unwrap().remove(id);
        drop(creating);
        created?;
        let requester = match adapter {
            Some(_) => {
                let upstream = plugin::load_bundled(&[plugin::BundledPlugin {
                    manifest: vosu_adapter_upstream::MANIFEST,
                    settings_schema: None,
                    namespace_schemas: &[],
                    action_schemas: &[],
                    vtable: vosu_adapter_upstream::vosu_plugin_vtable,
                }])?;
                let id = PluginInstanceId::new(2);
                host.create_instance(id, &upstream[0], data.path().join("upstream"), None)?;
                Some(host.surface_requester(id)?)
            }
            None => None,
        };
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
            publication: bus.then_some((services, handle)),
            adapter: requester,
            submissions,
            host,
            pid,
            submitter: (services, handle),
            runtime,
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
