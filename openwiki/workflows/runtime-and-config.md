---
type: Operational Workflow
title: vosu runtime and configuration workflows
description: Startup, platform paths, watched configuration, counter files, listener reload, shutdown, and self-update behavior for operating and modifying vosu.
resource: src/runtime/lifecycle.rs
tags: [workflow, configuration, operations, updater, platform]
---

# Runtime and configuration workflows

## Startup

`App::run` parses compatible startup arguments, captures startup environment, applies update/OneDrive policy, and invokes runtime hooks. `ProductionRuntime::compose` resolves fallible dependencies before starting long-lived services. The daemon normally listens on `127.0.0.1:24050` and can open the dashboard at startup.

Configuration lives in the platform config root's `vosu.toml`, a flat typed TOML document with uppercase binding names. `ConfigStore` watches its parent directory for create/modify events, validates complete documents, publishes immutable values through `ArcSwap` and transition channels, and keeps the last-good runtime configuration when a reload is malformed. Programmatic updates publish immediately; a debounced save writes same-directory temporary bytes, flushes and syncs them, then performs atomic replacement. `register_before_publish` ensures resources such as static roots are replaced before matching configuration becomes observable.

A minimal valid configuration is:
```toml
DEBUG_LOG = false
ENABLE_AUTOUPDATE = true
POLL_RATE = 150.0
PRECISE_DATA_POLL_RATE = 10.0
SERVER_IP = "127.0.0.1"
SERVER_PORT = 24050.0
STATIC_FOLDER_PATH = "./static"
ALLOWED_IPS = "127.0.0.1,localhost,absolute"
```

## Paths and migration

Linux uses XDG config/data/cache roots; Windows uses the executable directory. `src/platform.rs` centralizes these rules. First startup performs idempotent migration from former `tosu` locations without overwriting current state. Configuration migration reads canonical TOML together with readable legacy `vosu.env` and `tosu.env` inputs, with precedence `vosu.toml > vosu.env > tosu.env > defaults`; it writes canonical bytes first, and only successful replacement permits removing legacy dotenv files. If canonical TOML is malformed, initialization leaves all sources in place. After migration, steady-state reads, writes, and watches use only `vosu.toml`. Relative custom static paths resolve from the launch directory, while default static roots map to the platform data root.
Counters are discovered recursively below the static root. `index.html` is the normal entrypoint; `metadata.txt` enriches catalog data; an immediate `ignore.txt` excludes a directory; `node_modules` is never traversed. Each counter package owns its authoritative schema at `<static-root>/<counter>/settings.schema.toml`, including developer schema saves, while mutable values live only at `<config>/settings/<counter>.settings.values.toml`; the root schema always wins. Without one, vosu creates it from root `settings.json` when present, otherwise by inferring scalar root `settings.toml`: strings become `text`, integers/floats become `number`, booleans become `checkbox`, and keys supply `uniqueID`/`title` and scalar defaults. Arrays, nested tables, and datetimes are rejected as ambiguous. Only after package sources does loading fall back to old config `<counter>.settings.toml` or `<counter>.settings.json`; all legacy counter schema and values inputs remain byte-for-byte untouched after canonical schema and values are created. Root JSON and TOML remain untouched, and when both exist JSON defines the schema while TOML may seed values.

Counter static serving resolves a trailing-slash counter path to its `index.html`, redirects a non-slash directory request to the slash form, and returns 404 instead of exposing a directory index when the index is absent. If catalog retrieval fails, the Available page still retains the locally discovered installed count while rendering the catalog error.

## Listener and shutdown

`ListenerRuntime` resolves and binds the effective target when configuration changes. Resource hooks run before a new snapshot is published; a failed reload retains the prior listener rather than dropping a working service. Initial readiness is reported only after binding succeeds. Shutdown first stops new accepts, closes WebSockets, drains listeners, stops commands/relays and state workers, then releases counters, config, and logging. Tokio shutdown is bounded because running blocking calculations cannot always be aborted.

## Self-update

The updater separates release selection, metadata/digest validation, staging, transaction records, platform helper behavior, and startup recovery (`src/updater/`). Production updates select the exact platform archive from `https://api.github.com/repos/shiruviaa/vosu/releases/latest`, require the published SHA-256 digest, stage one root executable, atomically replace the current binary, and retain rollback/crash recovery state. Set `ENABLE_AUTOUPDATE = false` in `vosu.toml` or pass `--update=false` to disable startup checks; `GET /api/runUpdates` starts the explicit path.

Do not bypass the transaction boundary from routes or scripts. If recovery quarantines an ambiguous prior transaction, preserve the evidence and consult the updater implementation/tests; a complete operator escalation procedure remains a documented backlog item.

## Safe operational defaults

Run as the same desktop user as osu!, retain loopback binding unless remote access is intentional, and do not run as root to bypass ptrace policy. On Linux, narrowly scoped `cap_sys_ptrace` may be required for same-user reads. Keep HTTPS-only counter downloads, path containment, archive limits, and authorization checks intact.

For platform and release checks, use the [operations runbook](../operations/runbook.md); for behavior verification, use [testing guidance](../testing/verification.md).
