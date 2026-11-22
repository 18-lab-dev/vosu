---
type: Contract
title: "Plugin ABI v1"
description: "Implementer-grade contract for the vosu 2.0 plugin binary interface, execution model, and host services."
---

# Plugin ABI v1

This is the implementer contract for [ADR 0019](../decisions/0019-plugin-runtime-and-trust.md): the versioned C ABI plugins implement, the message protocol between a plugin and host services, and how the host executes a plugin in-process or inside `vosu plugin-host`. The reference C declarations are [`include/vosu_plugin.h`](../../include/vosu_plugin.h); keep both in sync. Capability fields (`provides`/`requires`/`optional`, `[[actions]]`, plugin metadata, catalog mirroring) are defined in [`docs/v2/catalog-v2.md`](catalog-v2.md); field and namespace semantics are defined in [`docs/v2/state-schema.md`](state-schema.md); API v1 routes are defined in [`docs/v2/api-v1.md`](api-v1.md). This document references all three by name rather than redefining them. The shared naming contract (plugin ids, capability ids, namespaces, sessions) is normative for every identifier here.

**Field addressing.** Every opcode below that names a field within a namespace (`VOSU_OP_PUBLISH_FIELD`, `VOSU_OP_DEMAND_CHANGED`, `VOSU_OP_STATE_SUBSCRIBE`/`STATE_UPDATE`) addresses it with a namespace id plus an RFC 6901 JSON Pointer relative to that namespace's root — exactly how `docs/v2/api-v1.md`'s subscription and HTTP-read protocols address it. The dotted form (`field:<namespace>.<path>`) is only a capability id, used in manifests and the catalog; `docs/v2/catalog-v2.md` owns the dotted-path-to-namespace-prefix split rule. This document never uses dotted paths on the wire.

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

A reader (host reading the plugin's vtable, or plugin reading `vosu_host_services`) must never touch a field beyond `struct_size` bytes from the struct's start. Fields appended by a newer minor revision are therefore safe to add: an older peer's `struct_size` simply excludes them, and a producer that predates a given function pointer never had it to set, so the newer consumer must treat any function pointer past the reported `struct_size` as absent rather than reading uninitialized memory. The host rejects a plugin whose `abi_version` does not equal the major version implied by the symbol it looked up, and whose `struct_size` is smaller than the minimum valid size for that major version: the three required functions `create`, `dispatch`, and `destroy` (§6 lists every message `dispatch` alone now carries, replacing the earlier separate asynchronous-callback registration).

## 2. Plugin descriptor and symbol naming

A plugin's identity, version, and runtime declaration live in its manifest (`plugin.toml`, §8), not in the ABI itself — the vtable carries no name or id. Core correlates a loaded vtable with its manifest by load order: it loads the library named in the manifest's per-target `library` path and calls that library's single entry point; a library must not be shared by two manifests.

**Bundled first-party plugins** (the dashboard, overlay hosting, tool hosting, the osu! plugin, and the tosu v2 / gosumemory v1 / StreamCompanion adapters) are compiled into the `vosu` executable and never export a dynamic symbol. The Rust SDK's registration macro (§9) instead appends each bundled plugin's `vosu_plugin_vtable` to a compile-time-collected static registry (a linker-collected slice, so no runtime registration call is required and no bundled plugin can be accidentally omitted). At startup, core iterates that registry exactly as it iterates `dlopen`ed external plugins: same manifest validation, same `create`/`dispatch`/`destroy` calls, same trust table (§7.1) — bundled plugins are simply always eligible for `in-process` runtime, since their code already runs in the daemon's address space regardless of any grant. A bundled plugin's `plugin.toml` omits `[targets]` entirely (there is nothing to locate on disk) and must not request `runtime = "process"`; the host rejects such a manifest as invalid at startup, not silently downgrades it.

## 3. Message envelope and encoding

Every call across the plugin/host vtable boundary — `vosu_host_services::send` (plugin → host) and `vosu_plugin_vtable::dispatch` (host → plugin) — carries one `vosu_message`:

```c
typedef struct vosu_message {
    uint32_t opcode;
    uint32_t flags;
    uint64_t correlation_id; /* 0 when the opcode has no reply */
    vosu_bytes payload;
} vosu_message;
```

**Encoding choice.** Two payload shapes coexist, selected by opcode:

- **Flat binary layout** for the four opcodes on the measured hot path: state publication (`VOSU_OP_PUBLISH_FIELD`), demand signals (`VOSU_OP_DEMAND_CHANGED`), and memory read plans (`VOSU_OP_READ_PLAN_SUBMIT` / `VOSU_OP_READ_PLAN_RESULT`). These opcodes can fire many times per second per session. Their payload is a fixed-size `*_header` struct (defined per opcode below) at the start of the buffer, followed by trailing variable-length data (strings, op lists, result arrays); every variable-length field in the header is an `{offset, len}` pair naming a byte range **within that same payload buffer**, never a pointer. Building a publish or a read plan therefore costs writing a small struct and appending bytes, not a serialize pass, and — because the payload contains no pointers — the exact same bytes are valid whether `dispatch`/`send` is a direct in-process call or a frame relayed to/from `vosu plugin-host` (§7.2): the relay copies the buffer, it does not reinterpret it.
- **CBOR** ([RFC 8949](https://www.rfc-editor.org/rfc/rfc8949)) for every other opcode: session lifecycle, focus, scanning, resource/tree/stream/root operations, actions, settings, logging, timers, and cross-plugin state subscription. These are low-frequency (session attach/detach, a user clicking an action, a settings change, a handful of log lines) or already latency-insensitive (a signature scan). CBOR was chosen over JSON because it is binary and needs no quoting/escaping or text-number round-trip concerns, and over a second bespoke binary format because its data model (maps, arrays, strings, byte strings, numbers) is exactly the JSON Schema model the published namespace and settings schemas already use ([`docs/v2/state-schema.md`](state-schema.md)): a CBOR action payload or settings object re-encodes to JSON for an HTTP response with no semantic translation, and every mainstream language has a mature CBOR library, which matters because `vosu plugin-host` relays this encoding over its channel regardless of the plugin's implementation language.

No field in any payload — flat or CBOR — is ever a raw pointer into the host's or the plugin's address space. A target-process address (an absolute address from a scan result, or a read-plan base/dependent address) is an opaque `uint64_t` meaningful only as an argument back into a read plan; neither side dereferences it directly, even a fully trusted in-process plugin — it always goes through `VOSU_OP_READ_PLAN_SUBMIT`.

**Alignment.** `vosu_message::payload.data` is always 8-byte aligned; the relay inside `vosu plugin-host` (§7.2) copies each frame into an aligned buffer before dispatching it rather than pointing into the wrapping ring as-is. Inside a flat payload, every `*_offset` field names a position aligned to the element it addresses: 1 (no requirement) for raw string/byte data, 4 for an array of a 4-byte-aligned struct (`vosu_pointer_ref`, `vosu_read_result_flat`), 8 for an array of a struct containing a `uint64_t`/`int64_t`/`double` member (`vosu_plan_op`, `vosu_stage_header`). Every flat struct is written with explicit `reserved` padding fields so its layout has no compiler-inserted gaps. The exact byte layout, field by field:

| Struct | Size | Fields (offset: name, size) |
| --- | --- | --- |
| `vosu_field_publish_header` | 48 | 0: handle(8), 8: ns_offset(4), 12: ns_len(4), 16: pointer_offset(4), 20: pointer_len(4), 24: value_kind(4), 28: reserved(4), 32: scalar(8), 40: data_offset(4), 44: data_len(4) |
| `vosu_pointer_ref` | 16 | 0: ns_offset(4), 4: ns_len(4), 8: pointer_offset(4), 12: pointer_len(4) |
| `vosu_demand_changed_header` | 24 | 0: handle(8), 8: regular(1), 9: precise(1), 10: reserved(6), 16: selectors_offset(4), 20: selector_count(4) |
| `vosu_plan_base_flat` | 32 | 0: kind(4), 4: module_name_offset(4), 8: module_name_len(4), 12: reserved(4), 16: address(8), 24: module_offset(8) |
| `vosu_plan_op` | 16 | 0: offset(8), 8: length(4), 12: reserved(4) |
| `vosu_stage_base` | 24 | 0: kind(4), 4: ref_stage(4), 8: ref_op(4), 12: reserved(4), 16: offset(8) |
| `vosu_stage_header` | 32 | 0: base(24), 24: ops_offset(4), 28: op_count(4) |
| `vosu_read_plan_submit_header` | 56 | 0: plan(8), 8: handle(8), 16: root(32), 48: stages_offset(4), 52: stage_count(4) |
| `vosu_read_result_flat` | 24 | 0: kind(4), 4: bytes_offset(4), 8: bytes_len(4), 12: error_code(4), 16: error_message_offset(4), 20: error_message_len(4) |
| `vosu_read_plan_result_header` | 16 | 0: plan(8), 8: results_offset(4), 12: result_count(4) |

**Session identity.** A session has two identities that are deliberately not interchangeable: the **public id**, an opaque string minted by core (the naming contract's session id, also what `docs/v2/api-v1.md` addresses sessions by), and the **handle**, a `vosu_session_id` (`uint64_t`) scoped to one plugin instance. `VOSU_OP_SESSION_ATTACHED` (§6.2) delivers both; every later message that names a session — flat or CBOR — uses the handle, never the string, with `0` reserved to mean "no particular session" or "every session of this plugin" where an opcode's description says so.

## 4. Threading model and blocking

The host serializes calls into one plugin instance (`dispatch`) onto one thread per instance by default, matching the existing single-worker-per-instance model. A plugin's manifest may declare `concurrent_dispatch = true` (§8) to opt into concurrent calls, which requires its `dispatch` implementation to be internally synchronized; the host then dispatches from its own worker pool instead of one dedicated thread.

- `dispatch` may block; for an in-process plugin it runs on a host worker thread dedicated to that instance (never the state-bus publisher thread, so a slow plugin cannot stall [ADR 0002](../decisions/0002-immutable-latest-only-snapshots.md)'s publication), and for a `process`-runtime plugin it runs inside `vosu plugin-host`, which shares no state with core. Even so, `dispatch` should return quickly for the high-frequency notification opcodes (`VOSU_OP_DEMAND_CHANGED`, `VOSU_OP_FOCUS_CHANGED`, `VOSU_OP_TIMER_FIRED`, `VOSU_OP_READ_PLAN_RESULT`, `VOSU_OP_SETTINGS_CHANGED`): real work triggered by one of these must be handed to the plugin's own thread and reported back asynchronously (another publish, another action result), never performed inline, because these can arrive often enough to queue up behind a slow handler. Opcodes that represent an actual unit of work (`VOSU_OP_ACTION_INVOKE`, `VOSU_OP_RESOURCE_RESOLVE`, `VOSU_OP_TREE_LIST`/`TREE_LOOKUP`, `VOSU_OP_STREAM_READ`, `VOSU_OP_SCAN_REQUEST`'s eventual result) may legitimately take longer, at the cost of only that one plugin instance's own throughput.
- `create` may block briefly (it runs before the plugin is considered loaded) but must not perform unbounded I/O; a plugin needing slow first-run setup starts it in its own thread from `create` and reports readiness through normal state publication.
- `destroy` must return within the host's configured shutdown grace period (matching the 5-second tool-service shutdown precedent in [`docs/tools/protocol.md`](../tools/protocol.md)) or the host proceeds without it: for a `process`-runtime plugin, `vosu plugin-host` is killed; an in-process plugin that does not return leaves the daemon process exit blocked, an accepted cost of the trust an `in-process` grant represents.

## 5. Error model

Every `dispatch`/`send` call returns a `vosu_status`. `VOSU_OK` means the message was accepted for processing, not that any requested effect already completed — opcodes with a result (read plans, scans, resource/tree/stream operations, actions) report that result asynchronously through a later message, correlated by `correlation_id` (or, for read plans, by `plan`, since one submission's results may legitimately return out of order relative to a later submission under `concurrent_dispatch`). A non-`VOSU_OK` status means the call itself was rejected and produces no later message:

| Status | Meaning |
| --- | --- |
| `VOSU_ERR_INVALID_ARGUMENT` | Malformed payload, an offset/length outside the buffer, or a read plan over `VOSU_READ_PLAN_MAX_BYTES` |
| `VOSU_ERR_PERMISSION_DENIED` | An opcode this plugin's trust level cannot use (e.g. a read plan touching a session it did not match) |
| `VOSU_ERR_NOT_READY` | The target session has no live process |
| `VOSU_ERR_TARGET_GONE` | A session died between submission and execution |
| `VOSU_ERR_UNSUPPORTED` | An opcode the receiver's minor version predates |
| `VOSU_ERR_BACKEND` | An underlying OS failure |
| `VOSU_ERR_CANCELLED` | The call answers an invocation, stream, or plan that was already cancelled or closed by the other side |
| `VOSU_ERR_TIMED_OUT` | The call exceeded a host-enforced deadline (an action invocation, a slow stream read, or plan execution) |

Per-read failure inside an otherwise-accepted plan is reported per read (`vosu_read_result_kind`, mirroring `src/process.rs`'s `MemoryError`/`ReadStatus` vocabulary) rather than failing the whole plan.

## 6. Host services

### 6.1 Process matching

A game plugin declares, in its manifest `[match]` table (§8), the process matchers core uses to recognize candidate instances for [ADR 0001](../decisions/0001-single-daemon-multi-instance-lifecycle.md) discovery. This table is the schema `docs/v2/catalog-v2.md`'s `matcher` JSON object mirrors verbatim:

```toml
[match]
process_names = ["osu!.exe"]          # exact executable file names
process_path_patterns = []             # regular expressions matched against the full executable path
command_line_patterns = []             # regular expressions matched against the joined argv
```

The JSON mirror (used in `catalog-v2.json` plugin entries) is `"matcher": {"process_names": [...], "process_path_patterns": [...], "command_line_patterns": [...]}` with identical field names and semantics. Precise matching rules:

- Patterns use Rust [`regex`](https://docs.rs/regex) crate syntax and are **full-match anchored** (as if wrapped in `^(?:…)$`); a pattern that only needs to match part of a path must spell out its own wildcards.
- `process_names` and `process_path_patterns` are compared **case-insensitively on Windows** and **case-sensitively on Linux**, matching each platform's own filesystem semantics.
- Path separators in both the candidate path and the pattern are normalized to `/` before matching, so one pattern works on both targets.
- Two plugins must not declare overlapping matchers (a process both could claim) while **both are installed and enabled**; the host refuses to start the second with `VOSU_ERR_INVALID_ARGUMENT` surfaced at manifest validation. A catalogued-but-not-installed plugin's matcher never triggers this refusal — it would otherwise block installing a replacement game plugin ([ADR 0022](../decisions/0022-install-resolver-and-2-0-transition.md)).

A matched process becomes a candidate instance exactly as 1.x client detection does; acceptance, generation assignment, and PID-order retention are unchanged core behavior, now parameterized by which plugin's matchers accepted the process.

### 6.2 Session lifecycle

A **session** (naming contract) is one attached client; its lifecycle, generation, and selection semantics are exactly [ADR 0001](../decisions/0001-single-daemon-multi-instance-lifecycle.md)'s, generalized across game plugins by [ADR 0020](../decisions/0020-game-agnostic-data-model.md). The plugin never opens or closes a session itself — core owns discovery and sends:

- `VOSU_OP_SESSION_ATTACHED` (CBOR): `{"session": string, "handle": u64, "generation": u64, "pid": u32, "bitness": "x86"|"x64"}`. `session` is the public id (naming contract); `handle` is this plugin instance's `vosu_session_id` for every later message. The plugin may now begin reading and must identify the client (below) before publishing anything else for it.
- `VOSU_OP_SESSION_DETACHED` (CBOR): `{"handle": u64, "reason": "exited"|"lost"|"superseded"}`. The plugin must stop reading that session; any read plan it still submits for that handle answers `VOSU_ERR_TARGET_GONE`.

`bitness` tells the plugin which pointer width a dependent read-plan stage uses for that session (§6.3); it never issues raw syscalls itself to discover it.

**Filling `vosu.client`.** Per [`docs/v2/state-schema.md`](state-schema.md), core fills `vosu.client.session`, `.pid`, `.focused`, and `.plugin` itself from its own bookkeeping (it already has all four). The attaching game plugin fills the remaining four leaves with one message, sent once per session before any other publication for it:

`VOSU_OP_SESSION_IDENTIFY` (plugin → host, CBOR, no reply): `{"handle": u64, "game": string, "variant": string, "name": string, "version": string}`.

A session is not exposed anywhere outside core — `/api/v1/sessions`, other plugins, focus selection — until its game plugin sends `SESSION_IDENTIFY` for it. If identification does not arrive within 5 seconds of `SESSION_ATTACHED`, core logs once and keeps the session hidden (it still exists internally, and the game plugin may still identify it later, which exposes it from that point on) rather than exposing a session with no `vosu.client.game`/`.variant`/`.name`/`.version`.

**Focus.** Which session is focused is decided by the Focus rule in `docs/v2/api-v1.md`, over identified sessions only; this document does not restate it. Core notifies every affected plugin when focus changes (typically two notifications: the session losing it and the session gaining it):

`VOSU_OP_FOCUS_CHANGED` (host → plugin, CBOR, no reply): `{"handle": u64, "focused": bool}`.

`SESSION_ATTACHED` carries no `focused` field; a plugin that needs it waits for the first `FOCUS_CHANGED` (sent once, promptly, after the session is identified, even if focus does not change relative to the previous state).

**Delivery ordering.** Core sends a plugin instance `SESSION_ATTACHED` for a handle before any other message naming that handle to it — not only to the owning game plugin, but to any plugin a handle is about to be named to for any reason (a `STATE_SUBSCRIBE`/`STATE_UPDATE` targeting that session, an `ACTION_INVOKE`/`ACTION_CALL` carrying it, a `RESOURCE_RESOLVE` for a session-scoped root). A plugin that has no use for session lifecycle itself may simply ignore the fields it does not need.

**Non-owning plugins and session events.** `SESSION_ATTACHED`, `SESSION_DETACHED`, and `FOCUS_CHANGED` for a given session go to that session's owning game plugin always, and additionally to any other plugin that subscribed to session events: `VOSU_OP_STATE_SUBSCRIBE` (§6.12) with a selector naming the reserved pseudo-namespace `vosu.sessions` and an empty pointer, exactly as `docs/v2/api-v1.md`'s session-events WebSocket reuses its field-subscription protocol for the same purpose. Non-owning plugins learn of a session only once it is identified: their `SESSION_ATTACHED` is sent when the owning plugin's `SESSION_IDENTIFY` arrives, not at attach. A subscribing plugin receives the three messages above instead of (not in addition to) a `STATE_UPDATE` for that pseudo-namespace, since they are already typed events; this is the one selector for which `STATE_SUBSCRIBE` changes a plugin's delivered opcode set rather than only its demand.

### 6.3 Memory read plans

A read plan (`VOSU_OP_READ_PLAN_SUBMIT`, flat layout in the header) is one base address (an absolute address or a module name plus offset, resolved by core against that session's module list so the plugin never computes an ASLR-adjusted address itself) followed by an ordered list of **stages**. Each stage has a base — either the plan's own root (stage 0, typically) or a value read by an **earlier** stage's identified op, interpreted as a pointer-width value (per the session's reported bitness) and optionally offset further — and a batch of independent `{offset, length}` ops relative to that resolved base. This expresses both a deep sequential pointer chase (one op per stage, each stage dependent on the previous) and a wide batch of sibling reads from one resolved address (many ops in one stage) in a single submission.

Core executes the whole dependent chain against the exact process that plugin's matcher accepted — never another session's process, even if the plugin names an arbitrary handle — and answers with one `VOSU_OP_READ_PLAN_RESULT` whose `results` are flat, in the same (stage, op) order as the submission: **one submission is one round trip**, regardless of stage or op count. This is the design [`docs/v2/brokered-reads.md`](brokered-reads.md)'s `BrokeredReadBench` (branch `v2/bench`) measured and recommends: a plan that could express only flat, independent reads (forcing the plugin to resolve pointers itself and issue follow-up reads) reproduces that benchmark's "naive" variant, 16–25x slower, and must not ship. The same `vosu_read_plan_submit_header`/`vosu_read_plan_result_header` pair executes unchanged whether the plugin runs in-process (core's own `ProcessMemory` answers it directly on the calling thread) or is brokered (the pair is relayed, byte for byte, to and from `vosu plugin-host` over the channel in §7.2).

A submitted plan whose encoded payload, or whose resulting response payload, would exceed `VOSU_READ_PLAN_MAX_BYTES` (64 KiB) is rejected with `VOSU_ERR_INVALID_ARGUMENT` before any read executes, bounding host allocation per request regardless of what a plugin submits. Per-read outcomes mirror `src/process.rs`'s existing `ReadStatus`/`MemoryError` vocabulary (`VOSU_READ_OK`, `VOSU_READ_NOT_READY`, `VOSU_READ_UNSUPPORTED`, `VOSU_READ_ERROR`) so a game plugin's retry/backoff logic matches today's in-tree readers. A dependent stage whose `ref_op` read a length other than the session's pointer width is a plan validation error (`VOSU_ERR_INVALID_ARGUMENT`), not a silent truncation or zero-extension.

A plugin that no longer needs an outstanding plan's result (its owning session detached, or its own logic moved on) may abandon it with `VOSU_OP_READ_PLAN_CANCEL` (plugin → host, CBOR, no reply): `{"plan": u64}`. Core discards the result instead of delivering it once execution finishes; if the result was already in flight when the cancellation arrived, the plugin's `dispatch` returns `VOSU_ERR_CANCELLED` for the late `VOSU_OP_READ_PLAN_RESULT` rather than treating it as valid.

### 6.4 Signature scanning and module enumeration

`VOSU_OP_SCAN_REQUEST` (plugin → host, CBOR): `{"session": handle u64, "module": string|null, "pattern": string, "non_zero_mask": bool, "all_matches": bool}`. `pattern` uses exactly `src/process/scanner.rs`'s `Pattern::parse` grammar (space-separated hex byte pairs and `??` wildcards). `module` scopes the scan to one module's readable regions; `null` scans every readable region of the process. `VOSU_OP_SCAN_RESULT` (host → plugin, CBOR, reply correlated by `correlation_id`): `{"addresses": [u64, ...]}` — absolute target-process addresses, suitable as a later read plan's `VOSU_BASE_ABSOLUTE`. There is no dedicated scan-id type: a scan is correlated like every other CBOR request/reply pair. Scans are not on the measured hot path and are not batched with reads; a plugin that scans every tick instead of caching the result against a module's load address is a plugin bug, not an ABI limitation.

### 6.5 Declared file roots and resource resolution

Resources follow [ADR 0021](../decisions/0021-api-v1-and-upstream-adapters.md) and are routed by [`docs/v2/api-v1.md`](api-v1.md); this section defines only the plugin-facing protocol core uses to mint and later resolve a reference the plugin published.

**File roots.** A game folder's real path is known only at runtime, from the attached process, so the manifest (§8) names root **slots** only — bare strings, no path — and the plugin registers each slot's actual path once it knows it:

`VOSU_OP_ROOT_DECLARE` (plugin → host, CBOR, no reply): `{"handle": u64|null, "name": string, "path": string}`. `name` must be one of the manifest's declared `root_slots`; `path` is an absolute, host-native filesystem path. `handle: null` declares a plugin-wide root (most game installations have one songs/maps folder regardless of session); a non-null handle scopes the root to one session, for a plugin that can observe a different path per attached process. Core opens the declared path with [ADR 0005](../decisions/0005-root-confined-file-serving.md)'s retained-handle confinement and keyed storage; a later resolve reply naming a root the plugin never declared, or a `path` that escapes it, is refused rather than trusted. When both a session-scoped and a plugin-wide root share `name`, the session-scoped one takes precedence for that session.

**Minting a reference's id.** A `VOSU_VALUE_RESOURCE_REF`/`VOSU_VALUE_TREE_REF` (§6.6) carries `{"kind": string, "key": <CBOR>, "name": string, "content_id": <CBOR>}`: `kind` and `key` are the plugin's own lookup key (opaque to core, echoed back unchanged in a later resolve request); `name` is a file name whose extension both tells core which allowlisted type to assign ([`docs/v2/api-v1.md`](api-v1.md) owns the allowlist) and **is the published type for the lifetime of that id** — a single resource is typed once, at publish time, from `name`, and the served `Content-Type` always equals that published type, never a type derived later from wherever the resolved file actually lives; `content_id` is the content identity (a store hash, or a path+size+modification-time digest). Core mints the public id **the moment the field is published** — with no round trip to the plugin — from the inputs [`docs/v2/api-v1.md`](api-v1.md)'s id registry defines; the plugin only answers a resolve request the first time something actually requests that id's bytes or index, which may be much later or never.

**Resolving a reference.** A resource or tree reference is always published within a session (§6.6), so core always has one to name: on the first `GET`/`HEAD` for a minted id (or its tree index), core asks the plugin to resolve the `{kind, key}` it published, naming that session's handle so the right root is chosen — the session's own declared root first, then a plugin-wide root of the same name, per "File roots" above:

- `VOSU_OP_RESOURCE_RESOLVE` (host → plugin, CBOR): `{"handle": u64, "kind": string, "key": <CBOR>}`.
- `VOSU_OP_RESOURCE_RESOLVED` (plugin → host, CBOR, reply correlated by `correlation_id`; this is also the reply opcode for `TREE_LIST` and `TREE_LOOKUP` below — the host already knows which request a reply answers from its own pending-request table keyed by `correlation_id`, so one reply opcode suffices): `{"root": string, "path": string}` (a declared root plus a relative path inside it) or `{"stream": u64, "size": u64}` (a `vosu_resource_id` naming a bounded byte stream of the given total size, served through `STREAM_READ`/`STREAM_CHUNK` below — mandatory for a `process`-runtime plugin generating content, since it may not even share filesystem visibility with a root core could open directly) or `{"error": {"code": string, "message": string}}`.
- `VOSU_OP_TREE_LIST` (host → plugin, CBOR): `{"handle": u64, "kind": string, "key": <CBOR>}` — list a tree's index. Reply (`RESOURCE_RESOLVED`): `{"entries": [{"path": string, "size": u64, "hash": string}], "truncated": bool, "revision": string}`, matching `docs/v2/api-v1.md`'s tree-index body; an entry with unknown content hashing omits `hash` entirely rather than sending it `null`. Core assigns each entry's `type` itself, from `path`'s extension against the same allowlist as a single resource, when projecting the index to HTTP — the plugin never computes or sends a MIME type.
- `VOSU_OP_TREE_LOOKUP` (host → plugin, CBOR): `{"handle": u64, "kind": string, "key": <CBOR>, "path": string}` — resolve one file within a tree by its relative path, using the plugin's own lookup rules (case folding, fallback matching, and so on). Reply (`RESOURCE_RESOLVED`): same shape as the `RESOURCE_RESOLVE` reply.
- `VOSU_OP_STREAM_READ` (host → plugin, CBOR): `{"stream": u64, "offset": u64, "len": u32}` — read one byte range of a previously opened stream; ranges may be requested in any order. `VOSU_OP_STREAM_CHUNK` (plugin → host, CBOR, reply correlated by `correlation_id`): `{"data": bytes}` (fewer bytes than requested, including zero, signals end of stream) or `{"error": {...}}`.
- `VOSU_OP_STREAM_CLOSE` (host → plugin, CBOR, no reply): `{"stream": u64}` — core no longer needs this stream (evicted from its bounded resource registry, or the request that opened it ended); the plugin releases whatever backs it, and core drops any cached resolution for the id that stream answered, so the next request resolves it again from scratch rather than reusing a now-dead stream. A `STREAM_READ`/`STREAM_CHUNK` exchanged for a stream after its `STREAM_CLOSE` answers `VOSU_ERR_CANCELLED` instead of proceeding.

A game plugin declares which common-namespace `field:` ids it fills in its manifest's `covers` list, and which core resource kinds it fills as reserved-name entries in its `[[resource_kinds]]` table (§8); [`docs/v2/catalog-v2.md`](catalog-v2.md) defines both declarations and what each covers, and this document does not restate them. Its own namespace's fields and resources need no separate coverage declaration, since publishing them is already licensed by `provides`. A `[[resource_kinds]]` entry also bounds which MIME-type families that kind may be typed as; [`docs/v2/api-v1.md`](api-v1.md) owns the allowlist itself (the family tokens are exactly `image`, `audio`, `video`, `chart-text`). The bare kind names `file`, `audio`, `background`, `set`, and `skin` always address the core kinds (`resource:chart.file`/`.audio`/`.background`/`.set`, `resource:skin`); they are forbidden only as the name of a plugin's *own* kind, which must use any other bare name and becomes `resource:<plugin-id>.<kind>` only inside a capability id (`docs/v2/catalog-v2.md` owns this grammar).

### 6.6 State publication to namespaces

A plugin publishes typed values with `VOSU_OP_PUBLISH_FIELD` (§3's flat layout): a handle, a namespace (e.g. `vosu.osu`), an RFC 6901 pointer below it (e.g. `/play/pp/current`), and one value. Core applies [ADR 0002](../decisions/0002-immutable-latest-only-snapshots.md)'s immutable, latest-only publication per session: a plugin publishes whenever a value changes (or at its own sampling cadence while demand exists, §6.7); it never assembles or sends a whole namespace document, and never serializes to JSON itself — projection into each served API is core's job, driven by the JSON Schema each namespace publishes ([`docs/v2/state-schema.md`](state-schema.md)).

A scalar value (`VOSU_VALUE_BOOL`/`I64`/`U64`/`F64`) or a string/byte leaf (`VOSU_VALUE_STRING`/`BYTES`) replaces exactly the value at `pointer`. An array or object — `vosu.play.judgements`, `vosu.osu.keys.all`, and similar — has no leaf-by-leaf wire representation: it is published whole, as `VOSU_VALUE_CBOR`, a CBOR-encoded subtree that replaces the entire value at `pointer` in one message. A resource or tree reference is `VOSU_VALUE_RESOURCE_REF`/`TREE_REF`, trailing CBOR `{"kind": string, "key": <CBOR>, "name": string, "content_id": <CBOR>}` exactly as §6.5's "Minting a reference's id" describes; `key` is any CBOR value the plugin chooses and later receives back unchanged in a `RESOURCE_RESOLVE`/`TREE_LIST`/`TREE_LOOKUP` request, so a plugin may use it as its own lookup key (a file path, a database row id, anything).

`vosu.client` is the one namespace a plugin never publishes to directly (§6.2 fills it from the attach handshake and `SESSION_IDENTIFY`). A game plugin publishes a common-namespace field or a core resource kind only when its coverage declarations (`covers` for fields, a reserved-name `[[resource_kinds]]` entry for core resource kinds, both defined in `docs/v2/catalog-v2.md`) include it; a plugin's own namespace needs no such declaration, since `provides` already licenses it. A covered field the plugin genuinely cannot read for the current session is published as `VOSU_VALUE_NULL`, never a placeholder zero or empty value ([ADR 0020](../decisions/0020-game-agnostic-data-model.md)). Reference maps are the one exception to "NULL replaces the value": publishing `VOSU_VALUE_NULL` at a member of `vosu.chart`'s `/resources` object (for example `/resources/background`) removes that key, which is how a kind the current chart has no file for is omitted. `vosu.skin`'s `/tree` is an ordinary nullable field and is published as `VOSU_VALUE_NULL` when there is no skin tree.

### 6.7 Demand signals

Core tracks, per session and per (namespace, pointer) selector, whether any subscriber — a WebSocket subscriber of [`docs/v2/api-v1.md`](api-v1.md)'s field-subscription protocol, or another plugin's `STATE_SUBSCRIBE` (§6.12) — currently needs it, generalizing 1.x's selected-field subscription hub. A one-shot HTTP read and an action invocation create no demand. `VOSU_OP_DEMAND_CHANGED` (§3's flat layout) tells the plugin the session's **complete current demand**, as a snapshot, whenever it changes — not a single field's transition. A plugin always samples every field it fills at its configured baseline (regular) cadence; there is no signal in this protocol that permits skipping a field entirely, regardless of subscriber count.

- `regular`/`precise`: host-computed acceleration gates, each the union, across every currently selected (namespace, pointer) pair for this session, of that field's declared `x-sampling-lane` (`docs/v2/state-schema.md#sampling-lanes`) — `regular` unions the `regular`/`both` lanes, `precise` the `precise`/`both` lanes — after core's own gameplay-status gating is already applied. A plugin never reads `x-sampling-lane` itself to decide its own gating; core's compiler is its only consumer. `regular`/`precise` tell a plugin when to sample *above* its baseline, never whether to sample at all.
- `selectors`: the session's full current set of subscribed (namespace, pointer) pairs, so a plugin with several independently accelerated fields knows exactly which ones currently have a subscriber, since the two lane flags alone only say whether *any* do.

### 6.8 Actions

An action's declaration — name, version, display title, argument/result JSON Schemas, whether it is session-scoped, and whether it is CLI-exposed — is an `[[actions]]` manifest entry defined by [`docs/v2/catalog-v2.md`](catalog-v2.md); this document only specifies the wire messages that invoke one.

Core forwards an invocation with `VOSU_OP_ACTION_INVOKE` (host → plugin, CBOR): `{"invocation": u64, "session": u64|null, "caller": {"kind": "tool"|"plugin"|"cli", "id": string}, "args": <CBOR>}`. `session` is `null` for an action declared `session: "none"`; `caller.kind` is `"cli"` for a `vosux <plugin-id> <action>` invocation through the broker credential ([ADR 0019](../decisions/0019-plugin-runtime-and-trust.md); the broker path itself is `docs/v2/api-v1.md`'s concern), `"plugin"` for `VOSU_OP_ACTION_CALL` (§6.12), and `"tool"` otherwise.

The callee plugin answers asynchronously, never as `dispatch`'s return value:

- `VOSU_OP_ACTION_PROGRESS` (CBOR, zero or more, no reply): `{"invocation": u64, "fraction": f64|null, "message": string|null}`.
- `VOSU_OP_ACTION_RESULT` (CBOR, exactly once): `{"invocation": u64, "status": "ok"|"error", "value": <CBOR>|null, "code": string|null, "message": string|null}`.

Both are **bidirectional**: plugin → host when the callee is answering core directly (a tool/CLI invocation), and host → plugin when core is relaying the callee's answer to the plugin that issued `VOSU_OP_ACTION_CALL` (§6.12) — core remaps the callee's own `invocation` id back to the caller's id as part of that relay, so each side only ever sees ids it recognizes as its own. `VOSU_OP_ACTION_CANCEL` is bidirectional the same way: host → plugin to ask a callee to stop, and plugin → host for a plugin cancelling its own outstanding `ACTION_CALL` (`{"invocation": u64}`, the caller's own id; core remaps and forwards it to the callee).

Core may request early termination with `VOSU_OP_ACTION_CANCEL` (CBOR): `{"invocation": u64}`; a plugin that supports cancellation stops promptly and still sends exactly one `ACTION_RESULT` with `"status": "error", "code": "cancelled"` (the status `docs/v2/api-v1.md` maps to its own `"state": "cancelled"`). A plugin that ignores cancellation simply runs to completion — core does not forcibly interrupt an in-process plugin's action (it cannot safely do so) and, for a `process`-runtime plugin, only kills `vosu plugin-host` as a last resort after a configurable timeout, which also ends every other in-flight call to that plugin.

### 6.9 Settings

A plugin declares its settings shape with `settings_schema` (§8), a path to a `settings.schema.json` sibling using exactly the overlay/tool `settings.json` schema format ([`docs/tools/protocol.md`](../tools/protocol.md)). Core owns storage, the dashboard editing UI, and `password`-type write-only handling. The plugin's effective settings object is part of the `create` vtable function's `init` parameter (`vosu_plugin.h`: CBOR `{"plugin_id", "settings", "data_dir"}`) — no round trip is needed for the initial value — and every later change arrives as `VOSU_OP_SETTINGS_CHANGED` (host → plugin, CBOR, no reply): `{"values": <CBOR map>}`.

### 6.10 Logging

`VOSU_OP_LOG_RECORD` (plugin → host, CBOR, no reply): `{"level": "trace"|"debug"|"info"|"warn"|"error", "message": string, "fields": <CBOR map>}`. Core attaches its own structured context (plugin id, session, timestamp) and writes to the daemon's existing logging sink. Records are rate-limited per plugin instance (200/second, matching the host's existing bounded-queue discipline); records past the limit are dropped and counted, with the count itself logged once per second while dropping continues, so a runaway plugin cannot grow host memory or CPU use without bound.

### 6.11 Timers

A plugin requests periodic or one-shot callbacks instead of spawning its own OS timer thread, so the host can account for and suspend a plugin's scheduled work exactly as it does reads: `VOSU_OP_TIMER_SET` (plugin → host, CBOR): `{"timer": u64, "interval_ms": u32|null}` (`null` requests a one-shot fire after the host's minimum granularity). The host answers each due timer with `VOSU_OP_TIMER_FIRED` (host → plugin, CBOR, no reply): `{"timer": u64}`. `VOSU_OP_TIMER_CANCEL` (plugin → host, CBOR, no reply): `{"timer": u64}` stops a periodic timer; a cancelled one-shot timer that already fired is a no-op.

### 6.12 Cross-plugin state reads and calls

A plugin reads another plugin's namespace the same way an HTTP client does, generalized to the ABI: `VOSU_OP_STATE_SUBSCRIBE` (plugin → host, CBOR, no reply): `{"session": u64|null, "selectors": [{"ns": string, "pointer": string}, ...]}` (`session: null` with the reserved `vosu.sessions` namespace subscribes to session lifecycle events instead, §6.2). Each `STATE_SUBSCRIBE` **replaces** this plugin's previous selection for the named session wholesale, exactly as 1.x's subscription establishment does — it is not additive; a plugin that wants to add a selector resends the whole set, and sends an empty `selectors` list to unsubscribe entirely from that session. Subscribing creates ordinary demand — the targeted namespace's owning plugin receives it through `DEMAND_CHANGED` like any other subscriber, which is how a provider plugin that depends on a game plugin's field gets it sampled. Core answers with `VOSU_OP_STATE_UPDATE` (host → plugin, CBOR), once immediately with the current values and again on every change, latest-only: `{"session": u64, "values": [{"ns": string, "pointer": string, "value": <CBOR>}]}` (a pointer with no value currently reports `null`, matching the HTTP/WebSocket absence rule).

A plugin invokes another plugin's declared action with `VOSU_OP_ACTION_CALL` (plugin → host, CBOR): `{"invocation": u64 (caller-assigned; never reused for another call within this plugin instance's lifetime), "action": "<plugin-id>.<name>", "session": u64|null, "args": <CBOR>}`. `action` is the bare `{plugin-id}.{name}` form `docs/v2/api-v1.md`'s routes and `vosu.invoke` use, never the `action:` capability-id prefix, which exists only in manifests and the catalog. Core checks that the calling plugin's own manifest `requires`/`optional` ([`docs/v2/catalog-v2.md`](catalog-v2.md)) names that action's capability id before routing it to the target plugin as `ACTION_INVOKE` with `caller: {"kind": "plugin", "id": "<calling plugin id>"}`; the result and any progress messages are delivered back to the calling plugin as `ACTION_RESULT`/`ACTION_PROGRESS` (§6.8) correlated by its own `invocation` id, exactly as a tool's HTTP invocation would see them, and the calling plugin may cancel its own call the same way (§6.8).

## 7. Execution and trust

### 7.1 Runtime request and the host's decision

A manifest requests `runtime = "in-process"` or `runtime = "process"` (§8). `in-process` is granted only to bundled plugins, plugins from the official sources, or a source the user has explicitly marked trusted ([ADR 0023](../decisions/0023-authenticated-catalog-sources.md)); every other plugin may request it but is refused outright (`VOSU_ERR_PERMISSION_DENIED` at load time, logged, plugin left unloaded) rather than silently downgraded — a manifest cannot use its own runtime request to bypass the trust decision.

`runtime = "process"` is available to any plugin regardless of trust tier. **Approval** (the same user-approval flow 1.x applies to user tools, required before the plugin runs at all) applies only to a plugin that is **not** eligible for `in-process` (`user` trust, per the table above); a bundled, official, or user-trusted-source plugin that happens to request `process` runs without a separate approval step, since the user already trusted its source.

### 7.2 The `vosu plugin-host` process protocol

For a `process`-runtime grant, core spawns `vosu plugin-host` once per plugin instance (not shared across plugins, so one crash is isolated to its own plugin).

**Channel setup.** Before spawning, core creates two fixed-capacity shared-memory regions, one per direction (host → plugin-host and plugin-host → host), each organized as a single-producer/single-consumer byte ring with a small atomic header (read offset, write offset, capacity) plus a paired wakeup primitive: on Linux, an anonymous `memfd_create` region and an `eventfd` per direction; on Windows, an **unnamed** file mapping (`CreateFileMappingW` with a `NULL` name, `MapViewOfFile`) and an **unnamed** auto-reset event object (`CreateEventW` with a `NULL` name) per direction — **the Windows mechanics are the one piece of this transport marked "to be measured" by [`docs/v2/brokered-reads.md`](brokered-reads.md#open-questions) and do not block this ABI freeze**; the ring/frame format below is unconditional. Core hands both regions and both wakeup primitives to the child via inherited descriptors/handles at spawn only (on Linux the `memfd` fds and `eventfd`s are simply not marked close-on-exec; on Windows the handles are marked inheritable and passed through the process creation call, never through a named object another process could open by guessing or enumerating its name). Because the channel is reachable only through handles the child inherited — never a discoverable name or path a different local process could open or connect to — **no separate bearer token or authentication frame is needed**: possession of the inherited handle is the whole handshake. `vosu plugin-host` and core always ship as the same vosu release, so there is no ABI-version negotiation at this hop either; version negotiation happens only at the plugin-library boundary (§1), identically whether that library is loaded by core or by `vosu plugin-host`.

Once its capability drop (§7.3) or job-object confinement (§7.4) completes, `vosu plugin-host` `dlopen`s the plugin library named in the manifest's per-target `library` path, resolves `vosu_plugin_entry_v1`, validates `abi_version`/`struct_size` exactly as an in-process load would, and calls `create` with a local `vosu_host_services` whose `send` writes a frame into the plugin-host → host ring and signals its wakeup primitive, and whose replies arrive by `vosu plugin-host` draining the host → plugin-host ring (woken by its primitive) and calling `dispatch`. The plugin library's own code is unaware it is being relayed: it only ever makes and receives ordinary in-process vtable calls into `vosu plugin-host`'s local implementation of `vosu_host_services`.

**Framing.** Each ring carries length-prefixed frames, written and read as a contiguous sequence of bytes (wrapping at the ring's capacity): a 4-byte little-endian total length (excluding itself), then `opcode` (u32), `flags` (u32), `correlation_id` (u64), `payload_len` (u32), and `payload_len` bytes of payload — the same fields as `vosu_message`, concatenated. A writer whose frame would overflow the ring's current free space blocks (parking and retrying) for up to 2 seconds before the connection is treated as unresponsive and the crash-handling path below applies; this keeps each ring's capacity bounded rather than growing without limit, per `AGENTS.md`'s unbounded-queue rule. Frames may be answered out of order; `correlation_id` (or, for read plans, `plan`) is how each side matches a reply to its request. A flat-layout opcode's frame is exactly its `vosu_message` as `dispatch`/`send` would otherwise receive it — the relay never reinterprets or re-encodes it, only copies it into and out of the ring.

**Crash handling and restart.** An unexpected `vosu plugin-host` process exit, or its reader observing the writer side of either ring go silent past the 2-second backpressure window, marks every session that plugin owned `detached` (`reason: "lost"`), logs the exit, and restarts the process with the same exponential backoff as 1.x tool services: 1, 2, 4, 8, 16 seconds; five crashes within five minutes mark the plugin `failed` until the user restarts it from the dashboard. An in-process plugin crash is not survivable — it is the daemon crashing — which is exactly the cost the trust table in §7.1 prices in.

### 7.3 Linux: capability drop before loading plugin code

`vosu --setup` grants the `vosu` executable `CAP_SYS_PTRACE` (and, where eBPF process-event backends are available, `CAP_BPF`, `CAP_PERFMON`, `CAP_SYS_RESOURCE`) so core itself can read arbitrary same-user process memory. `vosu plugin-host` must never hold any of these: every memory read it needs goes through `VOSU_OP_READ_PLAN_SUBMIT` to core, which executes it against only the one process that plugin matched. Before `dlopen`ing the plugin library — on its single thread, before any plugin code has run — `vosu plugin-host` executes, in order:

1. `prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)` — refuse to regain privilege through a later `execve`.
2. `prctl(PR_CAPBSET_DROP, cap, 0, 0, 0)` for every capability still in the bounding set, until it is empty — a dropped bounding-set capability can never be re-added to the permitted set, even by a `setuid` binary the plugin might somehow invoke.
3. `prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_CLEAR_ALL, 0, 0, 0)` — clear the ambient set so nothing survives into a child process by inheritance.
4. `capset(2)` with an empty `cap_user_data_t` for every capability set it addresses (effective, permitted, inheritable) — zero the calling thread's own active capabilities, not just what a future `execve` could gain.

Only after step 4 returns does `vosu plugin-host` call `dlopen`. Because `prctl`/`capset` apply per-thread, this sequence runs before the process creates any additional thread (including any thread the plugin's own `create` might spawn).

### 7.4 Windows: job objects

Same-user `PROCESS_VM_READ` needs no elevated privilege on Windows, so no capability-drop equivalent is required; `vosu plugin-host` instead gets the same lifecycle confinement 1.x tool services use. Core creates the child with `CREATE_SUSPENDED`, immediately calls `AssignProcessToJobObject` with a job configured with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` (so closing core's job handle — including an unclean core exit — terminates every plugin-host process rather than leaking it) before resuming the thread, eliminating the race where the child could run unconfined between creation and assignment. The inheritable file-mapping and event-object handles (§7.2) are included in the same `CreateProcess` handle list, so the child receives its channel atomically with being placed in the job.

## 8. Plugin manifest (`plugin.toml`) runtime fields

This document defines only the fields the ABI itself needs. Identity/display metadata (`description`, `author`, `homepage`, `icon`, `banner`, `replaces`, `host`), capability declarations (`provides`/`requires`/`optional`, `[[actions]]`), and the common-field coverage list `covers` are defined by [`docs/v2/catalog-v2.md`](catalog-v2.md), which also defines what a reserved-name `[[resource_kinds]]` entry covers and mirrors the runtime-relevant fields below into `catalog-v2.json` plugin entries so the resolver can plan an install without downloading every candidate package.

```toml
manifest_version = 1              # required; this host accepts only 1
id = "vosu.osu"                   # required; naming-contract plugin id
version = "2.0.0"                 # required, semver of this plugin
abi_version = 1                   # required; major ABI version this binary targets
runtime = "in-process"            # required; "in-process" | "process" (request only, see 7.1)
concurrent_dispatch = false       # optional, default false; see "Threading model"
settings_schema = "settings.schema.json"  # optional, path relative to the manifest

[targets.x86_64-unknown-linux-gnu]   # omitted entirely for bundled plugins
library = "lib/libvosu_osu.so"       # path relative to the manifest

[targets.x86_64-pc-windows-msvc]
library = "lib/vosu_osu.dll"

[match]                           # game plugins only; schema in §6.1
process_names = ["osu!.exe"]
process_path_patterns = []
command_line_patterns = []

root_slots = ["songs"]            # declared root-slot names only, no paths (§6.5); paths register at runtime via VOSU_OP_ROOT_DECLARE
covers = ["field:vosu.play.combo", "field:vosu.chart.title"]  # common-namespace fields this game plugin fills (`docs/v2/catalog-v2.md` defines; §6.6 consumes it)

[[resource_kinds]]                # upper bound on the MIME-type families a kind may be typed as (§6.5); api-v1.md owns the allowlist itself
kind = "file"                     # bare; one of the reserved core kinds (file/audio/background/set/skin), so this stays "resource:chart.file" — never prefixed with this plugin's own id
tree = false
families = ["chart-text"]

[[resource_kinds]]
kind = "beatmap-background"       # bare, not one of the reserved core kinds, so becomes "resource:<plugin-id>.beatmap-background"
tree = false
families = ["image"]
```

A plugin missing a `[targets.<host triple>]` entry for the running host (and not bundled) is simply unavailable there; the install resolver ([ADR 0022](../decisions/0022-install-resolver-and-2-0-transition.md)) surfaces that before installation, and the host otherwise refuses to start it with a named error rather than guessing a fallback binary. `root_slots` entries use the grammar `[a-z0-9][a-z0-9_-]{0,63}`; `[[resource_kinds]].kind` uses one segment `[a-z0-9][a-z0-9-]{0,31}`, bare everywhere in this table and in `RESOURCE_RESOLVE`/`TREE_LIST`/`TREE_LOOKUP`'s `kind` field (`VOSU_OP_ROOT_DECLARE` has no `kind` field; it addresses a root by `name`, not a resource kind) — only a `resource:` capability id adds the plugin's own id, and only when `kind` is not one of the five reserved core names (`docs/v2/catalog-v2.md` owns this grammar). `manifest_version`, `id` grammar, `version` semver, and `abi_version` validity are checked before any library is loaded.

## 9. Rust SDK sketch

```rust
// vosu-plugin-api (the Rust SDK crate)

pub trait Plugin: Sized + Send {
    fn create(host: HostServices, init: Init) -> Result<Self, PluginError>;
    /// A host -> plugin message this plugin's dispatch received.
    fn on_message(&mut self, message: Message) -> Result<(), PluginError>;
}

pub struct Init { pub plugin_id: String, pub settings: ciborium::Value, pub data_dir: PathBuf }

pub struct HostServices { /* wraps `*const vosu_host_services` + its ctx pointer */ }

impl HostServices {
    pub fn publish(&self, handle: SessionId, ns: &str, pointer: &str, value: Value);
    pub fn identify_session(&self, handle: SessionId, game: &str, variant: &str, name: &str, version: &str);
    pub fn declare_root(&self, handle: Option<SessionId>, name: &str, path: &Path);
    pub fn submit_read_plan(&self, handle: SessionId, root: PlanBase, stages: &[Stage]) -> PlanId;
    pub fn scan(&self, handle: SessionId, request: ScanRequest) -> CorrelationId;
    pub fn log(&self, level: Level, message: &str, fields: impl Into<CborMap>);
    pub fn set_timer(&self, interval: Option<Duration>) -> TimerId;
    pub fn cancel_timer(&self, timer: TimerId);
    pub fn subscribe_state(&self, session: Option<SessionId>, selectors: &[(String, String)]);
    pub fn call_action(&self, action: &str, session: Option<SessionId>, args: impl Into<CborMap>) -> InvocationId;
}

// One macro call generates `vosu_plugin_entry_v1` for an external build, or
// appends to the bundled-plugin registry for a build with `bundled` enabled;
// the plugin author's code is identical either way.
vosu_plugin_api::export_plugin!(OsuPlugin);

struct OsuPlugin { host: HostServices, /* ... */ }

impl Plugin for OsuPlugin {
    fn create(host: HostServices, init: Init) -> Result<Self, PluginError> {
        Ok(Self { host })
    }

    fn on_message(&mut self, message: Message) -> Result<(), PluginError> {
        match message {
            Message::SessionAttached { handle, bitness, .. } => self.attach(handle, bitness),
            Message::ReadPlanResult { plan, results } => self.handle_reads(plan, results),
            Message::DemandChanged { handle, regular, precise, selectors } => {
                self.set_sampling(handle, regular, precise, selectors)
            }
            _ => Ok(()),
        }
    }
}
```

## 10. Open questions

- **Action cancellation of an in-process *callee* plugin has no enforcement mechanism**, only a convention that well-behaved plugins honor `VOSU_OP_ACTION_CANCEL`. A plugin cancelling its own outstanding `ACTION_CALL` is well-defined (§6.8/§6.12); it is the host asking a callee to stop that cannot be forced. This is a direct consequence of ADR 0019's trust table (an in-process plugin cannot be safely preempted); no spec-level mitigation is proposed beyond documenting the asymmetry tools must expect.
- **`concurrent_dispatch` is declared but its contract is not yet tested against a reference plugin.** Recommendation: require the first concurrent-capable plugin (likely a provider plugin) to ship a stress test exercising overlapping `dispatch` calls before any plugin ships with the flag set, rather than gating it behind a host-side check that cannot verify thread-safety.
