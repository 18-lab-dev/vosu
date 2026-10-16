# Security, installation, and system behavior

This disclosure describes what the vosu daemon does to the computer it runs on, with links to the code behind each behavior. Network requests are listed in the [privacy policy](../PRIVACY.md). Release signing is covered by the [code signing policy](../CODE_SIGNING_POLICY.md).

## What vosu does

vosu reads the state of a running osu!stable or osu!lazer client from that process's memory and publishes it, with game files and installed overlays, through a local HTTP/WebSocket API and dashboard. It does not inject code into the game, write to the game's memory or files, hook its input or graphics, or automate gameplay. Rendering overlays inside the game is out of scope ([ADR 0008](decisions/0008-external-in-game-overlays.md)).

## Access to the game process

- **Read-only memory access.** Linux reads with `process_vm_readv` ([`src/process/linux.rs`](../src/process/linux.rs)); Windows opens the game with `PROCESS_VM_READ | PROCESS_QUERY_INFORMATION` only ([`src/process/windows.rs`](../src/process/windows.rs)). vosu has no code path that writes to another process.
- **Same user only.** Linux discovery and opening require the target to run under the daemon's real user ID; Windows compares the primary-token user SIDs. vosu must run as the desktop user who runs osu!, never as root or Administrator.
- **Process discovery.** vosu enumerates processes to find osu! clients and reads their command lines and install paths. Tournament and debug clients are filtered by command line.
- **Lazer offset generation.** When neither the offset cache nor the embedded asset matches a lazer build, vosu extracts its embedded helper payload into the cache and runs two helper processes on the client's own .NET runtime. They measure field offsets from the client's assemblies on disk (`--assemblies-only`) and from a separate helper process; they do not attach to, pause, or modify the running game ([`src/runtime/offset_generator.rs`](../src/runtime/offset_generator.rs)).

## Privileges

### Linux

Same-user memory reads can be blocked by the kernel's Yama ptrace policy. `vosu --setup` grants file capabilities on the vosu executable so it can read without running as root ([`src/platform/linux/capabilities.rs`](../src/platform/linux/capabilities.rs)):

- `cap_sys_ptrace,cap_sys_resource,cap_perfmon,cap_bpf=ep` when the eBPF process-event backend works on this kernel;
- otherwise `cap_sys_ptrace=ep` only.

Run as a normal user, setup asks for authentication through polkit `pkexec` before calling `setcap`. If the executable or its directory is writable without root, as with a portable archive, setup warns first, because the prompt authorizes whatever file is at that path. Linux packages run `/usr/bin/vosu --setup` as root after installation and upgrade; a failure there does not fail the package transaction ([`packaging/linux/postinstall.sh`](../packaging/linux/postinstall.sh)).

With those capabilities, vosu watches process starts and exits to notice osu! quickly. It prefers an eBPF program attached to the `sched_process_exec`, `sched_process_fork`, `sched_process_exit`, and `task_rename` raw tracepoints ([`src/instance/events/bpf/process_events.bpf.c`](../src/instance/events/bpf/process_events.bpf.c)), falls back to the kernel's process-event connector (CN_PROC) when permitted, and otherwise polls. The eBPF program sees every process start, rename, and exit in the kernel, but reports to vosu only processes whose names match osu! candidates.

### Windows

vosu needs no elevation, installs no driver or service, and writes no registry keys.

## Installation, files, and removal

| Form | What is installed or created |
| --- | --- |
| Portable archive | Only the extracted `vosu`/`vosu.exe` and `LICENSE`. On Windows, configuration, overlays, settings, logs, and caches live next to the executable. |
| Linux packages | `/usr/bin/vosu`, `/usr/share/applications/vosu.desktop`, icons under `/usr/share/icons/hicolor`, and the license under `/usr/share/licenses/vosu` and `/usr/share/doc/vosu` ([`packaging/linux/nfpm.yaml`](../packaging/linux/nfpm.yaml)). Packages ship no systemd unit, service, or repository configuration. |
| All Linux forms | User data under the XDG config, data, and cache roots (`vosu/` in each). |

[Files and migration](../README.md#files-and-migration) lists every path, and [Uninstall](../README.md#uninstall) gives removal steps for each form, including user data.

## Changes outside vosu's own files

- **Start at login** (Linux, off by default): writes a marked systemd user unit at `${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/vosu.service` and enables it for the graphical session. Turning it off disables and deletes only a unit vosu wrote; an unrelated unit with that name is never overwritten ([`src/desktop/login.rs`](../src/desktop/login.rs)).
- **Self-update** (portable and other user-writable installs): replaces the vosu executable in place through a recoverable helper process, after checking the release archive's published SHA-256 digest. Package-managed installs never self-update.
- **Browser and file manager:** vosu opens the dashboard in the default browser at startup (`OPEN_DASHBOARD_ON_STARTUP`, on by default), from the tray, and when it is launched a second time. **Open folder** opens an overlay folder in the file manager.
- **Tray icon:** a StatusNotifierItem on Linux or a notification-area icon on Windows, with **Open dashboard** and **Quit**.
- **Legacy tosu data:** on first start after the rename, vosu moves non-conflicting files from former `tosu` XDG roots on Linux and migrates legacy configuration files once.

## Network exposure

The listener is `127.0.0.1:24050` by default. Anything on the computer that can connect can read game state, and browser pages from other origins are rejected unless allowed by `ALLOWED_IPS`. Management actions (installing, deleting, or opening overlays, changing settings, running updates) additionally require same-origin browser provenance. That check stops other websites, but not installed overlays, which share the dashboard's origin ([ADR 0013](decisions/0013-counters-share-dashboard-origin.md)), and not native programs, which can forge the header. Exposing the listener beyond loopback is an explicit configuration choice; the traffic is unencrypted and unauthenticated. The [README](../README.md#pages-counters-reports-and-updates) has the full authorization rules.

Native overlay brokers can authenticate a loopback `/websocket/commands` upgrade
with the per-start `broker.json` bearer token ([integration contract](../README.md#native-overlay-brokers)).
Any same-account process able to read that file can write counter settings and
invoke the other same-origin command-socket writes. This makes the existing
native-Origin-forgery limitation explicit; it is not an additional HTTP
management credential and grants no authority on HTTP routes. Brokers must keep
it out of page JavaScript/logs and authorize their own inbound counter frames.
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


## Third-party overlays

Overlays are third-party web pages that run in the user's browser or OBS. vosu does not review them. Installed overlays can read all game data the API exposes and use the dashboard's management actions. The **Available** page states this above the catalog listing.

## Analytics and crash reporting

vosu has no telemetry, analytics, or crash reporting. Diagnostic reports are generated locally on request and never uploaded.
