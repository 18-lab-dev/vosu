//! M4b behavioral goldens (`tests/fixtures/upstream/GOLDEN_CONTRACT.md`).
//!
//! `replay` is the real-host replay that golden cases run through: a
//! recorded `vosu-adapter-capture-v1` selection is published by a bundled
//! fixture game through `VOSU_OP_PUBLISH_BATCH`, and the production
//! composition serves it from its real listener. A vosu dump is only ever
//! replay input. Expected route records come from upstream observations.
//!
//! `golden_cases` holds the pinned-fixture test and the comparator
//! self-tests over the committed synthetic mechanics fixture
//! (`tests/fixtures/golden-synthetic/`, written by G0's writer with
//! `provenance: "synthetic"`), which is never pilot or full evidence.
#![expect(
    clippy::disallowed_methods,
    reason = "Test fixtures write replay assets and poll bounded settle loops"
)]
use serde_json::{Value, json};

#[path = "adapter_golden/assets.rs"]
mod assets;
#[path = "adapter_golden/case_file.rs"]
mod case_file;
#[path = "adapter_golden/compare.rs"]
mod compare;
#[path = "adapter_golden/divergence.rs"]
mod divergence;
#[path = "adapter_golden/envelope.rs"]
mod envelope;
#[path = "adapter_golden/fixture.rs"]
mod fixture;
#[path = "adapter_golden/golden_cases.rs"]
mod golden_cases;
#[path = "adapter_golden/golden_gates.rs"]
mod golden_gates;
#[path = "adapter_golden/replay.rs"]
mod replay;
#[path = "adapter_golden/replay_cases.rs"]
mod replay_cases;
#[path = "adapter_golden/routes.rs"]
mod routes;
#[path = "adapter_golden/source.rs"]
mod source;
#[path = "adapter_golden/synthetic.rs"]
mod synthetic;
#[path = "adapter_golden/websocket.rs"]
mod websocket;

#[path = "state_api_compat/oracle.rs"]
#[expect(
    dead_code,
    reason = "Shared source-oracle helpers; the round-trip source uses a subset"
)]
mod oracle;
#[path = "state_bus_compat/seeds.rs"]
#[expect(
    dead_code,
    reason = "Shared nonzero seeds; the state-bus coverage helper is unused here"
)]
mod seeds;
