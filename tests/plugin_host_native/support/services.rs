//! M5 I0 support: testkit packages in `mode=services`
//! (`crates/vosu-plugin-testkit/examples/testkit_process_plugin/services.rs`),
//! each under its own manifest, plugin id, process name and receipt
//! directory; the production API v1 router served over the test's
//! `PluginHost`; and the `drive` action through core's invocation executor.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use serde_json::{Map, Value, json};
use vosu_core::actions::{
    ActionRegistry as _, InvocationId, InvocationPolicy, InvocationSink, Principal, TerminalOutcome,
};
use vosu_core::plugin::settings::{
    PluginSettingsService, SettingsChangeDelivery as _, SettingsError, SettingsStorage,
};
use vosu_core::plugin::{LoadedPlugin, PluginHost, PluginInstanceId};
use vosu_plugin_api::ActionProgress;

use super::artifacts::{self, Artifact, Staged};
use super::fixture::{Fixture, Receipt};
use super::host::Host;

/// Every bounded wait in the I0 modules.
pub const BOUND: Duration = Duration::from_secs(20);
/// A drive or job that must not time out during a case.
pub const LONG: InvocationPolicy = InvocationPolicy {
    deadline: Duration::from_secs(120),
    grace: Duration::from_secs(5),
};

/// The generated stream's byte at `offset`, exactly as the fixture's
/// `services::generated` computes it.
pub fn generated(offset: u64) -> u8 {
    ((offset % 251) as u8) ^ ((offset >> 12) as u8)
}

/// One services-mode instance to create.
#[derive(Debug, Clone, Default)]
pub struct Spec {
    /// Short name: plugin id `test.i0.<name>`, process `vosu-i0-<name>`.
    pub name: &'static str,
    /// Never identify a session (a `vosu.sessions` observer).
    pub observer: bool,
    /// The `files` root an owner declares.
    pub root: Option<PathBuf>,
    /// The generated stream's size.
    pub stream: u64,
    /// Extra manifest tables appended at the end (`[[requires]]`, ...).
    pub requires: Vec<String>,
    /// Record each `STREAM_READ` past offset 0 and never answer it
    /// (`hold_reads`).
    pub hold_reads: bool,
    /// A dispatch-time fault and its trigger opcode (`fault`, `trigger`).
    pub fault: Option<(&'static str, u32)>,
    /// Load the same library in-process instead of in a plugin host
    /// (`runtime = "in-process"`, a debug-build development package).
    pub in_process: bool,
}

/// One created instance.
pub struct Plugin {
    pub id: u64,
    pub plugin_id: String,
    pub process: String,
    pub fixture: Fixture,
    pub loaded: LoadedPlugin,
    _staged: Staged,
}

/// One host with services-mode instances 1, 2, …
pub struct Services {
    // Field order is drop order: the host (and its children) go first.
    pub host: Host,
    pub plugins: Vec<Plugin>,
    pub root: tempfile::TempDir,
}

const NAMESPACE: &str = r#"{"title":"PLUGIN","version":"1.0.0","type":"object","properties":{
"marker":{"type":"string","maxLength":256,"x-sampling-lane":{"lane":"regular","activity":"always"}},
"count":{"type":"integer","x-sampling-lane":{"lane":"precise","activity":"always"}},
"resources":{"type":"object"}}}"#;
const SETTINGS: &str = r#"[
{"uniqueID":"LABEL","type":"text","title":"Label","value":"initial-label"},
{"uniqueID":"RATE","type":"number","title":"Rate","value":150.0}]"#;
const DRIVE_ARGS: &str = r#"{"type":"object","required":["op"],"additionalProperties":false,"properties":{
"op":{"enum":["publish","publish_invalid","refs","declare","sessions","subscribe","call","call_block","cancel","answer","plan"]},
"text":{"type":"string","maxLength":256},
"path":{"type":"string","maxLength":4096},
"count":{"type":"integer","minimum":0,"maximum":64},
"session":{"type":"integer","minimum":0},
"handle":{"type":"integer","minimum":0},
"plan":{"type":"integer","minimum":0},
"address":{"type":"integer","minimum":0},
"length":{"type":"integer","minimum":0,"maximum":65536},
"invocation":{"type":"integer","minimum":0},
"progress":{"type":"integer","minimum":0,"maximum":5000},
"action":{"type":"string","maxLength":256},
"args":{"type":"object"},
"selectors":{"type":"array","maxItems":16,"items":{"type":"string","maxLength":512}}}}"#;
const DRIVE_RESULT: &str =
    r#"{"type":"object","required":["status"],"properties":{"status":{"type":"integer"}}}"#;
const WORK_ARGS: &str = r#"{"type":"object","required":["n"],"additionalProperties":false,"properties":{
"n":{"type":"integer","minimum":0,"maximum":1000000},
"progress":{"type":"integer","minimum":0,"maximum":5000},
"hold":{"type":"boolean"},
"bad_result":{"type":"boolean"}}}"#;
const WORK_RESULT: &str = r#"{"type":"object","required":["n","invocation","caller"],"additionalProperties":false,"properties":{
"n":{"type":"integer"},"invocation":{"type":"integer"},"caller":{"type":"string","maxLength":256}}}"#;

/// B0's `make`: artifacts to declare, by opaque key (the entry itself).
const MAKE_ARGS: &str = r#"{"type":"object","required":["declare"],"additionalProperties":false,"properties":{
"declare":{"type":"array","maxItems":8,"items":{"type":"object","required":["name","size","source"],"additionalProperties":false,"properties":{
"name":{"type":"string","maxLength":255},"size":{"type":"integer","minimum":0},
"source":{"enum":["stream","file"]},"path":{"type":"string","maxLength":256}}}}}}"#;
const MAKE_RESULT: &str =
    r#"{"type":"object","required":["declared"],"properties":{"declared":{"type":"array"}}}"#;

/// The services package manifest for `spec`.
fn manifest(spec: &Spec, plugin_id: &str, process: &str) -> String {
    let runtime = if spec.in_process {
        "in-process"
    } else {
        "process"
    };
    let mut text = format!(
        "kind = 'plugin'\nmanifest_version = 1\nid = '{plugin_id}'\nname = 'I0 services fixture'\n\
         version = '1.0.0'\nhost = '>=2.0.0-alpha.1, <3.0.0'\nabi_version = 1\nruntime = '{runtime}'\n\
         root_slots = ['files', 'artifacts']\nsettings_schema = 'settings.json'\n\n\
         [targets.x86_64-unknown-linux-gnu]\nlibrary = 'lib/libtestkit_process_plugin.so'\n\
         [targets.x86_64-pc-windows-msvc]\nlibrary = 'lib/testkit_process_plugin.dll'\n\n\
         [match]\nprocess_names = ['{process}', '{process}.exe']\n\n\
         [[provides]]\nid = 'game:fixture'\nversion = '*'\n\
         [[provides]]\nid = 'field:{plugin_id}'\nversion = '1.0.0'\nschema = 'namespace.json'\n"
    );
    for action in ["drive", "work", "make"] {
        text.push_str(&format!(
            "[[provides]]\nid = 'action:{plugin_id}.{action}'\nversion = '1.0.0'\n"
        ));
    }
    for kind in ["doc", "docs", "generated"] {
        text.push_str(&format!(
            "[[provides]]\nid = 'resource:{plugin_id}.{kind}'\nversion = '1.0.0'\n"
        ));
    }
    for (kind, tree) in [("doc", false), ("docs", true), ("generated", false)] {
        text.push_str(&format!(
            "[[resource_kinds]]\nkind = '{kind}'\ntree = {tree}\nfamilies = ['chart-text']\n"
        ));
    }
    for action in ["drive", "work", "make"] {
        text.push_str(&format!(
            "[[actions]]\nname = '{action}'\nversion = '1.0.0'\ntitle = 'I0 {action}'\n\
             args_schema = '{action}.args.json'\nresult_schema = '{action}.result.json'\n\
             session = 'none'\ncli = true\n"
        ));
    }
    for extra in &spec.requires {
        text.push_str(extra);
        text.push('\n');
    }
    text
}

/// Stages `spec`'s package in `dir`: the manifest and its assets, and the
/// prepared testkit library under `lib/`. Prints the `RECEIPT` line.
fn stage(dir: &Path, spec: &Spec, plugin_id: &str, process: &str) -> Staged {
    assert!(!dir.exists(), "{} is reused", dir.display());
    let source = Artifact::Testkit.library();
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    let assets = [
        ("manifest.toml", manifest(spec, plugin_id, process)),
        ("namespace.json", NAMESPACE.replace("PLUGIN", plugin_id)),
        ("settings.json", SETTINGS.to_owned()),
        ("drive.args.json", DRIVE_ARGS.to_owned()),
        ("drive.result.json", DRIVE_RESULT.to_owned()),
        ("work.args.json", WORK_ARGS.to_owned()),
        ("work.result.json", WORK_RESULT.to_owned()),
        ("make.args.json", MAKE_ARGS.to_owned()),
        ("make.result.json", MAKE_RESULT.to_owned()),
    ];
    for (name, text) in assets {
        std::fs::write(dir.join(name), text).unwrap();
    }
    let library = dir.join("lib").join(source.file_name().unwrap());
    std::fs::copy(&source, &library).unwrap();
    let sha256 = artifacts::sha256(&library);
    assert_eq!(
        sha256,
        artifacts::sha256(&source),
        "the staged copy differs"
    );
    println!(
        "RECEIPT artifact={} sha256={sha256} path={}",
        Artifact::Testkit.name(),
        library.display()
    );
    Staged {
        dir: dir.to_owned(),
        library,
        sha256,
    }
}

/// Writes the services-mode control file next to `staged`'s library (the
/// fixture's `control.rs` format).
fn control(fixture: &Fixture, staged: &Staged, spec: &Spec) {
    let mut text = format!(
        "nonce={}\ndir={}\nmode=services\nstream={}\n",
        fixture.nonce,
        fixture.dir.to_str().unwrap(),
        spec.stream
    );
    if let Some(root) = &spec.root {
        text.push_str(&format!("root={}\n", root.to_str().unwrap()));
    }
    if spec.observer {
        text.push_str("observer=1\n");
    }
    if spec.hold_reads {
        text.push_str("hold_reads=1\n");
    }
    if let Some((fault, trigger)) = spec.fault {
        text.push_str(&format!("fault={fault}\ntrigger={trigger}\n"));
    }
    std::fs::write(
        staged
            .library
            .parent()
            .unwrap()
            .join("testkit-fixture.control"),
        text,
    )
    .unwrap();
}

/// Collects one invocation's terminal outcome.
struct Sink {
    outcome: mpsc::SyncSender<TerminalOutcome>,
}

impl InvocationSink for Sink {
    fn progress(&self, _: ActionProgress) {}
    fn finish(&self, result: TerminalOutcome) {
        // A second terminal would be caught by `Job::outcome`'s caller.
        let _ = self.outcome.try_send(result);
    }
}

/// One invocation in flight.
pub struct Job {
    pub id: InvocationId,
    outcome: mpsc::Receiver<TerminalOutcome>,
}

impl Job {
    /// The terminal outcome, within [`BOUND`].
    pub fn outcome(&self) -> TerminalOutcome {
        self.outcome
            .recv_timeout(BOUND)
            .unwrap_or_else(|_| panic!("invocation {} never finished", self.id))
    }

    /// The terminal outcome, if the invocation already finished.
    pub fn try_outcome(&self) -> Option<TerminalOutcome> {
        self.outcome.try_recv().ok()
    }

    /// The `status` a successful `drive` reported.
    pub fn status(&self) -> u32 {
        match self.outcome() {
            TerminalOutcome::Succeeded(value) => value["status"].as_u64().unwrap() as u32,
            other => panic!("drive {} did not succeed: {other:?}", self.id),
        }
    }
}

/// The failure code of `outcome`.
pub fn failure(outcome: TerminalOutcome) -> String {
    match outcome {
        TerminalOutcome::Failed(failure) => failure.code,
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[derive(Default)]
struct MemorySettings(Mutex<std::collections::HashMap<String, Map<String, Value>>>);

impl SettingsStorage for MemorySettings {
    fn load(&self, plugin_id: &str) -> Result<Map<String, Value>, SettingsError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .get(plugin_id)
            .cloned()
            .unwrap_or_default())
    }
    fn store(&self, plugin_id: &str, values: &Map<String, Value>) -> Result<(), SettingsError> {
        self.0
            .lock()
            .unwrap()
            .insert(plugin_id.to_owned(), values.clone());
        Ok(())
    }
}

impl Services {
    /// Creates one instance per spec, with ids 1, 2, …, each started by
    /// the production launcher and `READY`.
    pub fn new(specs: &[Spec]) -> Self {
        let mut run = Self::empty();
        for spec in specs {
            run.add(spec);
        }
        run
    }

    /// A host with no instance yet.
    pub fn empty() -> Self {
        let root = super::scratch();
        let host = Host::new(root.path());
        Self {
            host,
            plugins: Vec::new(),
            root,
        }
    }

    /// Stages, validates and creates the next instance. The package is
    /// validated against the host's live snapshot, so its `requires` can
    /// name namespaces earlier instances registered.
    pub fn add(&mut self, spec: &Spec) {
        let plugin_id = format!("test.i0.{}", spec.name);
        let process = format!("vosu-i0-{}", spec.name);
        let root = self.root.path();
        let staged = stage(
            &root.join(format!("package-{}", spec.name)),
            spec,
            &plugin_id,
            &process,
        );
        let fixture = Fixture::new(&root.join(format!("receipts-{}", spec.name)));
        control(&fixture, &staged, spec);
        let loaded = vosu_core::plugin::load_development(
            &vosu::BUNDLE.version(),
            &self.host().validation_snapshot(),
            &staged.dir,
            true,
        )
        .unwrap_or_else(|error| panic!("{plugin_id} does not validate: {error}"));
        let id = self.plugins.len() as u64 + 1;
        self.host
            .create(id, &loaded)
            .unwrap_or_else(|error| panic!("{plugin_id} did not reach READY: {error}"));
        let created = fixture
            .wait("init", BOUND)
            .unwrap_or_else(|| panic!("{plugin_id} never ran create in services mode"));
        if spec.in_process {
            assert_eq!(created.pid, std::process::id(), "create ran outside core");
        } else {
            assert_ne!(created.pid, std::process::id(), "create ran in core");
        }
        self.plugins.push(Plugin {
            id,
            plugin_id,
            process,
            fixture,
            loaded,
            _staged: staged,
        });
    }

    pub fn plugin(&self, index: usize) -> &Plugin {
        &self.plugins[index]
    }

    pub fn host(&self) -> &Arc<PluginHost> {
        &self.host.host
    }

    /// The live child of instance `index`: its latest `init` receipt.
    pub fn child(&self, index: usize) -> u32 {
        self.plugin(index)
            .fixture
            .receipts("init")
            .last()
            .expect("an init receipt")
            .pid
    }

    /// Invokes `action` of instance `index` as the broker.
    pub fn invoke(&self, index: usize, action: &str, args: Value) -> Job {
        let action = format!("{}.{action}", self.plugin(index).plugin_id);
        let descriptor = self
            .host()
            .get(&action)
            .unwrap_or_else(|| panic!("{action} is not registered"));
        let (outcome_sender, outcome) = mpsc::sync_channel(4);
        let sink = Arc::new(Sink {
            outcome: outcome_sender,
        });
        let _enter = self.host.enter();
        let id = self
            .host()
            .invocation_executor_with_policy(LONG)
            .invoke(Principal::Broker, descriptor, None, args, sink)
            .unwrap_or_else(|error| panic!("{action} was refused: {error}"));
        Job { id, outcome }
    }

    /// Starts one `drive` of instance `index`.
    pub fn drive(&self, index: usize, args: Value) -> Job {
        self.invoke(index, "drive", args)
    }

    /// Offers synthetic target `target` to instance `index` and returns
    /// its attach receipt (identified, root declared, published).
    pub fn attach(&self, index: usize, target: u32) -> Receipt {
        let plugin = self.plugin(index);
        self.host
            .offer_target(target, &plugin.process, target.to_le_bytes().to_vec());
        let attach = plugin
            .fixture
            .wait_for("attach", BOUND, |receipt| {
                receipt.get("target") == target.to_string()
            })
            .unwrap_or_else(|| panic!("{} was never attached to {target}", plugin.plugin_id));
        for key in ["identify", "declare", "publish"] {
            assert_eq!(attach.get(key), "0", "{key} was refused: {attach:?}");
        }
        attach
    }

    /// Waits for instance `index`'s first `probe` receipt accepted by
    /// `accept`.
    pub fn wait(&self, index: usize, probe: &str, accept: impl Fn(&Receipt) -> bool) -> Receipt {
        self.plugin(index)
            .fixture
            .wait_for(probe, BOUND, accept)
            .unwrap_or_else(|| {
                panic!(
                    "{}: no matching {probe} receipt",
                    self.plugin(index).plugin_id
                )
            })
    }

    /// Opens instance `index`'s `<name>-<pid>` barrier for child `pid`.
    pub fn barrier(&self, index: usize, name: &str, pid: u32) {
        let fixture = &self.plugin(index).fixture;
        std::fs::write(fixture.dir.join(format!("{name}-{pid}")), &fixture.nonce).unwrap();
    }

    /// Applies `patch` to instance `index`'s settings through core's
    /// settings service and delivers the resulting `SETTINGS_CHANGED`.
    pub fn apply_settings(&self, index: usize, patch: Value) {
        let plugin = self.plugin(index);
        let service = PluginSettingsService::new(Arc::new(MemorySettings::default()));
        let message = service
            .apply(
                &plugin.plugin_id,
                plugin.loaded.settings_schema().expect("a settings schema"),
                patch.as_object().unwrap(),
            )
            .unwrap();
        let _enter = self.host.enter();
        self.host().settings_changed(&plugin.plugin_id, message);
    }

    pub fn instance(&self, index: usize) -> PluginInstanceId {
        PluginInstanceId::new(self.plugin(index).id)
    }
}

/// Arguments for a `drive`.
pub fn op(op: &str, fields: Value) -> Value {
    let mut args = json!({ "op": op });
    args.as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    args
}

/// Ends process `pid` with the OS, the way crash evidence does.
#[cfg(target_os = "linux")]
pub fn kill(pid: u32) {
    // SAFETY: signals one of this test's own unreaped plugin hosts.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) }, 0);
}

#[cfg(windows)]
pub fn kill(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    // SAFETY: plain call; a null result is checked before use.
    let process = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
    assert!(!process.is_null(), "opening {pid}");
    // SAFETY: `process` is the live handle opened above with terminate
    // access, closed right after.
    unsafe {
        assert_ne!(TerminateProcess(process, 9), 0, "terminating {pid}");
        CloseHandle(process);
    }
}
