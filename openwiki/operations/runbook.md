---
type: Runbook
title: vosu operations runbook
description: Practical operator notes for building, launching, diagnosing platform access, protecting the local API, handling updates, and understanding release constraints.
resource: README.md
tags: [operations, runbook, security, release, troubleshooting]
---

# Operations runbook

## Launch checklist

- Use Rust 1.96.1 and `--locked` for source builds.
- Run `cargo run --locked --no-default-features` for local development.
- Run the daemon as the same desktop user as osu!.
- Confirm `127.0.0.1:24050` is reachable before investigating API readiness.
- Check that the client process is supported and that stable/lazer path and offset resolution can complete.
- Keep `OPEN_DASHBOARD_ON_STARTUP` and `ENABLE_AUTOUPDATE` aligned with the intended environment.

A state endpoint returning `500 {"error":"osu is not ready/running"}` means no focused ready instance is currently published; it is not by itself evidence that the server failed.

## Platform access

Linux stable uses Wine process discovery and native reads; lazer requires an exposed `osu!.deps.json` layout. `VOSU_OSU_PATH` can override path discovery. If ptrace policy blocks same-user reads, use the distribution-appropriate configuration or narrowly grant `cap_sys_ptrace`; do not run the dashboard as root. Windows requires supported native process combinations and MSVC-built releases.

Offset/signature resolution may remain pending and retry while a process is alive. A pending resolver alone does not confirm that a game build is supported.

## Security boundaries

Keep the listener loopback-only unless remote access is deliberate and protected. Authorization combines allowed peer/network patterns with allowed `Origin`/`Referer` tokens; a hostname token does not authorize an arbitrary remote peer. Preserve exact route/method matching, request and WebSocket message limits, root-confined file serving, escaped report output, HTTPS-only counter downloads, private/reserved destination rejection, ZIP safety checks, and same-filesystem replacement.

## Updates and releases

Startup updates follow stable releases, verify the selected platform archive’s published SHA-256 digest, stage exactly one root executable, and use a crash-recoverable helper/rollback flow. Disable startup checks with `ENABLE_AUTOUPDATE=false` or `--update=false`; use `GET /api/runUpdates` for the explicit route. Do not run Windows directly from an archive or OneDrive-controlled directory because safe replacement/rollback may be unavailable.

Release packaging requires exact platform archives containing only the executable and `LICENSE`; release smoke tests exercise an extracted binary. Windows additionally signs and verifies the executable through the release workflow. See `scripts/package-release.py` and `scripts/verify-windows-release.ps1` for enforcement.

## Diagnostics

1. Check logs and effective configuration paths for the platform.
2. Confirm listener binding and authorization before debugging memory access.
3. Confirm process discovery, permissions, client architecture, and offset resolution.
4. Use focused compatibility/native tests for code changes rather than relying on compilation.
5. For precise input issues, dry-run navigation and autoplay before real injection; interpret capture artifacts using count deltas and receive-gap fields, not event-list presence alone.

The source implementation and [runtime/configuration workflow](../workflows/runtime-and-config.md) are authoritative for recovery ordering. Security-sensitive changes require the [testing guide](../testing/verification.md) and relevant native platform evidence.
