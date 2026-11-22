---
type: Contract
title: "Plugin ABI v1"
description: "Implementer-grade contract for the vosu 2.0 plugin binary interface, execution model, and host services."
---

# Plugin ABI v1

This is the implementer contract for [ADR 0019](../decisions/0019-plugin-runtime-and-trust.md): the versioned C ABI plugins implement, the message protocol between a plugin and host services, and how the host executes a plugin in-process or inside `vosu plugin-host`. The reference C declarations are [`include/vosu_plugin.h`](../../include/vosu_plugin.h); keep both in sync. Capability fields (`provides`/`requires`/`optional`) are defined in [`docs/v2/catalog-v2.md`](catalog-v2.md); field semantics are defined in [`docs/v2/state-schema.md`](state-schema.md). The shared naming contract (plugin ids, capability ids, namespaces, sessions) is normative for every identifier in this document.

## 1. Entry points and ABI version negotiation

A plugin is a shared library (`.so` on `x86_64-unknown-linux-gnu`, `.dll` on `x86_64-pc-windows-msvc`) that exports exactly one C symbol:

```c
const vosu_plugin_vtable *vosu_plugin_entry_v1(void);
```

The `v1` in the symbol name is the **ABI major version**. A host built against major version 1 looks up `vosu_plugin_entry_v1` and nothing else; it never loads a library that does not export that symbol, and it never calls a function whose struct layout it cannot interpret. A breaking ABI change (removing a field, changing a field's type or meaning, changing calling convention) bumps the major version and the symbol name (`vosu_plugin_entry_v2`); hosts may support more than one major version during a migration window by looking up each in turn.

Additive, backward-compatible growth (new optional functions, new opcodes) is a **minor** revision and keeps the same symbol name. Both `vosu_plugin_vtable` and `vosu_host_services` begin with:

```c
uint32_t struct_size; /* sizeof(the struct) as built by the producer */
uint32_t abi_version; /* must equal VOSU_PLUGIN_ABI_MAJOR */
```

A reader (host reading the plugin's vtable, or plugin reading `vosu_host_services`) must never touch a field beyond `struct_size` bytes from the struct's start. Fields appended by a newer minor revision are therefore safe to add: an older peer's `struct_size` simply excludes them, and a producer that predates a given function pointer never had it to set, so the newer consumer must treat any function pointer past the reported `struct_size` as absent (equivalent to NULL) rather than reading uninitialized memory. The host rejects a plugin whose `abi_version` does not equal the major version implied by the symbol it looked up (defends against a stale build that forgot to update the symbol) and whose `struct_size` is smaller than the minimum valid size for that major version (the four required functions: `create`, `dispatch`, `set_message_callback`, `destroy`).

## 2. Plugin descriptor and symbol naming

A plugin's identity, version, and runtime declaration live in its manifest (`plugin.toml`, §10), not in the ABI itself — the vtable carries no name or id. Core correlates a loaded vtable with its manifest by load order: it loads the library named in the manifest's per-target `library` path and calls that library's single entry point; a library must not be shared by two manifests.

**Bundled first-party plugins** (the dashboard, overlay hosting, tool hosting, the osu! plugin, and the tosu v2 / gosumemory v1 / StreamCompanion adapters) are compiled into the `vosu` executable and never export a dynamic symbol. The Rust SDK's registration macro (§11) instead appends each bundled plugin's `vosu_plugin_vtable` to a compile-time-collected static registry (a linker-collected slice, so no runtime registration call is required and no bundled plugin can be accidentally omitted). At startup, core iterates that registry exactly as it iterates `dlopen`ed external plugins: same manifest validation, same `create`/`dispatch`/`destroy` calls, same trust table (§9.1) — bundled plugins are simply always eligible for `in-process` runtime, since their code already runs in the daemon's address space regardless of any grant. A bundled plugin's `plugin.toml` omits `[targets]` entirely (there is nothing to locate on disk) and must not request `runtime = "process"`; the host rejects such a manifest as invalid at startup, not silently downgrades it.

## 3. Message envelope and encoding

Every call across the plugin/host vtable boundary — `vosu_host_services::send` (plugin → host) and `vosu_plugin_vtable::dispatch` plus the registered callback (host → plugin) — carries one `vosu_message`:

```c
typedef struct vosu_message {
    uint32_t opcode;
    uint32_t flags;
    uint64_t correlation_id; /* 0 when the opcode has no reply */
    vosu_bytes payload;
} vosu_message;
```

**Encoding choice.** Two payload shapes coexist, selected by opcode:

- **Fixed C layouts** for the three families on the measured hot path: state publication (`VOSU_OP_PUBLISH_FIELD`), demand signals (`VOSU_OP_DEMAND_CHANGED`), and memory read plans (`VOSU_OP_READ_PLAN_SUBMIT` / `VOSU_OP_READ_PLAN_RESULT`). These opcodes can fire many times per second per session. Their payload bytes are the corresponding `#[repr(C)]`/`struct` value reinterpreted as a byte slice — there is no serialize or parse step, so building a publish or a read plan costs a value construction, not an encoding pass, and the cost is identical whether the call is a direct in-process vtable invocation or (for `process`-runtime plugins) relayed over the `vosu plugin-host` IPC channel, where the same bytes are copied rather than re-encoded. Variable-length data (namespace/field-path strings, read-plan steps, result bytes) is referenced by ordinary borrowed `vosu_bytes`/pointer+count pairs, valid only for the call's duration.
- **CBOR** ([RFC 8949](https://www.rfc-editor.org/rfc/rfc8949)) for every other opcode: session lifecycle, scanning, resource resolution, actions, settings, logging, and timers. These are low-frequency (session attach/detach, a user clicking an action, a settings change, a handful of log lines) or already latency-insensitive (a signature scan). CBOR was chosen over JSON because it is binary and does not need quoting/escaping or text-number round-trip concerns, and over a bespoke binary format because its data model (maps, arrays, strings, byte strings, and numbers) is exactly the JSON Schema model the published namespace and settings schemas already use ([`docs/v2/state-schema.md`](state-schema.md)): a CBOR action payload or settings object re-encodes to JSON for an HTTP response with no semantic translation, and every mainstream language has a mature CBOR library, which matters because plugin-host relays this encoding over IPC regardless of the plugin's implementation language.

No field in any payload is ever a raw pointer into the host's or the plugin's address space. A target-process address (an absolute address from a scan result, or the `address`/`module_offset` fields of a read-plan base) is an opaque `uint64_t` meaningful only as an argument back into a read plan; neither side dereferences it directly, even an in-process, fully trusted plugin — it always goes through `VOSU_OP_READ_PLAN_SUBMIT`. This is what keeps the plugin binary's contract identical whether the host runs it in-process or inside `vosu plugin-host` (§9.2): the vtable call shape never depends on which process holds the real `ProcessMemory` handle.

## 4. Threading model and blocking

The host serializes calls into one plugin instance (`dispatch`, the registered message callback) onto one thread per instance by default, matching the existing single-worker-per-instance model. A plugin's manifest may declare `concurrent_dispatch = true` (§10) to opt into concurrent calls, which requires its `dispatch` and callback implementations to be internally synchronized; the host then dispatches from its own worker pool instead of one dedicated thread.

- `dispatch` may block. For an in-process plugin this runs on a host worker thread dedicated to that instance (never the state-bus publisher thread, so a slow plugin cannot stall [ADR 0002](../decisions/0002-immutable-latest-only-snapshots.md)'s publication); for a `process`-runtime plugin it runs inside `vosu plugin-host`, which has no shared state with core to stall.
- The registered **asynchronous callback** (deliveries the host makes to the plugin outside of a `dispatch` call it is still inside — read-plan results arriving after submission, and timer fires) must return within a small, fixed budget (1 ms) because, for an in-process plugin, it may be invoked from the host's own read/publish worker. Slower work triggered by a callback must be deferred to the plugin's own thread and reported back asynchronously (another publish, another action result), never performed inline.
- `create` may block briefly (it runs before the plugin is considered loaded) but must not perform unbounded I/O; a plugin needing slow first-run setup starts it in its own thread from `create` and reports readiness through normal state publication.
- `destroy` must return within the host's configured shutdown grace period (matching the 5-second tool-service shutdown precedent in [`docs/tools/protocol.md`](../tools/protocol.md)) or the host proceeds without it: for a `process`-runtime plugin, `vosu plugin-host` is killed; an in-process plugin that does not return leaves the daemon process exit blocked, which is an accepted cost of the trust an `in-process` grant represents.

## 5. Error model

Every `dispatch`/`send` call returns a `vosu_status`. `VOSU_OK` means the message was accepted for processing, not that any requested effect already completed — opcodes with a result (read plans, scans, actions) report that result asynchronously through the message callback, correlated by `correlation_id` (or, for read plans, by `vosu_plan_id`, since one submission's results may legitimately return out of order relative to a later submission under `concurrent_dispatch`). A non-`VOSU_OK` status means the call itself was rejected and produces no later message: `VOSU_ERR_INVALID_ARGUMENT` for a malformed payload, `VOSU_ERR_PERMISSION_DENIED` for an opcode the plugin's trust level cannot use (for example a `process`-runtime plugin's read plan touching a session it did not match), `VOSU_ERR_NOT_READY` when the target session has no live process, `VOSU_ERR_TARGET_GONE` when a session died between submission and execution, `VOSU_ERR_UNSUPPORTED` for an opcode the receiver's minor version predates, and `VOSU_ERR_BACKEND` for an underlying OS failure. Per-read failure inside an otherwise-accepted plan is reported per read (`vosu_read_result_kind`, mirroring `src/process.rs`'s `MemoryError`/`ReadStatus` vocabulary) rather than failing the whole plan.

## 6. Host services

### 6.1 Process matching

A game plugin declares, in its manifest `[match]` table (§10), the process matchers core uses to recognize candidate instances for [ADR 0001](../decisions/0001-single-daemon-multi-instance-lifecycle.md) discovery: `process_names` (exact executable file names), `process_path_patterns` (regular expressions matched against the full executable path), and `command_line_patterns` (regular expressions matched against the joined argv). A matched process becomes a candidate instance exactly as 1.x client detection does; acceptance, generation assignment, and PID-order retention are unchanged core behavior, now parameterized by which plugin's matchers accepted the process. Two plugins must not declare overlapping matchers for the same executable; the host refuses to start the second with `VOSU_ERR_INVALID_ARGUMENT` surfaced at manifest validation, not at runtime.

### 6.2 Session lifecycle

A **session** (naming contract) is one attached client; its lifecycle, generation, and selection semantics are exactly [ADR 0001](../decisions/0001-single-daemon-multi-instance-lifecycle.md)'s, generalized across game plugins by [ADR 0020](../decisions/0020-game-agnostic-data-model.md). The plugin never opens or closes a session itself — core owns discovery and sends:

- `VOSU_OP_SESSION_ATTACHED` (CBOR): `{"session": string, "generation": u64, "pid": u32, "bitness": "x86"|"x64", "focused": bool}`. The plugin may now publish `vosu.client` (required) and begin reading.
- `VOSU_OP_SESSION_DETACHED` (CBOR): `{"session": string, "reason": "exited"|"lost"|"superseded"}`. The plugin must stop reading that session; any read plan it still submits for that session answers `VOSU_ERR_TARGET_GONE`.

`bitness` tells the plugin which pointer width `vosu_plan_step::deref` uses for that session; it never issues raw syscalls itself to discover it.

### 6.3 Memory read plans

A read plan (`vosu_read_plan_submit`, §4 of the header) batches every memory read a plugin needs into one message: each entry is a base (an absolute address or a module name plus offset, resolved by core against that session's module list so the plugin never computes an ASLR-adjusted address itself) followed by an ordered list of `(offset, deref)` steps, and a final read length. Core executes the whole plan against the exact process that plugin's matcher accepted — never another session's process, even if the plugin names an arbitrary session id — and answers with one `VOSU_OP_READ_PLAN_RESULT` whose `results` are parallel to the request: **one submission is one round trip**, regardless of how many reads it contains. This is the shape [`crates/vosu-bench`](../../src/process.rs)'s `BrokeredReadBench` (branch `v2/bench`) measures: the same `vosu_read_plan_submit`/`vosu_read_plan_result` pair executes unchanged whether the plugin runs in-process (core's own `ProcessMemory::read_many` answers it directly on the calling thread) or is brokered (the pair is relayed, unmodified as bytes, to and from `vosu plugin-host` over the IPC channel in §9.2). Per-read outcomes mirror `src/process.rs`'s existing `ReadStatus`/`MemoryError` vocabulary (`VOSU_READ_OK`, `VOSU_READ_NOT_READY`, `VOSU_READ_UNSUPPORTED`, `VOSU_READ_ERROR`) so a game plugin's retry/backoff logic matches today's in-tree readers.

### 6.4 Signature scanning and module enumeration

`VOSU_OP_SCAN_REQUEST` (CBOR): `{"session": string, "module": string|null, "pattern": string, "non_zero_mask": bool, "all_matches": bool}`. `pattern` uses exactly `src/process/scanner.rs`'s `Pattern::parse` grammar (space-separated hex byte pairs and `??` wildcards). `module` scopes the scan to one module's readable regions (by core's `readable_regions`); `null` scans every readable region of the process. `VOSU_OP_SCAN_RESULT` (CBOR): `{"addresses": [u64, ...]}` — absolute target-process addresses, suitable as a later read plan's `VOSU_BASE_ABSOLUTE`. Scans are not on the measured hot path and are not batched with reads; a plugin that scans every tick instead of caching the result against a module's load address is a plugin bug, not an ABI limitation.

### 6.5 Declared file roots and resource resolution

Resources follow [ADR 0021](../decisions/0021-api-v1-and-upstream-adapters.md): a plugin declares the resource kinds it fills (`chart.file`, `chart.audio`, `chart.background`, `chart.set`, `skin`, or `resource:<plugin-id>.<kind>` for its own kinds) in its manifest, and never serves HTTP itself. When state publication references a resource or tree, the plugin resolves that reference through `VOSU_OP_RESOURCE_RESOLVE` (CBOR request from core, CBOR reply from the plugin — the plugin is the callee here since only it knows where its data lives):

- Request: `{"kind": string, "key": <plugin-defined CBOR value identifying the item>}`.
- Reply: either `{"root": string, "path": string}` (a previously declared file root and a relative path inside it, which core opens with [ADR 0005](../decisions/0005-root-confined-file-serving.md)'s retained-handle confinement) or `{"stream": <resource_id>}` naming a bounded byte stream the plugin serves through further `VOSU_OP_RESOURCE_RESOLVE`-scoped read calls (for generated content, and mandatory for `process`-runtime plugins, which never hand core a filesystem root they might not even share visibility into). Declared roots are listed once, by absolute path, in the manifest's `[resources]` table (§10); core refuses a `path` reply that escapes a declared root rather than trusting the plugin's confinement.

### 6.6 State publication to namespaces

A plugin publishes typed values with `VOSU_OP_PUBLISH_FIELD` (§3's fixed layout): a session, a namespace (e.g. `vosu.osu`), a dotted field path below it (e.g. `pp.current`), and one `vosu_value`. Together, `ns` and `field_path` name the capability id `field:<ns>.<field_path>` from the naming contract. Core applies [ADR 0002](../decisions/0002-immutable-latest-only-snapshots.md)'s immutable, latest-only publication per session: a plugin publishes whenever a value changes (or at its own sampling cadence while demand exists, §6.7); it never assembles or sends a whole namespace document, and never serializes to JSON itself — projection into each served API is core's job, driven by the JSON Schema each namespace publishes ([`docs/v2/state-schema.md`](state-schema.md)). A field the plugin's manifest does not declare as filled must never be published; one it declares but genuinely cannot read for the current session is published as `VOSU_VALUE_NULL`, never a placeholder zero or empty value ([ADR 0020](../decisions/0020-game-agnostic-data-model.md)).

### 6.7 Demand signals

Core tracks, per namespace and field, whether any subscriber (an HTTP/WebSocket client, an action, another plugin) currently needs it, exactly as 1.x's selected-field subscription hub does. `VOSU_OP_DEMAND_CHANGED` (§3's fixed layout) tells the plugin when that changes: `subscribed: true` for a field or whole namespace means the plugin should begin or continue sampling it; `false` means it may stop. A plugin samples a high-frequency value only while subscribed demand exists for it; this is advisory in the sense that over-publishing is never rejected, but a plugin that samples unconditionally defeats the purpose of this signal and should not.

### 6.8 Actions

Tools invoke a plugin action by its capability id (`action:<plugin-id>.<name>`, naming contract) through core, which forwards `VOSU_OP_ACTION_INVOKE` (CBOR): `{"invocation": vosu_invocation_id-as-u64, "args": <CBOR map>}`. The plugin eventually answers (asynchronously, through the message callback, not as `dispatch`'s return value) with `VOSU_OP_ACTION_RESULT` (CBOR): `{"invocation": u64, "status": "ok"|"error", "value": <CBOR>|null, "code": string|null, "message": string|null}`. Core may request early termination with `VOSU_OP_ACTION_CANCEL` (CBOR `{"invocation": u64}`); a plugin that supports cancellation stops promptly and still sends exactly one `VOSU_OP_ACTION_RESULT` with `"status": "error", "code": "cancelled"`. A plugin that ignores cancellation simply runs to completion — core does not forcibly interrupt an in-process plugin's action (it cannot safely do so) and, for a `process`-runtime plugin, only kills `vosu plugin-host` as a last resort after a configurable timeout, which also ends every other in-flight call to that plugin.

### 6.9 Settings

A plugin declares its settings shape with `settings_schema` (§10), a path to a `settings.schema.json` sibling using exactly the overlay/tool `settings.json` schema format ([`docs/tools/protocol.md`](../tools/protocol.md)). Core owns storage, the dashboard editing UI, and `password`-type write-only handling; it delivers the effective settings object to the plugin at `create` time (as part of the CBOR-encoded `host_ctx` handshake payload core passes before calling `create`, not a separate opcode) and on every change with `VOSU_OP_SETTINGS_CHANGED` (CBOR): `{"values": <CBOR map>}`.

### 6.10 Logging

`VOSU_OP_LOG_RECORD` (CBOR, plugin → host, no reply): `{"level": "trace"|"debug"|"info"|"warn"|"error", "message": string, "fields": <CBOR map>}`. Core attaches its own structured context (plugin id, session, timestamp) and writes to the daemon's existing logging sink. Records are rate-limited per plugin instance (200/second, matching the host's existing bounded-queue discipline); records past the limit are dropped and counted, with the count itself logged once per second while dropping continues, so a runaway plugin cannot grow host memory or CPU use without bound.

### 6.11 Timers

A plugin requests periodic or one-shot callbacks instead of spawning its own OS timer thread, so the host can account for and suspend a plugin's scheduled work exactly as it does reads: `VOSU_OP_TIMER_SET` (CBOR, plugin → host): `{"timer": vosu_timer_id-as-u64, "interval_ms": u32|null}` (`null` requests a one-shot fire after the host's minimum granularity). The host answers each due timer with `VOSU_OP_TIMER_FIRED` (CBOR): `{"timer": u64}`. `VOSU_OP_TIMER_CANCEL` (CBOR, plugin → host): `{"timer": u64}` stops a periodic timer; a cancelled one-shot timer that already fired is a no-op.

## 7. Execution and trust

### 7.1 Runtime request and the host's decision

A manifest requests `runtime = "in-process"` or `runtime = "process"` (§10). The host grants exactly the table in [ADR 0019](../decisions/0019-plugin-runtime-and-trust.md#execution-and-trust): `in-process` only for bundled plugins, plugins from the official sources, or a source the user has explicitly marked trusted ([ADR 0023](../decisions/0023-authenticated-catalog-sources.md)); every other plugin may request `in-process` but is refused outright (`VOSU_ERR_PERMISSION_DENIED` at load time, logged, plugin left unloaded) rather than silently downgraded to `process` — a manifest cannot use its own runtime request to bypass the trust decision. `runtime = "process"` is always available, for any plugin, after the same user-approval flow 1.x applies to user tools.

### 7.2 The `vosu plugin-host` process protocol

For a `process`-runtime grant, core spawns `vosu plugin-host` once per plugin instance (not shared across plugins, so one crash is isolated to its own plugin) with:

- a bound local endpoint core created before spawning: a Unix domain socket path on Linux, a named pipe on Windows, passed as `--endpoint <path>`;
- a per-launch random token (32 bytes, hex-encoded), passed as `--token <hex>` and never logged;
- no other inherited privilege beyond what §7.3/§7.4 describe.

**Handshake.** `vosu plugin-host` connects to the endpoint and sends the token as the first frame; core closes the connection without responding if it does not match within 5 seconds. On success core sends a `Handshake` frame: the negotiated ABI major version and, once a session exists, that session's bitness. `vosu plugin-host` then completes the capability drop (§7.3) or job-object confinement (§7.4), `dlopen`s the plugin library named in the manifest's per-target `library` path, resolves `vosu_plugin_entry_v1`, validates `abi_version`/`struct_size` exactly as an in-process load would, and calls `create` with a local `vosu_host_services` whose `send` function serializes the outgoing `vosu_message` onto the pipe back to core and whose replies arrive by deserializing inbound frames into calls on the plugin's registered callback. The plugin library's own code is unaware it is being relayed: it only ever makes and receives ordinary in-process vtable calls into `vosu plugin-host`'s local implementation of `vosu_host_services`.

**Framing.** Each direction of the pipe carries length-prefixed frames: a 4-byte little-endian total length (excluding itself), followed by `opcode` (u32), `flags` (u32), `correlation_id` (u64), `payload_len` (u32), and `payload_len` bytes — the same fields as `vosu_message`, concatenated rather than passed as a C struct. A frame larger than 16 MiB is refused and the connection closed (bounded buffering, per `AGENTS.md`'s unbounded-queue rule). Frames may arrive and be answered out of order; `correlation_id` (or, for read plans, `vosu_plan_id`) is how each side matches a reply to its request.

**Crash handling and restart.** An unexpected pipe EOF or `vosu plugin-host` process exit (status conditions other than a `destroy`-initiated clean shutdown) marks every session that plugin owned `detached` (`reason: "lost"`), logs the exit, and restarts the process with the same exponential backoff as 1.x tool services: 1, 2, 4, 8, 16 seconds; five crashes within five minutes mark the plugin `failed` until the user restarts it from the dashboard. An in-process plugin crash is not survivable — it is the daemon crashing — which is exactly the cost the trust table in §7.1 prices in.

### 7.3 Linux: capability drop before loading plugin code

`vosu --setup` grants the `vosu` executable `CAP_SYS_PTRACE` (and, where eBPF process-event backends are available, `CAP_BPF`, `CAP_PERFMON`, `CAP_SYS_RESOURCE`) so core itself can read arbitrary same-user process memory. `vosu plugin-host` must never hold any of these: every memory read it needs goes through `VOSU_OP_READ_PLAN_SUBMIT` to core, which executes it against only the one process that plugin matched. Before `dlopen`ing the plugin library — on its single thread, before any plugin code has run — `vosu plugin-host` executes, in order:

1. `prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)` — refuse to regain privilege through a later `execve`.
2. `prctl(PR_CAPBSET_DROP, cap, 0, 0, 0)` for every capability still in the bounding set, until it is empty — a dropped bounding-set capability can never be re-added to the permitted set, even by a `setuid` binary the plugin might somehow invoke.
3. `prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_CLEAR_ALL, 0, 0, 0)` — clear the ambient set so nothing survives into a child process by inheritance.
4. `capset(2)` with an empty `cap_user_data_t` for every capability set it addresses (effective, permitted, inheritable) — zero the calling thread's own active capabilities, not just what a future `execve` could gain.

Only after step 4 returns does `vosu plugin-host` call `dlopen`. Because `prctl`/`capset` apply per-thread, this sequence runs before the process creates any additional thread (including any thread the plugin's own `create` might spawn).

### 7.4 Windows: job objects

Same-user `PROCESS_VM_READ` needs no elevated privilege on Windows, so no capability-drop equivalent is required; `vosu plugin-host` instead gets the same lifecycle confinement 1.x tool services use. Core creates the child with `CREATE_SUSPENDED`, immediately calls `AssignProcessToJobObject` with a job configured with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` (so closing core's job handle — including an unclean core exit — terminates every plugin-host process rather than leaking it) before resuming the thread, eliminating the race where the child could run unconfined between creation and assignment.

## 8. Plugin manifest (`plugin.toml`) runtime fields

```toml
manifest_version = 1              # required; this host accepts only 1
id = "vosu.osu"                   # required; naming-contract plugin id
name = "osu!"                     # required, display name
version = "2.0.0"                 # required, semver of this plugin
abi_version = 1                   # required; major ABI version this binary targets
runtime = "in-process"            # required; "in-process" | "process" (request only, see 7.1)
concurrent_dispatch = false       # optional, default false; see "Threading model"
settings_schema = "settings.schema.json"  # optional, path relative to the manifest

[targets.x86_64-unknown-linux-gnu]   # omitted entirely for bundled plugins
library = "lib/libvosu_osu.so"       # path relative to the manifest

[targets.x86_64-pc-windows-msvc]
library = "lib/vosu_osu.dll"

[match]                           # game plugins only
process_names = ["osu!.exe"]
process_path_patterns = []        # regex, matched against the full executable path
command_line_patterns = []        # regex, matched against the joined argv

[resources]                       # declared file roots, by name, for resource resolution (6.5)
songs = "/abs/path/to/osu!/Songs"

# [provides] / [requires] / [optional]: docs/v2/catalog-v2.md; not defined here.
```

A plugin missing a `[targets.<host triple>]` entry for the running host (and not bundled) is simply unavailable there; the install resolver ([ADR 0022](../decisions/0022-install-resolver-and-2-0-transition.md)) surfaces that before installation, and the host otherwise refuses to start it with a named error rather than guessing a fallback binary. `manifest_version`, `id` grammar, `version` semver, and `abi_version` validity are checked before any library is loaded.

## 9. Rust SDK sketch

```rust
// vosu-plugin-api (the Rust SDK crate)

pub trait Plugin: Sized + Send {
    fn create(host: HostServices) -> Result<Self, PluginError>;
    /// A host -> plugin message this plugin's dispatch received.
    fn on_message(&mut self, message: Message) -> Result<(), PluginError>;
}

pub struct HostServices { /* wraps `*const vosu_host_services` + its ctx pointer */ }

impl HostServices {
    pub fn publish(&self, session: SessionId, ns: &str, field_path: &str, value: Value);
    pub fn submit_read_plan(&self, session: SessionId, plan: ReadPlan) -> PlanId;
    pub fn scan(&self, session: SessionId, request: ScanRequest) -> ScanId;
    pub fn log(&self, level: Level, message: &str, fields: impl Into<CborMap>);
    pub fn set_timer(&self, interval: Option<Duration>) -> TimerId;
    pub fn cancel_timer(&self, timer: TimerId);
}

// One macro call generates `vosu_plugin_entry_v1` for an external build, or
// appends to the bundled-plugin registry for a build with `bundled` enabled;
// the plugin author's code is identical either way.
vosu_plugin_api::export_plugin!(OsuPlugin);

struct OsuPlugin { host: HostServices, /* ... */ }

impl Plugin for OsuPlugin {
    fn create(host: HostServices) -> Result<Self, PluginError> {
        Ok(Self { host })
    }

    fn on_message(&mut self, message: Message) -> Result<(), PluginError> {
        match message {
            Message::SessionAttached { session, bitness, .. } => self.attach(session, bitness),
            Message::ReadPlanResult { plan, results } => self.handle_reads(plan, results),
            Message::DemandChanged { session, field, subscribed } => {
                self.set_sampling(session, field, subscribed)
            }
            _ => Ok(()),
        }
    }
}
```

## 10. Open questions

- **Shared-memory fast path for large read-plan/resource payloads.** The plugin-host protocol (§7.2) frames every payload inline on the pipe; a shared-memory ring buffer for payloads above a size threshold was considered (ADR 0019's mention of "a pipe/shared memory") but adds negotiated-region lifetime and cross-platform naming complexity that this spec defers until `BrokeredReadBench` (`v2/bench`) shows pipe framing itself, rather than the extra process hop, is the bottleneck for realistic read-plan sizes. Recommendation: ship pipe-only framing for v1; revisit only if the bench shows per-byte (not per-round-trip) cost dominates.
- **`concurrent_dispatch` is declared but its contract is not yet tested against a reference plugin.** Recommendation: require the first concurrent-capable plugin (likely a provider plugin) to ship a stress test exercising overlapping `dispatch` calls before any plugin ships with the flag set, rather than gating it behind a host-side check that cannot verify thread-safety.
- **Action cancellation for in-process plugins has no enforcement mechanism**, only a convention that well-behaved plugins honor `VOSU_OP_ACTION_CANCEL`. This is a direct consequence of ADR 0019's trust table (an in-process plugin cannot be safely preempted) and is recorded here rather than in the ADR per this phase's constraints; no spec-level mitigation is proposed beyond documenting the asymmetry tools must expect.
