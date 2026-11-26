# M7 L2 handoff (m7/l2)

Done (signed): c55ee003 T1 merge; 24725233 merge of f97c3861; 9efb35b1 (d);
fbae450a (b); b00b06ba (c); a5e75838 (a) design-system composition,
ServerAssets/PageService deletion; c329eda9 (i) 2.0 Tools page; 19020536 (h)
dashboard smoke on 2.0 web tools + overlay-host empty-search Prefix route
(the Overlays/Available lists were empty after the merge); 738b9db5 (f)
runbook; 2b8552b5 (g) catalog-v2 link.

Host-header rewrite (deleted in 24725233): it ran after the dashboard Host
check (`src/server/routes.rs` ~355, DNS-rebinding guard, still present) and
only rewrote Host to the overlay origin for CounterSearch so server-rendered
cards got absolute overlay URLs. Cards are origin-relative now and surface
requests never carry Host, so it had no security role; nothing restored.

## Acceptance on 2b8552b5 (logs in /tmp/m7l2-acc/)
- Passed: test-harness build; nextest dashboard/overlay-host/tool-host/core
  1046/1046; daemon_smoke 8, plugin_host, api_v1_compat; server_compat 37,
  config_compat, catalog_v2_compat (seen so far); dashboard_smoke (full,
  `failures: []`, 6 preview frames, previews uncounted); every
  `scripts/check-*.py` except standalone-crates (still running when handed
  off), incl. check-deleted-assertions --base v2/main (local v2/main
  fast-forwarded to origin/v2/main e83598be).
- Also passed: runtime_wakeups, performance_invariants, and `prop_`
  (25 test binaries, 0 failed).
- Not yet confirmed: check-standalone-crates, tool_host_smoke, fmt check,
  workspace clippy -D warnings, `cargo xwin clippy` (C_INCLUDE_PATH unset).

## Remaining
- Run the unconfirmed acceptance items above.
- Optional (A7.3 spirit): `OverlayRoutes::register` skips dashboard-role rows
  when the dashboard is off (needs the dashboard flag plumbed to the host).
