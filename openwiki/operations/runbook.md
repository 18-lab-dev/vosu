---
type: Runbook
title: vosu operations runbook
description: Practical operator notes for building, launching, diagnosing platform access, protecting the local API, handling updates, and understanding release constraints.
resource: README.md
tags: [operations, runbook, security, release, troubleshooting]
---

# Operations runbook

## Documentation maintenance

Update ordinary Markdown in `openwiki/` in the PR that changes the documented behavior. Review only affected contracts and architecture; do not run recurring inference or whole-tree generation. The directory name is retained for stable links, not as an OpenWiki tooling requirement. Preserve dated benchmark and experiment evidence rather than rewriting historical results.

## Launch checklist

- Use Rust 1.96.1 and `--locked` for source builds.
- Run `cargo run --locked` for local development; Linux process-event backends remain enabled by default.
- Run the daemon as the same desktop user as osu!.
- Confirm `127.0.0.1:24050` is reachable before investigating API readiness.
- Check that the client process is supported and that stable/lazer path and offset resolution can complete.
- Keep `OPEN_DASHBOARD_ON_STARTUP` and `ENABLE_AUTOUPDATE` aligned with the intended environment in `vosu.toml`.

A state endpoint returning `500 {"error":"osu is not ready/running"}` means no focused ready instance is currently published; it is not by itself evidence that the server failed.

## Configuration

Edit the platform config root's `vosu.toml`; it is a flat, typed TOML file with uppercase binding names. For example:

```toml
ENABLE_AUTOUPDATE = true
OPEN_DASHBOARD_ON_STARTUP = true
SERVER_IP = "127.0.0.1"
SERVER_PORT = 24050.0
POLL_RATE = 150.0
```

Startup precedence is `vosu.toml > vosu.env > tosu.env > defaults`. The two `.env` files are migration inputs only: vosu writes canonical TOML atomically first and removes readable legacy files only after successful replacement. A malformed TOML file leaves the prior runtime configuration active during watch reload; at startup, malformed input leaves all sources untouched.

The dashboard’s installed cards persist pin state in browser local storage, keep pinned cards first, and support drag reordering. Shift-click a counter URL to open it in a new window, or press `/` outside an editable control to focus search. If catalog retrieval fails, the Available page can show an error while retaining the locally discovered installed count.

## Platform access

Linux stable uses Wine process discovery and native reads; lazer requires an exposed `osu!.deps.json` layout. `VOSU_OSU_PATH` can override path discovery. If ptrace policy blocks same-user reads, use the distribution-appropriate configuration or narrowly grant `cap_sys_ptrace`; do not run the dashboard as root. Windows requires supported native process combinations and MSVC-built releases.

Offset/signature resolution may remain pending and retry while a process is alive. A pending resolver alone does not confirm that a game build is supported.

## Security boundaries

Keep the listener loopback-only unless remote access is deliberate and protected. Read authorization combines allowed peer/network patterns with allowed `Origin`/`Referer` tokens, retaining headerless/opaque overlay reads; a hostname token does not authorize an arbitrary remote peer. Management POSTs additionally require exact same-origin browser provenance (scheme, host, port), not merely a trusted hostname or loopback peer. Settings sockets retain reads but require that same provenance for writes; use the hosted settings page for file-origin overlays. See the README for method cutover and reverse-proxy boundaries. Preserve request/frame limits, root-confined files, escaped reports, HTTPS-only downloads, private/reserved destination rejection, ZIP safety checks, and same-filesystem replacement.

## Updates and releases

Startup updates follow stable releases, verify the selected platform archive’s published SHA-256 digest, stage exactly one root executable, and use a crash-recoverable helper/rollback flow. Set `ENABLE_AUTOUPDATE = false` in `vosu.toml` or pass `--update=false` to disable startup checks; use same-origin `POST /api/runUpdates` for the explicit route. Do not run Windows directly from an archive or OneDrive-controlled directory because safe replacement/rollback may be unavailable.

Release packaging requires exact platform archives containing only the executable and `LICENSE`; release smoke tests exercise an extracted binary. Windows additionally signs and verifies the executable through the release workflow. The updater currently reads latest-release metadata from `shiruviaa/vosu`; confirm this source when diagnosing update failures. See `scripts/package-release.py` and `scripts/verify-windows-release.ps1` for enforcement.

## Diagnostics

1. Check logs and the effective platform config root's `vosu.toml` path.
2. Confirm listener binding and authorization before debugging memory access.
3. Confirm process discovery, permissions, client architecture, and offset resolution.
4. Use focused compatibility/native tests for code changes rather than relying on compilation.
5. For precise input issues, dry-run navigation and autoplay before real injection; interpret capture artifacts using count deltas and receive-gap fields, not event-list presence alone.

The source implementation and [runtime/configuration workflow](../workflows/runtime-and-config.md) are authoritative for recovery ordering. Security-sensitive changes require the [testing guide](../testing/verification.md) and relevant native platform evidence.
