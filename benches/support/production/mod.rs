//! Core side of the M5 production benches (P1 `brokered_reads_bench`'s
//! `production_*` variants, X1 `plan_memory_bench`'s `_process` cases).
//!
//! This bench process is core: a real `PluginHost` with the real process
//! launcher, starting the real `vosu plugin-host` child of the `vosu`
//! executable Cargo built for this bench (`CARGO_BIN_EXE_vosu`). The child
//! loads the bench plugin (`examples/brokered_reads_plugin.rs`), which
//! submits its read plans through the real relay, ring and core ingress to
//! the real broker and executor. Nothing on that path is a bench copy: the
//! only bench-only seams are the `bench-harness` launcher without an
//! approval record and, for X1, the session-memory hook that points a
//! synthetic session at the same fixture memory the `_plan` cases read.
//!
//! Core drives the plugin with [`protocol`] commands outside every timed
//! window; the plugin times its own round trips.

pub mod protocol;

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vosu_core::plugin::process::launcher::ProcessHostLauncher;
use vosu_core::plugin::{LoadedPlugin, PluginHost, PluginInstanceId, ProcessRecord};
use vosu_plugin_api::memory::{ProcessBitness, ProcessMemory};
use vosu_plugin_api::{Bitness, DemandChanged, Message};

use protocol::Fields;

/// The longest any one command, attachment or startup may take.
const BOUND: Duration = Duration::from_secs(120);

struct Console;

impl vosu_core::logging::ConsoleSink for Console {
    fn is_tty(&self) -> bool {
        false
    }
    fn write_log(&self, _text: &str) -> std::io::Result<()> {
        Ok(())
    }
    fn clear_progress(&self, _lines: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn write_progress(&self, _text: &str) -> std::io::Result<()> {
        Ok(())
    }
}

struct EmptySettings;

impl vosu_core::plugin::settings::SettingsStorage for EmptySettings {
    fn load(
        &self,
        _plugin_id: &str,
    ) -> Result<
        serde_json::Map<String, serde_json::Value>,
        vosu_core::plugin::settings::SettingsError,
    > {
        Ok(serde_json::Map::new())
    }
    fn store(
        &self,
        _plugin_id: &str,
        _values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), vosu_core::plugin::settings::SettingsError> {
        Ok(())
    }
}

/// The bench plugin library, built into this profile's `examples`
/// directory by `cargo build --release --example brokered_reads_plugin`
/// (`cargo bench` builds no examples). Missing is a failure, not a skip.
pub fn plugin_library() -> PathBuf {
    let mut path = std::env::current_exe().expect("locate this bench's own executable");
    path.pop();
    path.pop();
    path.push("examples");
    path.push(library_name());
    assert!(
        path.is_file(),
        "missing {}; run `cargo build --locked --release --example brokered_reads_plugin` first",
        path.display()
    );
    path
}

fn library_name() -> &'static str {
    if cfg!(windows) {
        "brokered_reads_plugin.dll"
    } else {
        "libbrokered_reads_plugin.so"
    }
}

fn target_triple() -> &'static str {
    if cfg!(windows) {
        "x86_64-pc-windows-msvc"
    } else {
        "x86_64-unknown-linux-gnu"
    }
}

/// Scratch output in a caller-owned temporary directory, not user state.
#[expect(
    clippy::disallowed_methods,
    reason = "bench scratch files under a temporary directory"
)]
fn write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

/// A real `PluginHost` with the production launcher installed.
pub struct ProductionHost {
    host: Arc<PluginHost>,
    runtime: tokio::runtime::Runtime,
    root: tempfile::TempDir,
    library: Vec<u8>,
    hooked: std::sync::Mutex<Vec<u32>>,
    next_instance: Cell<u64>,
}

impl ProductionHost {
    pub fn new() -> Self {
        let library = std::fs::read(plugin_library()).expect("read the bench plugin library");
        let root = tempfile::tempdir().expect("production bench scratch directory");
        let runtime = tokio::runtime::Runtime::new().expect("core runtime");
        let host = {
            let _enter = runtime.enter();
            let logger = vosu_core::logging::Logger::new(
                root.path().join("logs"),
                vosu_core::logging::LoggerOptions {
                    version: vosu::application_version().to_string(),
                    debug_log: false,
                    queue_capacity: 64,
                },
                Arc::new(Console),
            )
            .expect("core logger");
            let settings = Arc::new(vosu_core::plugin::settings::PluginSettingsService::new(
                Arc::new(EmptySettings),
            ));
            PluginHost::new(logger, settings, runtime.handle().clone())
        };
        let launcher = ProcessHostLauncher::with_executable(
            Path::new(env!("CARGO_BIN_EXE_vosu")),
            vosu::plugin_host::BUILD_IDENTITY,
        )
        .expect("the production process launcher");
        host.install_process_launcher(Arc::new(launcher))
            .expect("install the production process launcher");
        Self {
            host,
            runtime,
            root,
            library,
            hooked: std::sync::Mutex::new(Vec::new()),
            next_instance: Cell::new(1),
        }
    }

    /// Stages and validates a `runtime = "process"` development package of
    /// the bench plugin matching `process_names`, exactly as `--plugin`
    /// loads one.
    pub fn package(&self, plugin_id: &str, process_names: &[&str]) -> LoadedPlugin {
        let dir = self.root.path().join("packages").join(plugin_id);
        std::fs::create_dir_all(dir.join("lib")).expect("package directory");
        let names = process_names
            .iter()
            .map(|name| format!("'{name}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let manifest = format!(
            "kind='plugin'\nmanifest_version=1\nid='{plugin_id}'\nname='{plugin_id}'\n\
             version='1.0.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='process'\n\n\
             [match]\nprocess_names=[{names}]\n\n\
             [[provides]]\nid='game:fixture'\nversion='*'\n\
             [[provides]]\nid='field:{plugin_id}'\nversion='1.0.0'\nschema='schema.json'\n\n\
             [targets.{target}]\nlibrary='lib/{library}'\n",
            target = target_triple(),
            library = library_name(),
        );
        write(&dir.join("manifest.toml"), manifest.as_bytes());
        write(
            &dir.join("schema.json"),
            format!(
                r#"{{"title":"{plugin_id}","version":"1.0.0","type":"object","properties":{{"value":{{"type":"integer"}}}}}}"#
            )
            .as_bytes(),
        );
        write(&dir.join("lib").join(library_name()), &self.library);
        vosu_core::plugin::load_development(
            vosu::application_version(),
            &vosu_core::plugin::ValidationSnapshot::default(),
            &dir,
            cfg!(debug_assertions),
        )
        .unwrap_or_else(|error| panic!("{} does not validate: {error}", dir.display()))
    }

    /// Creates one instance of `loaded`; returns once its child reached
    /// `READY`, with the time `create_instance` took (spawn to `READY`
    /// plus core's registration).
    pub fn start(&self, loaded: &LoadedPlugin) -> (Instance<'_>, Duration) {
        let id = self.next_instance.get();
        self.next_instance.set(id + 1);
        let dir = self.root.path().join("data").join(id.to_string());
        std::fs::create_dir_all(&dir).expect("instance data directory");
        let _enter = self.runtime.enter();
        let started = Instant::now();
        self.host
            .create_instance(PluginInstanceId::new(id), loaded, dir.clone(), None)
            .unwrap_or_else(|error| panic!("plugin host instance {id} failed to start: {error}"));
        let startup = started.elapsed();
        (
            Instance {
                host: self,
                id,
                dir,
                next_command: Cell::new(1),
            },
            startup,
        )
    }

    /// Reports `pid` named `name` to the matcher, as discovery does.
    pub fn offer(&self, pid: u32, name: &str, bitness: Bitness) {
        let _enter = self.runtime.enter();
        self.host.process_appeared(ProcessRecord {
            pid,
            name: name.to_owned(),
            path: format!("/bench/{name}"),
            command_line: name.to_owned(),
            bitness,
            start_time: 1,
            argv: None,
        });
    }

    /// Reports that `pid` exited, detaching its session.
    pub fn exited(&self, pid: u32) {
        let _enter = self.runtime.enter();
        self.host.process_exited(pid);
    }

    /// Points core's session memory for the synthetic `pid` at `memory`
    /// (the `bench-harness` hook): core then executes that session's plans
    /// against exactly the memory the in-process `_plan` cases read.
    pub fn hook(&self, pid: u32, memory: Arc<dyn ProcessMemory>, bitness: ProcessBitness) {
        vosu_core::plugin::test_support::set_session_memory(pid, memory, bitness);
        self.hooked.lock().unwrap().push(pid);
    }
}

impl Drop for ProductionHost {
    fn drop(&mut self) {
        let _enter = self.runtime.enter();
        self.host.destroy_all(Duration::from_secs(5));
        for pid in self.hooked.lock().unwrap().drain(..) {
            vosu_core::plugin::test_support::clear_session_memory(pid);
        }
    }
}

/// One running plugin-host instance and its command channel.
pub struct Instance<'h> {
    host: &'h ProductionHost,
    id: u64,
    dir: PathBuf,
    next_command: Cell<u64>,
}

impl Instance<'_> {
    /// Runs one command in the plugin and returns its reply.
    #[expect(
        clippy::disallowed_methods,
        reason = "the plugin answers through a file; core polls for it outside every timed window"
    )]
    pub fn command(&self, command: &Fields) -> Fields {
        let n = self.next_command.get();
        assert_eq!(
            n & protocol::DOORBELL_MASK,
            0,
            "command numbers fit the doorbell"
        );
        self.next_command.set(n + 1);
        write(
            &protocol::command_path(&self.dir, n),
            command.render().as_bytes(),
        );
        {
            let _enter = self.host.runtime.enter();
            self.host
                .host
                .dispatch_to_instance(
                    PluginInstanceId::new(self.id),
                    &Message::DemandChanged(DemandChanged {
                        handle: protocol::DOORBELL | n,
                        regular: false,
                        precise: false,
                        selectors: Vec::new(),
                    }),
                    0,
                )
                .expect("core accepted the doorbell");
        }
        let reply = protocol::reply_path(&self.dir, n);
        let deadline = Instant::now() + BOUND;
        while !reply.is_file() {
            assert!(
                Instant::now() < deadline,
                "instance {} never answered {command:?}",
                self.id
            );
            self.assert_live();
            std::thread::sleep(Duration::from_micros(200));
        }
        let fields = Fields::parse(&std::fs::read_to_string(&reply).expect("read the reply"));
        if let Some(error) = fields.optional("error") {
            panic!("instance {} failed {command:?}: {error}", self.id);
        }
        fields
    }

    /// Waits until core attached the session for `pid` to this instance.
    #[expect(
        clippy::disallowed_methods,
        reason = "the plugin records its attachment in a file; core polls for it before measuring"
    )]
    pub fn wait_attached(&self, pid: u32) {
        let marker = protocol::attached_path(&self.dir, pid);
        let deadline = Instant::now() + BOUND;
        while !marker.is_file() {
            assert!(
                Instant::now() < deadline,
                "instance {} never attached pid {pid}",
                self.id
            );
            self.assert_live();
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Fails the run once core saw this instance's plugin host fail: every
    /// figure must come from the one generation started for it.
    fn assert_live(&self) {
        let status = self
            .host
            .host
            .process_status(PluginInstanceId::new(self.id))
            .expect("a process instance");
        assert!(
            status.last_failure.is_none() && status.restarts == 0,
            "instance {}'s plugin host was lost: {status:?}",
            self.id
        );
    }

    /// Controlled destroy (§7.6): `DESTROY`, the grace, then kill and reap.
    pub fn stop(self) {
        let _enter = self.host.runtime.enter();
        self.host
            .host
            .teardown_instance(PluginInstanceId::new(self.id))
            .unwrap_or_else(|error| panic!("instance {} teardown: {error}", self.id));
    }
}
