# M7 C0 handoff (`m7/c0`)

C0 is complete and verified. No commits were pushed and no prepush or VM
was run. Native Windows runtime behavior was not exercised.

## Signed commits

- `e9374131`, `fe604bb7`, `26582373`, `4a1a8c54`: shared filter values,
  generic server, bus provider/capture and removal of route fallbacks.
- `02987f12`: intermediate move of lifecycle, service and tools into core;
  this historical WIP does not compile by itself.
- `4d128000`: compiling core composition, version threading and private
  executable-specific host glue.
- `035f8afa`: merge of landed `4d4e576a` from `origin/v2/main`; the moved
  `overlays.query` binding uses `Origins::overlay_listener()`.
- `93e5baf1`: caller and test cutover, shutdown joins, root export guard,
  dependency inventories and ownership docs.

Core owns lifecycle, HTTP/WebSocket services and the tool registry. The root
library exports only `BUNDLE` and `BUNDLED`; private glue wires the concrete
providers. The root overlay composition retains the weak process-wide query
rendezvous and the browser relay. `vosu-adapter-support -> vosu-surface` is
deliberate and limited to the shared filter type and bounds.

## Verification

- Core nextest, all features: 1,234 passed, 1 skipped.
- Root library/binary tests: 39/10 passed. The regressions check exact root
  exports and live command/API v1 WebSocket and task joins before held plugin
  destruction.
- `daemon_smoke`, `plugin_host`, `state_bus` and `api_v1_compat` with
  `test-harness`; `server_compat`, `config_compat`, `catalog_v2_compat` and
  `upstream_wire_extensions`: passed.
- Extra migrated fixture checks: `catalog_runtime`, one synthetic golden
  replay and the five local `vosu-test-harness` network tests passed. The
  network suite's 11 external-forge/CA tests remained ignored.
- `dashboard_smoke.py` and `tool_host_smoke.py`: passed serially with the
  feature-enabled executable and a free-port check before each run.
- Workspace rustfmt, Linux workspace Clippy and MSVC xwin workspace Clippy
  with `C_INCLUDE_PATH` unset: passed. Standalone check passed on both
  targets for all 26 workspace members.
- All `scripts/check-*.py` passed, including deleted assertions against
  `v2/main` with the relocation trailer. CI path classification tests passed.
