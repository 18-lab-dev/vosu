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

Configuration lives in `vosu.env`. `ConfigStore` watches it, validates changes, publishes immutable values through `ArcSwap` and transition channels, and writes updates through same-directory temporary files followed by atomic replacement. `register_before_publish` ensures resources such as static roots are replaced before matching configuration becomes observable.

## Paths and migration

Linux uses XDG config/data/cache roots; Windows uses the executable directory. `src/platform.rs` centralizes these rules. First startup performs an idempotent migration from former `tosu` locations without overwriting current state; after migration normal reads and writes use only `vosu` paths. Relative custom static paths are resolved from the launch directory, while default static roots map to the platform data root.

Counters are discovered recursively below the static root. `index.html` is the normal entrypoint; `metadata.txt` enriches catalog data; immediate `ignore.txt` excludes a directory; `node_modules` is never traversed. Counter values belong in central settings paths, not mutable source assets.

## Listener and shutdown

`ListenerRuntime` resolves and binds the effective target. A failed reload retains the prior listener rather than dropping a working service. Initial readiness is reported only after binding succeeds. Shutdown first stops new accepts, closes WebSockets, drains listeners, stops commands/relays and state workers, then releases counters, config, and logging. Tokio shutdown is bounded because running blocking calculations cannot always be aborted.

## Self-update

The updater separates release selection, metadata/digest validation, staging, transaction records, platform helper behavior, and startup recovery (`src/updater/`). Production updates select the exact platform archive, require the published SHA-256 digest, stage one root executable, atomically replace the current binary, and retain rollback/crash recovery state. `ENABLE_AUTOUPDATE=false` or `--update=false` disables startup checks; `GET /api/runUpdates` starts the explicit path.

Do not bypass the transaction boundary from routes or scripts. If recovery quarantines an ambiguous prior transaction, preserve the evidence and consult the updater implementation/tests; a complete operator escalation procedure remains a documented backlog item.

## Safe operational defaults

Run as the same desktop user as osu!, retain loopback binding unless remote access is intentional, and do not run as root to bypass ptrace policy. On Linux, narrowly scoped `cap_sys_ptrace` may be required for same-user reads. Keep HTTPS-only counter downloads, path containment, archive limits, and authorization checks intact.

For platform and release checks, use the [operations runbook](../operations/runbook.md); for behavior verification, use [testing guidance](../testing/verification.md).
