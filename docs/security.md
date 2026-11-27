# Security, installation, and system behavior

This disclosure describes what the vosu daemon does to the computer it runs on, with links to the code behind each behavior. Network requests are listed in the [privacy policy](../PRIVACY.md). Release signing is covered by the [code signing policy](../CODE_SIGNING_POLICY.md).

## What vosu does

vosu reads the state of a running osu!stable or osu!lazer client from that process's memory and publishes it, with game files and installed overlays, through a local HTTP/WebSocket API and dashboard. It does not inject code into the game, write to the game's memory or files, hook its input or graphics, or automate gameplay. Rendering overlays inside the game is out of scope ([ADR 0008](decisions/0008-external-in-game-overlays.md)).

## Access to the game process

- **Read-only memory access.** Linux reads with `process_vm_readv` ([`crates/vosu-sys/src/process/linux.rs`](../crates/vosu-sys/src/process/linux.rs)); Windows opens the game with `PROCESS_VM_READ | PROCESS_QUERY_INFORMATION` only ([`crates/vosu-sys/src/process/windows.rs`](../crates/vosu-sys/src/process/windows.rs)). vosu has no code path that writes to another process.
- **Same user only.** Linux discovery and opening require the target to run under the daemon's real user ID; Windows compares the primary-token user SIDs. vosu must run as the desktop user who runs osu!, never as root or Administrator.
- **Process discovery.** vosu enumerates processes to find osu! clients and reads their command lines and install paths. Tournament and debug clients are filtered by command line.
- **Lazer offset generation.** When neither the offset cache nor the embedded asset matches a lazer build, vosu extracts its embedded helper payload into the cache and runs two helper processes on the client's own .NET runtime. They measure field offsets from the client's assemblies on disk (`--assemblies-only`) and from a separate helper process; they do not attach to, pause, or modify the running game ([`crates/vosu-osu-reader/src/offset_generator.rs`](../crates/vosu-osu-reader/src/offset_generator.rs)).

## Privileges

### Linux

Same-user memory reads can be blocked by the kernel's Yama ptrace policy. `vosu --setup` grants file capabilities on the vosu executable so it can read without running as root ([`crates/vosu-sys/src/platform/linux/capabilities.rs`](../crates/vosu-sys/src/platform/linux/capabilities.rs)):

- `cap_sys_ptrace,cap_sys_resource,cap_perfmon,cap_bpf=ep` when the eBPF process-event backend works on this kernel;
- otherwise `cap_sys_ptrace=ep` only.

Run as a normal user, setup asks for authentication through polkit `pkexec` before calling `setcap`. If the executable or its directory is writable without root, as with a portable archive, setup warns first, because the prompt authorizes whatever file is at that path. Linux packages run `/usr/bin/vosu --setup` as root after installation and upgrade; a failure there does not fail the package transaction ([`packaging/linux/postinstall.sh`](../packaging/linux/postinstall.sh)).

With those capabilities, vosu watches process starts and exits to notice osu! quickly. It prefers an eBPF program attached to the `sched_process_exec`, `sched_process_fork`, `sched_process_exit`, and `task_rename` raw tracepoints ([`crates/vosu-sys/src/events/bpf/process_events.bpf.c`](../crates/vosu-sys/src/events/bpf/process_events.bpf.c)), falls back to the kernel's process-event connector (CN_PROC) when permitted, and otherwise polls. The eBPF program sees every process start, rename, and exit in the kernel, but reports to vosu only processes whose names match osu! candidates.

### Windows

vosu needs no elevation, installs no driver or service, and writes no registry keys.

## Native plugin execution

A process plugin runs native code with the desktop user's filesystem and
network access. A separate `vosu plugin-host` limits the effect of a crash;
**it is not a general-purpose sandbox**. ABI game-memory reads are brokered
by core against the plugin's authorized session. In-process code has the
daemon's access; only bundled plugins, official or explicitly trusted
sources, and explicit debug development loads are eligible.

- **Linux:** process plugins need kernel 5.4 or later, accessible procfs and
  permission to install the seccomp filter. `--setup` configures the
  daemon's game access, not plugin-host startup. Core sets `no_new_privs`
  before exec so the child gains none of the executable's file capabilities.
  Before opening the library, the child verifies that it is single-threaded,
  has no root UID, has zero inheritable, permitted, effective and ambient
  capabilities, and has `NoNewPrivs: 1` and the selected filter active.
  The bounding set is recorded, not emptied. A failed check refuses the
  load without an automatic retry or in-process fallback.
- **Linux user namespaces:** the default filter prevents creation and
  entry. `user_namespaces = true` in `[runtime.linux]` allows creation on
  Linux only after explicit approval, even for official and trusted
  sources. `setns` stays denied; neither the capability drop nor
  `no_new_privs` is relaxed. The OS may still forbid creation. A helper
  such as Chromium can use its own sandbox; this does not sandbox the
  plugin. Helpers inherit the drop and filter. Core death kills the
  plugin host, not plugin-spawned descendants unless the plugin arranges it.
- **Windows:** core starts the host suspended, assigns it to a dedicated
  non-breakaway `KILL_ON_JOB_CLOSE` job, then resumes it. The child
  inherits no job handle, game handles or credentials. Even abrupt core
  exit closes the job and kills its members, including descendants. A
  job confines lifetime, not the Windows token's privileges.

See the [plugin ABI](v2/plugin-abi.md#7-execution-and-trust) and
[ADR 0025](decisions/0025-linux-plugin-user-namespaces.md). These are
plugin-host policies, not the separately supervised vosulay runtime's policy.

**Approval and restart.** Explicit process approval is available through
**Tools → Plugins** or the reviewed catalog plan, never via CLI or
noninteractive startup. It binds the ID, process runtime, SHA-256 of the
exact manifest and target library, and provenance: a canonical development
package directory, or catalog source, installed version and archive SHA-256.
Development records live in `<config>/plugins/approvals.json`. Installed
records live only in `<data>/catalog/catalog-installed.json`, alongside
the installed graph. These are channel-specific roots; neither record
authorizes the other provenance. Trusted catalog packages that do not need
explicit approval still must match both verified executable digests. An
old archive-only approval needs re-review. Core rechecks before each load
or restart, and the child rehashes the opened library before loading it.

Approval, revocation, stop and manual restart require dashboard-origin
provenance plus `X-Vosu-CSRF`. Revocation stops the process host. An
unexpected exit (even exit zero), sustained ring pressure or other
transport loss counts once per generation. The automatic retry schedule
backs off by 1/2/4/8/16 seconds, but the fifth loss in a rolling five
minutes instead latches `failed`. Manual restart clears the latch, not the
approval gate. Idle silence and controlled stops are not losses. Revoking
trust from running in-process code takes effect at the next daemon restart;
vosu reports it as pending and does not restart the daemon automatically.

## Installation, files, and removal

| Form | What is installed or created |
| --- | --- |
| Portable archive | Only the extracted `vosu`/`vosu.exe` and `LICENSE`. On Windows, configuration, overlays, settings, logs, and caches live next to the executable. |
| Linux packages | `/usr/bin/vosu`, `/usr/share/applications/vosu.desktop`, icons under `/usr/share/icons/hicolor`, and the license under `/usr/share/licenses/vosu` and `/usr/share/doc/vosu` ([`packaging/linux/nfpm.yaml`](../packaging/linux/nfpm.yaml)). Packages ship no systemd unit, service, or repository configuration. |
| All Linux forms | User data under the XDG config, data, and cache roots (`vosu/` in each). |

[Files and migration](../README.md#files-and-migration) lists every path, and [Uninstall](../README.md#uninstall) gives removal steps for each form, including user data.

## Changes outside vosu's own files

- **Start at login** (Linux, off by default): writes a marked systemd user unit at `${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/vosu.service` and enables it for the graphical session. Turning it off disables and deletes only a unit vosu wrote; an unrelated unit with that name is never overwritten ([`src/desktop/login.rs`](../crates/vosu-core/src/desktop/login.rs)).
- **Self-update** (portable and other user-writable installs): replaces the vosu executable in place through a recoverable helper process, after checking the release archive's published SHA-256 digest. Package-managed installs never self-update.
- **Browser and file manager:** vosu opens the dashboard in the default browser at startup (`OPEN_DASHBOARD_ON_STARTUP`, on by default), from the tray, and when it is launched a second time. With the dashboard off (`ENABLE_DASHBOARD = false` or `VOSU_DASHBOARD=off`) it binds no dashboard listener and opens none of these. **Open folder** opens an overlay folder in the file manager.
- **Tray icon:** a StatusNotifierItem on Linux or a notification-area icon on Windows, with **Open dashboard** (absent when the dashboard is off), **Show logs** and **Quit**.
- **Legacy tosu data:** on first start after the rename, vosu moves non-conflicting files from former `tosu` XDG roots on Linux and migrates legacy configuration files once.

## Network exposure

The overlay/data listener remains `127.0.0.1:24050`; the independent dashboard defaults to `127.0.0.1:24051`. Both require allowed peers. Dashboard management requires exact dashboard-origin provenance and a per-start `X-Vosu-CSRF` token injected only into dashboard pages. Overlay-origin management mutations return 403, except scoped API-key and native broker value/profile writes and the unchanged settings command socket. Dashboard pages never serve overlay files; previews use the unchanged overlay origin. All dashboard pages forbid framing. [ADR 0016](decisions/0016-separate-dashboard-origin.md) supersedes ADR 0013's shared-origin decision without moving overlay localStorage/IndexedDB. Native same-account programs can still read dashboard pages and forge provenance; this is not local-user authentication.

Native overlay brokers can authenticate a loopback `/websocket/commands` upgrade
with the per-start `broker.json` bearer token ([integration contract](../README.md#native-overlay-brokers)).
Any same-account process able to read that file can write counter settings and
invoke the other same-origin command-socket writes. This makes the existing
native-Origin-forgery limitation explicit. On HTTP it grants value saves,
profile actions and eligible API v1 CLI actions/jobs, never schema saves,
native plugin approval or other management.
Brokers must keep it out of page JavaScript/logs and authorize their own inbound counter frames.
Linux uses a 0700 directory and atomically created 0600 files.
Windows files inherit their parent directory's ACL, as documented by
[Microsoft](https://learn.microsoft.com/en-us/windows/win32/fileio/file-security-and-access-rights).
The usual per-user LocalAppData location is not a security guarantee: broad
inheritable ACEs on an existing `%LOCALAPPDATA%\vosu` directory or its parents
can let other accounts read the token. vosu does not replace those ACLs;
administrators and other principals already entitled to read remain trusted.
Credential publication errors disable broker writes with a warning rather than
stopping the daemon. vosu never deletes `broker.json`: a stopped daemon's token
is stale and authenticates nothing, and each start atomically replaces the file,
so no shutdown check-then-unlink race exists. The private directory is not a
boundary against another process running as that account.


User-managed [API keys](../README.md#api-keys) grant only explicit overlay-value/
command-write or profile-management scopes. Keys may authenticate permitted remote
peers, never bypass `ALLOWED_IPS`, and cannot authorize daemon/key management,
reports, updates, or counter installation/deletion. Dashboard management requires
exact dashboard-origin provenance and the dashboard's per-start CSRF token. Key hashes and metadata persist in config-root
`api-keys.toml`; secrets are shown once, compared through constant-time SHA-256
hash comparisons, and never logged. Revoke compromised keys immediately; open
command sockets are actively closed with code 1008, even while idle. Plain HTTP/WS on a
LAN exposes bearer credentials: use loopback or a trusted TLS proxy. Browser
clients use the WebSocket subprotocol, never URL query parameters or cross-origin
HTTP Authorization. Installed overlay scripts no longer have dashboard management authority.
HTTP POSTs re-check their granted scope after receiving the body and before
dispatch, so revoking during a slow upload prevents the pending mutation.
On Unix, unsafe registry ownership or group/world access produces a warning
without refusing valid keys; key names reject bidirectional controls.

## Third-party overlays

Overlays are third-party web pages that run in the user's browser or OBS. vosu does not review them. Installed overlays can read game data and use their existing settings command socket, but cannot mutate dashboard management APIs without an explicitly granted scoped API key. The dashboard listener never serves their scripts or static files.

## Hosted tools

Tools are static web pages served on the overlay origin; vosu runs no tool process, and a tool page gains no OS access of its own. Development tools passed with repeatable `--tool <directory>` are trusted for that run. Installed tools come only from reviewed catalog v2 plans (HTTPS destination and redirect checks, package size and SHA-256 verification, hostile ZIP rejection and SDK manifest validation, committed atomically); tools from an untrusted source stay untrusted until explicitly trusted in the dashboard, and only a trusted, enabled tool whose requirements resolve is served or launched. vosu embeds no tools. A 1.x package with only `tool.toml` is never parsed, run or served.

Catalog sources are an ordered, user-editable list of exact source strings; only the fixed identities `18-lab-dev/vosu-tools`, `18-lab-dev/vosulay` and `18-lab-dev/vosu-overlays` are official and always trusted. Other sources start untrusted, and trust, the private-network exception and credentials change only through their own dashboard routes, never through the list (A6b.4). Granting trust shows the exact warning every time; trust makes a source's plugins eligible for in-process loading and never comes from a credential, private permission or package hash. Every installation is a reviewed catalog v2 plan confirmed once; user-trust process plugins also need an explicit approval bound to the exact artifact.

Catalog management is dashboard-only: every `/api/catalog/*` route answers 404 on the overlay listener, and every `POST`/`DELETE` needs same-origin dashboard provenance plus `X-Vosu-CSRF` before any side effect. The native loopback broker credential, API keys and tool tokens authorize no catalog source, credential, trust, install or uninstall action; the former broker-authenticated `/tools/_host/v1/cli/catalog/` routes are removed. Source credentials are write-only (`204`, presence only on read) and never appear in responses, errors or status.

Tool routes live only on the overlay listener under `/tools/`, each by exact method and path or one per-tool prefix; the dashboard never serves their scripts, and `/tools/<id>/api/…` is always 404 and never falls through to a static file. A tool's art is served with a sandboxing Content-Security-Policy, and its `_settings` read never includes `password` values. Reads, subscriptions and resources are public under the overlay peer policy, for tools as for overlays. Only plugin actions need authority: a dashboard launch opens an opener-less tab with a single-use, 60-second, 256-bit code in the URL fragment; the bridge removes the fragment before any asynchronous work and keeps the exchanged session only in that document's memory, never in cookies, same-origin storage or a URL, so an overlay later loaded in the same tab cannot recover it and a reload or bookmark is read-only. The session is sent only as `X-Vosu-Tool-Session` on the API v1 action and job requests the bridge builds itself, is compared in constant time, and authorizes only the actions the tool's `requires`/`optional` lists and an installed provider resolves, rechecked on every request; a tab can read and cancel only its own jobs. Disabling, untrusting, uninstalling or replacing a tool, or a restart, ends its sessions and requests cancellation of their jobs. Sharing an origin with overlays is not an opaque-origin sandbox: a same-origin window or frame that the tool UI itself opens without `noopener` (or embeds) can script the tool document and use its session, so tool UIs must not open or frame overlay pages that way. Plugins behind actions run native code under their own trust and approval. See the complete [protocol and trust contract](tools/protocol.md).


## Analytics and crash reporting

vosu has no telemetry, analytics, or crash reporting. Diagnostic reports are generated locally on request and never uploaded.
