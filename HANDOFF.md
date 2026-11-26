# F B0 handoff (branch f/b0-artifacts)

Done (commits ac5ab38f, a23d02a2): SDK opcodes 80-83 + codecs + header,
core job-owned artifact registry, sessionless lease owner, host
declare/open/release, `GET`/`HEAD /api/v1/jobs/{job}/artifacts/{artifact}`,
vosux listing, api_v1_compat `artifacts::*` (10 tests), SDK tests, docs
(plugin-abi §6.13, api-v1 Job artifacts, release matrix), changelog, SDK
version 1.0.0-alpha.2.

Remaining:
1. Bridge: `assets/server/tool-bridge.js` — `vosu.artifacts(jobId)` and
   `JobHandle.artifacts()` returning `{id,name,type,size, fetch({range,signal,method}),
   download({signal})}`; build URLs from validated ids, send
   `X-Vosu-Tool-Session` only there, `credentials:"omit"`, `redirect:"error"`,
   `cache:"no-store"`; download streams via `showSaveFilePicker` when present,
   Blob fallback otherwise. Update `docs/tools/protocol.md` frozen types,
   `tests/tool_bridge.mjs`, the browser phase of `tests/tool_host_smoke.py`, and
   `python scripts/check-asset-hashes.py --write`.
2. Real process plugin: add an artifact action to
   `crates/vosu-plugin-testkit/examples/testkit_process_plugin/` (new module;
   services.rs is 875 lines) and `tests/plugin_host_native/artifacts.rs`. The
   native `support/api.rs` `Api` needs a constructor that also passes
   `.with_actions(ActionRoutes::new(...))`. Cover: a multi-chunk stream and a
   plugin-wide-root file with no session, STREAM_CLOSE before ARTIFACT_RELEASE
   receipts, and child kill -> 410.
3. Source map row (integrator-owned): job artifacts in
   `crates/vosu-core/src/actions/{artifacts.rs,jobs/artifact_jobs.rs}`,
   `plugin/host/artifacts.rs`, `resource_io/artifact.rs`,
   `src/server/api_v1/artifacts.rs`.
