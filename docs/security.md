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

The overlay/data listener remains `127.0.0.1:24050`; the independent dashboard defaults to `127.0.0.1:24051`. Both require allowed peers. Dashboard management requires exact dashboard-origin provenance and a per-start `X-Vosu-CSRF` token injected only into dashboard pages. Overlay-origin management mutations return 403, except scoped API-key and native broker value/profile writes and the unchanged settings command socket. Dashboard pages never serve overlay files; previews use the unchanged overlay origin. All dashboard pages forbid framing. [ADR 0016](decisions/0016-separate-dashboard-origin.md) supersedes ADR 0013's shared-origin decision without moving overlay localStorage/IndexedDB. Native same-account programs can still read dashboard pages and forge provenance; this is not local-user authentication.

Native overlay brokers can authenticate a loopback `/websocket/commands` upgrade
with the per-start `broker.json` bearer token ([integration contract](../README.md#native-overlay-brokers)).
Any same-account process able to read that file can write counter settings and
invoke the other same-origin command-socket writes. This makes the existing
native-Origin-forgery limitation explicit. On HTTP it grants value saves,
profile actions, and broker-authenticated tool CLI listing, launch, open,
and session revocation; never schema saves or other management.
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

Tools are programs, not sandboxed overlays. Development tools passed with repeatable `--tool <directory>` are trusted for that run. Official catalog tools are trusted after HTTPS destination/redirect checks, package size and SHA-256 verification, hostile ZIP rejection, and manifest identity validation; installation swaps a verified staging directory atomically. vosu embeds no tools. User tools under the data directory's `tools/<id>` remain untrusted until explicitly approved in the dashboard. Approval allows a process to run with your OS account's rights. The `context` host permission controls disclosure of selected-instance absolute game and skin paths; `songs:write` declares filesystem intent, not an OS sandbox.

Catalog sources are an ordered, user-editable list; only the exact default identities `18-lab-dev/vosu-tools` and `18-lab-dev/vosulay` grant automatic tool trust. Removing and re-adding the identical shorthand keeps that identity; ordering and URL equivalence do not confer trust. A custom repository, differently cased spelling, pinned tag, or HTTPS source URL installs tools as `user`, untrusted until approved, even if it names the same repository. Earlier sources win id collisions and later entries are explicitly shadowed. Overlays from sources have no trust gate; the dashboard keeps the third-party JavaScript disclosure and asks before installing their tool requirements. Sources can be removed or disabled without deleting existing installations.

The native loopback broker credential also authorizes tool catalog install/update/uninstall jobs and source listing under `/tools/_host/v1/cli/catalog/`. It does not authorize editing catalog sources or managing overlay packages; those remain dashboard-CSRF actions. Installing a user-source tool through the broker does not trust it.

Tool UI and API routes live only on the overlay listener under `/tools/`; the dashboard never serves their scripts. API prefixes are matched on segment boundaries. GET/HEAD and WebSocket routes declared `read` are public to allowed overlay peers; mutating methods require a matching `write` prefix and a tool-bound UI session or the per-launch token of a running declared dependent. A dashboard launch opens an opener-less tab with a single-use 60-second code in the URL fragment; the browser bridge strips the fragment and keeps the exchanged session only in that document's memory, never in same-origin storage, so an overlay later loaded in the same tab cannot recover it and a reload returns the UI to read-only. Sessions expire when vosu exits. Sharing an origin with overlays is not an opaque-origin sandbox: a same-origin window or frame that the tool UI itself opens without `noopener` (or embeds) can script the tool document and use its session, so tool UIs must not open or frame overlay pages that way.

Every supervised service receives a fresh 256-bit OS-random token. Proxy requests discard cookies, hop-by-hop headers, and caller-supplied `X-Vosu-*` headers before adding the service token and caller identity; bodies are bounded to 1 MiB and responses stream. Services must authenticate their loopback port themselves (the SDK does so). Host and session tokens use constant-time comparisons and are never intentionally exposed in logs or browser metadata. Closing vosu closes child stdin, waits five seconds, then kills remaining processes; Linux parent-death signals and Windows Job Objects provide a daemon-death backstop. See the complete [protocol and trust contract](tools/protocol.md).

CLI processes receive a separate, tool-bound 256-bit secret, not the native
broker credential. At most 64 sessions coexist; each expires after 24 hours,
daemon shutdown, tool replacement, explicit revocation, or disable. The launcher
revokes its session after child exit (including spawn failure). CLI tokens have
the tool's permission-filtered host access plus read/write access to its own
declared API and declared dependencies, never unrelated tools. Forwarded requests
identify `X-Vosu-Caller: cli:<id>` and authenticate with the service's token.
CLI processes run with the user's OS rights and inherited terminal environment;
these grants are not a sandbox.


## Analytics and crash reporting

vosu has no telemetry, analytics, or crash reporting. Diagnostic reports are generated locally on request and never uploaded.
