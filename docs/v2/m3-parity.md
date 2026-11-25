# M3 route and test parity inventory

This inventory maps each M3 contract (API v1, resources and trees, actions,
jobs and clients) to the executable test that owns it, plus the live Windows
evidence recorded at the M3 exit. The route list itself is the
[final API-v1 route inventory](../testing/verification.md#final-api-v1-route-inventory);
the contract text is [API v1](api-v1.md). A row is covered only by the named
test or recorded observation, never by cross-compilation.

Integration targets listed at the exit (`cargo nextest list --features
test-harness`): `api_v1_compat` 33, `daemon_smoke` 2 (Linux only),
`plugin_host` 8, `osu_plugin` 8, `state_bus` 6, `prerelease_bootstrap` 1.

## Public state and subscriptions

| Contract | Owning test |
| --- | --- |
| Hidden attach, identify-only visibility | `api_v1_compat sessions::attached_but_unidentified_session_is_hidden_on_every_route` |
| Session events, removed selector positions | `api_v1_compat sessions::f5_abi_fixture_identifies_and_session_events_remove_selected_indices` |
| `active` rebinding with fresh initial and monotonic revision | `api_v1_compat sessions::active_stream_rebinds_with_fresh_initial_and_monotonic_revision` |
| Whole/namespace/pointer reads, methods, near misses, envelopes | `api_v1_compat state::all_state_routes_methods_queries_and_near_misses_use_api_envelopes` |
| Fixed-id detach closes; `active` rebinds | `api_v1_compat subscriptions::upgrade_detach_barrier_distinguishes_fixed_id_from_active`, `subscriptions::established_fixed_id_removes_indices_then_closes_normally` |
| Invalid requests, immutable selection, ping, 5 s timeout | `api_v1_compat subscriptions::invalid_requests_immutable_selection_ping_and_timeout_cross_transport` |
| 1 MiB frame boundary | `api_v1_compat subscriptions::escaped_output_at_one_mib_succeeds_and_one_byte_over_fails` |
| Full pong queue closes 1011 | `api_v1_compat subscriptions::actual_pong_slot_overflow_closes_and_preserves_first_payload` |
| Latest-only slow consumer | `api_v1_compat subscriptions::slow_writer_retains_only_latest_pending_snapshot` |
| Subscription demand registration/release; reads add none | `api_v1_compat subscriptions::transport_subscription_registers_and_releases_root_demand` |
| Pointer parsing and limits | `prop_api_pointer_*` (`src/server/api_v1/pointer_properties.rs`), `prop_pointer_parse_roundtrip` |
| Dashboard/Origin/peer refusal, upgrade allowlist | `api_v1_compat security::dashboard_origin_peer_policy_and_upgrade_allowlist_fail_closed` |
| `internal` never public; authorized plugin positive control | `api_v1_compat security::authorized_plugin_sees_private_paths_but_public_reads_caches_and_frames_never_do`, `state_bus internal_sentinel_never_reaches_public_projections_events_or_tracing` |
| Independent `API_V1_SUBSCRIPTION_DEBOUNCE` | config unit tests in `crates/vosu-core/src/config` |

## Resources and trees

| Contract | Owning test | Live Windows (2026-11-24) |
| --- | --- | --- |
| Lazy resolution, failed open retried, rejected batch leaves no membership | `api_v1_compat resource_lifecycle::resources_failed_open_retries_and_rejected_batch_leaves_no_membership` | — |
| Shared references across sessions, root precedence, stream close | `api_v1_compat resource_lifecycle::resources_shared_sessions_lazy_cache_root_precedence_and_stream_close` | lazer reference still 200 after the concurrent stable session detached |
| Restart-stable ids, rotated secret | `api_v1_compat resource_lifecycle::resources_restart_persistent_secret_and_rotated_secret_http`, `prop_resource_id_*` | — |
| 404 forged id vs 410 retired id | `prop_resource_id_tampered_or_arbitrary_tokens_fail`, `resource_lifecycle::resources_eviction_of_current_reference_is_410_not_resolver_access` | bogus id 404; ids of a detached stable session 410 (`unpublished_resource`/`unpublished_tree`) |
| Mutation retires the old id; new publication serves new bytes | `api_v1_compat resource_lifecycle::resources_mutation_typed_gone_http_410_and_new_publication` | not exercised live |
| Current-provider failure 502, not 410; mid-stream detach | `api_v1_compat resource_lifecycle::streams::*` | — |
| Ranges, HEAD, validators, 304/416 | `api_v1_compat resources::resources_file_stream_ranges_head_and_validators` | stable and lazer file/audio/background: exact bytes, bodyless HEAD, 206 `bytes 1-7`, 304, 416 `bytes */size` |
| Tree members, errors, disk mutation | `api_v1_compat resources::resources_tree_members_errors_and_disk_mutation`, `prop_tree_path_*` | set index/member/range; unpublished 404; escape 404; index HEAD 405 |
| Skin case-folded alias; set trees exact only | `vosu-osu resources::tests::resources_skin_exact_beats_folded_alias_and_failed_open_can_recover` | stable test skin `HITCIRCLE.PNG` served `HitCircle.png`; set `AUDIO.MP3` 404 |
| 307 convenience routes point only at published ids | `api_v1_compat resource_redirects::resources_convenience_redirect_only_published_urls_and_encode_once` | chart file/audio/background, `active`, skin `CURSOR.png` |
| Real stable/lazer direct, redirect, tree, mutation and privacy | `api_v1_compat trees::resources_real_osu_stable_and_lazer_direct_redirect_trees_mutation_and_privacy` | no native path in API-v1 bodies |
| Retained-handle confinement | `vosu-sys fs::tests::*` (Linux; Windows natively at High IL 61/61) | — |
| Daemon serves debug-plugin bytes | `daemon_smoke api_fixture::debug_fixture_is_discovered_identified_and_serves_published_bytes` (Linux) | — |

## Actions, jobs and clients

| Contract | Owning test | Live Windows |
| --- | --- | --- |
| Authority, limits, lifecycle, two distinguishable actions | `api_v1_compat actions::action_jobs_authority_limits_and_lifecycle` | — |
| Waiter limits, retention, eviction, running capacity | `api_v1_compat jobs::public_job_waiter_limits_retention_eviction_and_running_capacity` | — |
| Timeout vs late ABI success; `plugin_crashed` via the instance-loss seam | `api_v1_compat jobs::timeout_survives_real_late_abi_success_and_instance_loss_fails_running_job` | OS crash/kill proof is M5 |
| 64 relay slots, id remapping, oversize results | `api_v1_compat relay::*` | — |
| `vosux` CLI routing, sessions, detach, polling, SIGINT→DELETE | `api_v1_compat cli::vosux_production_action_routes` | `vosu x vosu.osu beatmap.import --session …` used for every live import |
| Import: bounded archive, selected client, no clobber, one terminal | `api_v1_compat import::real_osu_import_action_http_registry_schema_and_terminal`, `vosu-osu actions::import::tests` | stable `queued`, one new byte-identical `.osz`, 0 changed of 436; lazer `submitted`, selected executable, child reaped |
| Browser bridge invoke/start/progress/abort, page-memory credentials | `tests/tool_host_smoke.py` browser phase | — |
| Alpha isolation and restart | `prerelease_bootstrap prerelease_daemon_startup_restart_preserves_normal_settings` | alpha binary ran from an isolated program folder |
| Legacy APIs unchanged beside API v1 | `server_compat`, `state_api_compat` | `/json`, `/json/v2`, `/json/v2/precise`, `/json/sc`, `/files/beatmap/*`, `/files/skin/*` and `/websocket/v2` served; legacy file bytes equal the API v1 resource |

## Not covered at the M3 exit

- Lazer game-side import of the handed-off archive was not observed within
  30 s (store and realm unchanged). The vosu side of the contract held.
- Concurrent selected-destination import: both attempts on a stable session
  identified seconds earlier returned `target_gone` before the session had
  published its chart facts.
- Linux lazer and Linux stable/Wine live runs, native Windows `daemon_smoke`
  (the target is Linux-only), live source mutation and gameplay traversal.
- D17 for M3 waits on the official pinned benchmark run.
