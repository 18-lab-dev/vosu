//! The two runtimes of one source-oracle replay (M5 X0 parity).
//!
//! - **In-process**: M4a's own replay (`oracle::replay`): the fixture game
//!   linked into this test publishes each committed snapshot through
//!   `vosu_osu::fixtures::Roster` on this process's threads.
//! - **Process**: the external `osu_process_plugin` library, staged with its
//!   real `vosu.osu` fixture manifest and loaded through the test harness's
//!   official seam (`vosu_core::plugin::load_official_test_fixture`), runs
//!   the same ingress in a real `vosu plugin-host` child
//!   (`crates/vosu-osu/examples/osu_process_plugin/parity.rs`).
//!
//! Both compose the same bundled adapters at instances 2–4, the same
//! providers and the same target PIDs, so a replay differs only by runtime.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::oracle::{self, Input, Replay, adapter_fixture::Fixture};
use crate::support::artifacts::{Artifact, Staged};
use crate::support::host::{prime_executable, wait_until};

pub const BOUND: Duration = Duration::from_secs(20);
/// The first target PID; `adapter_fixture::Fixture::attach` numbers the
/// in-process replay's targets from here too.
const FIRST_PID: u32 = 750_001;

/// Which runtime a replay ran in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runtime {
    InProcess,
    Process,
}

/// One replay and what driving it needs.
pub struct Run {
    pub runtime: Runtime,
    pub replay: Replay,
    /// The target PID of each source instance, in attach order.
    pub pids: Vec<u32>,
    /// The process runtime's receipt directory.
    receipts: Option<PathBuf>,
    _package: Option<(tempfile::TempDir, Staged)>,
}

/// The committed input of case `id`.
pub fn input(id: &str) -> Input {
    let inventory: oracle::Inventory = serde_json::from_slice(
        &std::fs::read(oracle::fixture_root().join("inventory.json")).unwrap(),
    )
    .unwrap();
    let case = inventory
        .cases
        .iter()
        .find(|case| case.id == id)
        .unwrap_or_else(|| panic!("no oracle case {id}"));
    serde_json::from_slice(&std::fs::read(oracle::fixture_root().join(&case.input)).unwrap())
        .unwrap()
}

fn input_path(id: &str) -> PathBuf {
    oracle::fixture_root().join(id).join("input.json")
}

impl Run {
    /// M4a's in-process replay of `input`, once every publisher settled,
    /// as the child's receipts wait for.
    pub fn in_process(input: &Input) -> Self {
        let replay = oracle::replay(input);
        let pids = (0..replay.instances.len() as u32)
            .map(|index| FIRST_PID + index)
            .collect();
        let run = Self {
            runtime: Runtime::InProcess,
            replay,
            pids,
            receipts: None,
            _package: None,
        };
        run.settle();
        run
    }

    /// The process replay of `input`; `update` names a second committed
    /// case the child publishes to the selected session on [`Self::update`].
    pub fn process(input: &Input, update: Option<&str>) -> Self {
        prime_executable();
        let instances = oracle::instances(input);
        let pids: Vec<u32> = (0..instances.len() as u32)
            .map(|index| FIRST_PID + index)
            .collect();
        let chart = oracle::chart_content(input).is_some();
        let scratch = crate::support::scratch();
        let staged = Artifact::Osu.stage(&scratch.path().join("package"), &[]);
        let mut receipts = None;
        let fixture = Fixture::with_game(
            oracle::client(input),
            chart,
            oracle::fixture_engine(input),
            |h| {
                let launcher =
                    vosu_core::plugin::process::launcher::ProcessHostLauncher::with_executable(
                        Path::new(env!("CARGO_BIN_EXE_vosu")),
                        vosu::plugin_host::BUILD_IDENTITY,
                    )
                    .unwrap();
                h.host.install_process_launcher(Arc::new(launcher)).unwrap();
                let directory = h.root.path().join("receipts");
                std::fs::create_dir_all(&directory).unwrap();
                let control = serde_json::json!({
                    "input": input_path(&input.id),
                    "update": update.map(input_path),
                    "chart_root": h.root.path(),
                    "pids": pids,
                    "receipts": directory,
                });
                std::fs::write(
                    h.root.path().join("parity.json"),
                    serde_json::to_vec_pretty(&control).unwrap(),
                )
                .unwrap();
                receipts = Some(directory);
                let loaded = vosu_core::plugin::load_official_test_fixture(
                    vosu::application_version(),
                    &h.host.validation_snapshot(),
                    &staged.dir,
                    cfg!(debug_assertions),
                )
                .expect("the official seam loads the external osu! package");
                assert_eq!(loaded.manifest().id, "vosu.osu");
                assert!(matches!(
                    loaded.execution(),
                    vosu_core::plugin::Execution::Process(_)
                ));
                h.create(&loaded);
            },
        );
        let receipts = receipts.unwrap();
        // The same attach order, records and focus as `Fixture::attach`.
        for (index, (instance, pid)) in instances.iter().zip(&pids).enumerate() {
            let mut record = oracle::adapter_fixture::host::record(*pid, "osu!.exe");
            record.path = "/fixture/osu!.exe".into();
            record.bitness = vosu_plugin_api::Bitness::X86;
            fixture.h.host.process_appeared(record);
            assert!(
                wait_until(BOUND, || receipts.join(format!("published-{pid}")).exists()),
                "the child never published source instance {index}"
            );
            if instance.pid == 10 {
                fixture.h.foreground.set(Some(*pid));
                fixture.h.host.foreground_changed();
            }
        }
        Self {
            runtime: Runtime::Process,
            replay: Replay { fixture, instances },
            pids,
            receipts: Some(receipts),
            _package: Some((scratch, staged)),
        }
    }

    /// The selected session's target PID.
    pub fn selected_pid(&self) -> u32 {
        let index = self
            .replay
            .instances
            .iter()
            .position(|instance| instance.pid == 10)
            .expect("a selected source instance");
        self.pids[index]
    }

    /// Publishes committed case `update` to the selected session and waits
    /// until core acknowledged it: on this thread in-process, in the child
    /// (which already read the case) in the process runtime.
    pub fn update(&self, update: &Input) {
        match &self.receipts {
            None => {
                let (handle, _) = super::bus::selected(&self.replay);
                let publisher = self.replay.fixture.publisher(handle).unwrap();
                publisher
                    .publish(oracle::state(update), vosu_osu_state::Delivery::All)
                    .unwrap();
                assert!(wait_until(BOUND, || publisher.settled()));
            }
            Some(receipts) => {
                std::fs::write(receipts.join("update"), b"1").unwrap();
                assert!(
                    wait_until(BOUND, || receipts.join("updated").exists()),
                    "the child never published the update"
                );
            }
        }
    }

    /// Every in-process publisher settled.
    fn settle(&self) {
        for handle in self
            .replay
            .fixture
            .h
            .host
            .diagnostic_sessions()
            .iter()
            .map(|session| session.handle)
        {
            if let Some(publisher) = self.replay.fixture.publisher(handle) {
                assert!(wait_until(BOUND, || publisher.settled()));
            }
        }
    }
}
