---
type: Contract
title: "Plugin ABI v1"
description: "Implementer-grade contract for the vosu 2.0 plugin binary interface, execution model, and host services."
---

# Plugin ABI v1

This is the implementer contract for [ADR 0019](../decisions/0019-plugin-runtime-and-trust.md): the versioned C ABI plugins implement, the message protocol between a plugin and host services, and how the host executes a plugin in-process or inside `vosu plugin-host`. The reference C declarations are [`include/vosu_plugin.h`](../../include/vosu_plugin.h); keep both in sync. Capability fields (`provides`/`requires`/`optional`, `[[actions]]`, plugin metadata, catalog mirroring) are defined in [`docs/v2/catalog-v2.md`](catalog-v2.md); field and namespace semantics are defined in [`docs/v2/state-schema.md`](state-schema.md); API v1 routes are defined in [`docs/v2/api-v1.md`](api-v1.md). This document references all three by name rather than redefining them. The shared [naming contract](naming.md) (plugin ids, capability ids, namespaces, sessions) is normative for every identifier here.

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

A plugin's identity, version, and runtime declaration live in its `manifest.toml` (§8, `kind = "plugin"`), not in the ABI itself — the vtable carries no name or id. Core correlates a loaded vtable with its manifest by load order: it loads the library named in the manifest's per-target `library` path and calls that library's single entry point; a library must not be shared by two manifests.

**Bundled first-party plugins** (the dashboard, overlay hosting, tool hosting, the osu! plugin, and the upstream-compatible adapters `vosu.adapter.upstream`, `vosu.adapter.gosumemory`, and `vosu.adapter.streamcompanion`; D10) are compiled into the `vosu` executable and never export a dynamic symbol. The Rust SDK's registration macro (§9) instead appends each bundled plugin's `vosu_plugin_vtable` to an explicit `BUNDLED` array in the root crate (D13: a linker-collected slice still needs every crate referenced to be linked, so it gives no omission guarantee a compile-time list does not give more simply) — so no bundled plugin can be accidentally omitted. At startup, core iterates that array exactly as it iterates `dlopen`ed external plugins: same manifest validation, same `create`/`dispatch`/`destroy` calls, same trust table (§7.1) — bundled plugins are simply always eligible for `in-process` runtime, since their code already runs in the daemon's address space regardless of any grant. A bundled plugin's `manifest.toml` omits `[targets]` entirely (there is nothing to locate on disk) and must not request `runtime = "process"`; the host rejects such a manifest as invalid at startup, not silently downgrades it.

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

- **Flat binary layout** for the five opcodes on the measured hot path: state publication (`VOSU_OP_PUBLISH_FIELD`, and its atomic multi-field form `VOSU_OP_PUBLISH_BATCH`, §6.6), demand signals (`VOSU_OP_DEMAND_CHANGED`), and memory read plans (`VOSU_OP_READ_PLAN_SUBMIT` / `VOSU_OP_READ_PLAN_RESULT`). These opcodes can fire many times per second per session. Their payload is a fixed-size `*_header` struct (defined per opcode below) at the start of the buffer, followed by trailing variable-length data (strings, op lists, result arrays); every variable-length field in the header is an `{offset, len}` pair naming a byte range **within that same payload buffer**, never a pointer. Building a publish or a read plan therefore costs writing a small struct and appending bytes, not a serialize pass, and — because the payload contains no pointers — the exact same bytes are valid whether `dispatch`/`send` is a direct in-process call or a frame relayed to/from `vosu plugin-host` (§7.2): the relay copies the buffer, it does not reinterpret it. A receiver rejects any message whose `flags` sets a bit it does not recognize for that opcode with `VOSU_ERR_INVALID_ARGUMENT` rather than silently ignoring it (D4) — `VOSU_OP_READ_PLAN_SUBMIT`'s `VOSU_READ_PLAN_SYNC` (§6.3) is the one flag bit this freeze defines.
- **CBOR** ([RFC 8949](https://www.rfc-editor.org/rfc/rfc8949)) for every other opcode: session lifecycle, focus, scanning, resource/tree/stream/root operations, actions, settings, logging, timers, and cross-plugin state subscription. These are low-frequency (session attach/detach, a user clicking an action, a settings change, a handful of log lines) or already latency-insensitive (a signature scan). CBOR was chosen over JSON because it is binary and needs no quoting/escaping or text-number round-trip concerns, and over a second bespoke binary format because its data model (maps, arrays, strings, byte strings, numbers) is exactly the JSON Schema model the published namespace and settings schemas already use ([`docs/v2/state-schema.md`](state-schema.md)): a CBOR action payload or settings object re-encodes to JSON for an HTTP response with no semantic translation, and every mainstream language has a mature CBOR library, which matters because `vosu plugin-host` relays this encoding over its channel regardless of the plugin's implementation language. A receiver accepts and ignores a CBOR map key it does not recognize for a given message, so a minor-version producer may add fields an older reader does not yet understand — unlike an unrecognized `flags` bit, which the flat-payload rule above always rejects (D4): growth is additive on both encodings, but a reader's obligation to notice it differs between them.

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
| `vosu_publish_batch_header` | 16 | 0: handle(8), 8: entries_offset(4), 12: entry_count(4) |

**Session identity.** A session has two identities that are deliberately not interchangeable: the **public id**, an opaque string minted by core (the naming contract's session id, also what `docs/v2/api-v1.md` addresses sessions by), and the **handle**, a `vosu_session_id` (`uint64_t`) scoped to one plugin instance. `VOSU_OP_SESSION_ATTACHED` (§6.2) delivers both; every later message that names a session — flat or CBOR — uses the handle, never the string, with `0` reserved to mean "no particular session" or "every session of this plugin" where an opcode's description says so.

## 4. Threading model and blocking

The host serializes calls into one plugin instance (`dispatch`) onto one thread per instance by default, matching the existing single-worker-per-instance model. A plugin's manifest may declare `concurrent_dispatch = true` (§8) to opt into concurrent calls, which requires its `dispatch` implementation to be internally synchronized; the host then dispatches from its own worker pool instead of one dedicated thread. The one exception to per-instance serialization, regardless of `concurrent_dispatch`: a `VOSU_OP_READ_PLAN_RESULT` answering a `VOSU_READ_PLAN_SYNC` submission (§6.3, D5) is always delivered on the submitting thread itself, concurrently with whatever this instance's own serialized `dispatch` thread (or worker pool) is doing at that moment; a plugin using `VOSU_READ_PLAN_SYNC` is responsible for its own synchronization across that one concurrent delivery.

**Host lock order (A2b, extended by A3/R36).** When acquiring multiple host
locks, core must use
`focus -> state_serial -> instances -> sessions -> api_readers -> registry -> resources -> subscriptions -> invocations -> jobs`.
`api_readers` owns public API watch registrations and their latest snapshots;
`registry` is the existing state/provider registry; `resources` is reference
membership, pending resource operations and stream ownership; `invocations`
includes relay slots/outboxes. Sub-locks within a position are never nested.
Prepare publication bindings before committing state and resource membership
under `state_serial`; notify only after releasing the membership locks.
`api_committed` acquires `api_readers` under `state_serial` to serialize latest
API snapshot notifications. Lifecycle/focus snapshots acquire `api_readers`
after `sessions` while the completed transition is still serialized by `focus`.
API demand refresh runs after releasing `api_readers` and its later locks.
Reserve pending requests,
invocations and job slots under locks, release all locks before dispatch/plugin
callbacks, and reacquire only to complete. No callback, filesystem I/O, worker
join or await runs under these locks; completions cannot acquire an earlier
position while holding a later one. Cross-domain teardown gathers work first,
then cancels/closes/notifies after unlock.

- `dispatch` may block; for an in-process plugin it runs on a host worker thread dedicated to that instance (never the state-bus publisher thread, so a slow plugin cannot stall [ADR 0002](../decisions/0002-immutable-latest-only-snapshots.md)'s publication), and for a `process`-runtime plugin it runs inside `vosu plugin-host`, which shares no state with core — except, in both runtimes, a `VOSU_READ_PLAN_SYNC` result, delivered on the submitting thread itself rather than this dedicated thread (above). Even so, `dispatch` should return quickly for the high-frequency notification opcodes (`VOSU_OP_DEMAND_CHANGED`, `VOSU_OP_FOCUS_CHANGED`, `VOSU_OP_TIMER_FIRED`, `VOSU_OP_READ_PLAN_RESULT`, `VOSU_OP_SETTINGS_CHANGED`): real work triggered by one of these must be handed to the plugin's own thread and reported back asynchronously (another publish, another action result), never performed inline, because these can arrive often enough to queue up behind a slow handler. Opcodes that represent an actual unit of work (`VOSU_OP_ACTION_INVOKE`, `VOSU_OP_RESOURCE_RESOLVE`, `VOSU_OP_TREE_LIST`/`TREE_LOOKUP`, `VOSU_OP_STREAM_READ`, `VOSU_OP_SCAN_REQUEST`'s eventual result) may legitimately take longer, at the cost of only that one plugin instance's own throughput.
- `create` may block briefly (it runs before the plugin is considered loaded) but must not perform unbounded I/O; a plugin needing slow first-run setup starts it in its own thread from `create` and reports readiness through normal state publication.
- `destroy` must return within the host's configured shutdown grace period (matching the 5-second tool-service shutdown precedent in [`docs/tools/protocol.md`](../tools/protocol.md)) or the host proceeds without it: for a `process`-runtime plugin, `vosu plugin-host` is killed; an in-process plugin that does not return leaves the daemon process exit blocked, an accepted cost of the trust an `in-process` grant represents.
  Plugin destroy joins lookup workers unconditionally, within a 4.5-second
  budget; a worker is not left running merely because its lookup is still
  pending (A2b).

## 5. Error model

Every `dispatch`/`send` call returns a `vosu_status`. `VOSU_OK` means the message was accepted for processing, not that any requested effect already completed — opcodes with a result (read plans, scans, resource/tree/stream operations, actions) report that result asynchronously through a later message, correlated by `correlation_id` (or, for read plans, by `plan`, since one submission's results may legitimately return out of order relative to a later submission under `concurrent_dispatch`), except a `VOSU_READ_PLAN_SYNC` submission (§6.3, D5), whose result `send` itself delivers before returning. A non-`VOSU_OK` status means the call itself was rejected and produces no later message:

| Status | Meaning |
| --- | --- |
| `VOSU_ERR_INVALID_ARGUMENT` | Malformed payload, an offset/length outside the buffer, a read plan over `VOSU_READ_PLAN_MAX_BYTES`, or a submission that would exceed an outstanding-request cap (64 asynchronous read plans §6.3, 64 scans §6.4, 64 cross-plugin action calls §6.12, 64 timers §6.11; for a `process`-runtime plugin, also the 64 concurrent status/`SYNC` waiters of §7.6, refused before any frame is written) |
| `VOSU_ERR_PERMISSION_DENIED` | An opcode this plugin's trust level cannot use (e.g. a read plan touching a session it did not match) |
| `VOSU_ERR_NOT_READY` | The target session has no live process |
| `VOSU_ERR_TARGET_GONE` | A session died between submission and execution |
| `VOSU_ERR_UNSUPPORTED` | An opcode the receiver's minor version predates |
| `VOSU_ERR_BACKEND` | An underlying OS failure |
| `VOSU_ERR_CANCELLED` | The call answers an invocation, stream, or plan that was already cancelled or closed by the other side |
| `VOSU_ERR_TIMED_OUT` | The call exceeded a host-enforced deadline (an action invocation, a slow stream read, or plan execution) |

**UnknownHandle (A2b).** A session handle that was issued to this plugin
instance and then retired returns `VOSU_ERR_TARGET_GONE`; one that was never
issued to it returns `VOSU_ERR_INVALID_ARGUMENT`. This distinction also applies
to `STATE_SUBSCRIBE` (§6.12), rather than treating a retired handle as an
unexposed handle.

Per-read failure inside an otherwise-accepted plan is reported per read (`vosu_read_result_kind`, mirroring `src/process.rs`'s `MemoryError`/`ReadStatus` vocabulary) rather than failing the whole plan.

## 6. Host services

### 6.1 Process matching

A game plugin declares, in its manifest `[match]` table (§8), the process matchers core uses to recognize candidate instances for [ADR 0001](../decisions/0001-single-daemon-multi-instance-lifecycle.md) discovery. This table is the schema `docs/v2/catalog-v2.md`'s `matcher` JSON object mirrors verbatim:

```toml
[match]
process_names = ["osu!.exe"]          # exact executable file names
process_path_patterns = []             # regular expressions matched against the full executable path
command_line_patterns = []             # regular expressions matched against the joined argv
comm_hints = []                        # optional, default []; Linux-only substrings of at most 15 bytes, kernel/CN_PROC pre-filter only (D18)
linux_name_contains = []               # optional, default []; Linux-only userspace substring matcher against `name`, 1-15 bytes each (A1b/DB)
```

The JSON mirror (used in `catalog-v2.json` plugin entries) is `"matcher": {"process_names": [...], "process_path_patterns": [...], "command_line_patterns": [...], "comm_hints": [...], "linux_name_contains": [...]}` with identical field names and semantics. `linux_name_contains` is additive, exactly as §1's ABI-struct growth and §3's CBOR-message growth are: a new optional manifest field with no change to any existing field's meaning, so it needs no ABI major bump and no host-side change to recognize `comm_hints` or `process_names`; a host build that predates it simply has no matcher that declares it, the same "unknown field, no behavior change" shape §3's CBOR rule describes. Precise matching rules:

- Patterns use Rust [`regex`](https://docs.rs/regex) crate syntax and are **full-match anchored** (as if wrapped in `^(?:…)$`); a pattern that only needs to match part of a path must spell out its own wildcards.
- `process_names` and `process_path_patterns` are compared **case-insensitively on Windows** and **case-sensitively on Linux**, matching each platform's own filesystem semantics.
- Path separators in both the candidate path and the pattern are normalized to `/` before matching, so one pattern works on both targets.
- `comm_hints` (D18, **Linux only**; accepted but unused on Windows, which has no comm/CN_PROC pre-filter): substrings of at most 15 bytes (the kernel/CN_PROC `comm` field width) used only to fill the bounded in-kernel and CN_PROC process-event pre-filter alongside `process_names`; full matching, including every `process_path_patterns`/`command_line_patterns` check, always runs in userspace against the candidate's full path and argv regardless of what the pre-filter already narrowed. `comm_hints` itself never accepts a process on its own — it only shapes which execs the pre-filter forwards for userspace matching.
- `linux_name_contains` (A1b/DB, **Linux only**; accepted but ignored on Windows, which keeps 1.x's exact, case-insensitive `process_names` match there): entries are 1 to 15 bytes, compared case-sensitively. A process whose `name` (its kernel `comm` value) *contains* an entry matches — a **userspace matcher in its own right**, reproducing 1.x's Linux substring name rule, which exact `process_names` cannot express. Every `linux_name_contains` entry also feeds the kernel/CN_PROC pre-filter exactly like a `comm_hints` entry (same 15-byte width, same 256-entry bound below), so declaring a name here instead of only in `comm_hints` never widens the pre-filter.
- M6a Q6 (approved 2026-11-24): document validation on **every** platform
  requires each `comm_hints` and `linux_name_contains` entry to contain
  1–15 UTF-8 bytes and no NUL. Windows ignoring their runtime semantics
  does not exempt malformed packaging. Shared validation checks regex
  syntax with the runtime's anchored Rust-regex policy; it does not apply
  enabled-set overlap or the 256-entry registration bound to a catalog.
- A `process_names` entry itself longer than 15 bytes can never match the pre-filter's `comm` field on its own (osu!'s own names fit); such a plugin needs a `comm_hints` or `linux_name_contains` substring to appear in the pre-filter at all, or it is treated as unhinted (below). The combined pre-filter table across every enabled game plugin on Linux is bounded to 256 entries (`process_names` entries that fit plus `comm_hints` and `linux_name_contains` entries); a plugin whose hints would push the enabled total past that bound is refused with `VOSU_ERR_INVALID_ARGUMENT` at manifest validation, in enable order — it is never silently dropped or truncated. If any enabled game plugin declares `process_path_patterns` or `command_line_patterns` without also declaring `comm_hints`/`linux_name_contains` (or a `process_names` entry within the 15-byte limit), the pre-filter widens to every exec on the system — discarding the narrowing `AGENTS.md:130` requires keeping — and the dashboard names which installed plugin caused the widening.
- Overlap between two plugins' matchers is checked **statically** on `process_names`, `comm_hints`, and `linux_name_contains` (regex overlap on `process_path_patterns`/`command_line_patterns` is undecidable in general, D18): two matchers overlap when
  - a `process_names` entry is equal (under each platform's case rule) between them;
  - a `comm_hints` or `linux_name_contains` entry of one plugin is equal to, or a substring of, a `process_names` entry of the other (a process matching that exact name would also match the entry's substring rule);
  - a `comm_hints` or `linux_name_contains` entry of one plugin is equal to, or a substring of, a `comm_hints` or `linux_name_contains` entry of the other.

  This is exactly the pre-A1b D18 hint-overlap rule, with `linux_name_contains` treated like `comm_hints` for this static check (not the pairwise "could both match some 15-byte `comm`" rule an earlier revision of this section used: that made almost any two short entries statically conflict, e.g. a third-party plugin's `linux_name_contains = ["mygame"]` against `vosu.osu`'s `"osu!"`, since `"osu!mygame"` is 15 bytes or fewer — rejecting most real plugin pairs outright). Static overlap stays a cheap, conservative early refusal for the obvious case (one matcher's hint is already inside the other's exact name or hint); the runtime rule below is what actually catches two matchers that both accept the same real process, including a case like `"osu!"` and `"lazer"` sharing no containment yet occasionally both matching a real `comm` such as `osu!lazer`.

  Two plugins must not declare overlapping matchers by this definition while **both are installed and enabled**; the host refuses to start the second with `VOSU_ERR_INVALID_ARGUMENT` surfaced at manifest validation, exactly as before. A catalogued-but-not-installed plugin's matcher never triggers this refusal — it would otherwise block installing a replacement game plugin ([ADR 0022](../decisions/0022-install-resolver-and-2-0-transition.md)). At runtime, any process two enabled plugins' matchers both accept — which the static check above cannot always catch, since it does not attempt `process_path_patterns`/`command_line_patterns` overlap — goes to whichever plugin was installed earlier, and core logs the collision once rather than refusing either plugin.

A matched process becomes a candidate instance exactly as 1.x client detection does; acceptance, generation assignment, and PID-order retention are unchanged core behavior, now parameterized by which plugin's matchers accepted the process.

Catalog offers reuse these userspace matchers against validated metadata,
without loading/downloading code. Overlapping uninstalled alternatives are
allowed; enabled-set overlap/prefilter limits apply only at registration.
Offers are generation-scoped, bounded and invalidated on process exit/PID reuse
or source/install changes; acceptance uses the ordinary requested install plan,
not automatic approval/trust or plugin attachment (M6b/D18).

### 6.2 Session lifecycle

A **session** (naming contract) is one attached client; its lifecycle, generation, and selection semantics are exactly [ADR 0001](../decisions/0001-single-daemon-multi-instance-lifecycle.md)'s, generalized across game plugins by [ADR 0020](../decisions/0020-game-agnostic-data-model.md). The plugin never opens or closes a session itself — core owns discovery and sends:

- `VOSU_OP_SESSION_ATTACHED` (CBOR): `{"session": string, "handle": u64, "generation": u64, "pid": u32, "bitness": "x86"|"x64", "name": string|absent, "path": string|absent, "command_line": string|absent, "argv": [string]|absent}`. `session` is the public id (naming contract); `handle` is this plugin instance's `vosu_session_id` for every later message. The last four fields are additive, process facts the matcher and core already have (A1b/DA, a minor-version addition: "a receiver accepts and ignores a CBOR map key it does not recognize for a given message, so a minor-version producer may add fields an older reader does not yet understand", no ABI major bump) — `name` is the executable name the matcher saw (Windows: the image file name; Linux: the kernel `comm` value); `path` is the full executable path; `command_line` is the raw OS command line on Windows and the argv joined by spaces on Linux; `argv` is present only on Linux, holding the exact NUL-split argv. A host that predates this amendment sends `SESSION_ATTACHED` without these four keys at all; **absent means unknown**, never the empty string or empty array a present-but-empty value would mean, and a plugin's SDK layer must not collapse the two (an older-host read must not default a missing key to `""`). A plugin whose behavior depends on these fields should declare `host` at least the vosu version that adds them. The plugin may now begin reading and must identify the client (below) before publishing anything else for it. These four fields are omitted entirely — never present with a placeholder — in the `SESSION_ATTACHED` a non-owning subscriber receives (below); `path` would otherwise leak the install directory D2 reserves for the owning plugin.
- `VOSU_OP_SESSION_DETACHED` (CBOR): `{"handle": u64, "reason": "exited"|"lost"|"superseded"|"declined"}`. The plugin must stop reading that session; any read plan it still submits for that handle answers `VOSU_ERR_TARGET_GONE`.

A `SESSION_IDENTIFY` or `SESSION_DECLINE` racing retirement of an already-issued handle is refused with `VOSU_ERR_TARGET_GONE` (§5); the plugin treats that stale lifecycle reply as benign and starts no reader for it. A handle that was never issued (including zero) remains `VOSU_ERR_INVALID_ARGUMENT`.

`bitness` tells the plugin which pointer width a dependent read-plan stage uses for that session (§6.3); it never issues raw syscalls itself to discover it.

**Filling `vosu.client`.** Per [`docs/v2/state-schema.md`](state-schema.md), core fills `vosu.client.session`, `.pid`, `.focused`, and `.plugin` itself from its own bookkeeping (it already has all four). The attaching game plugin fills the remaining four leaves with one message, sent once per session before any other publication for it:

`VOSU_OP_SESSION_IDENTIFY` (plugin → host, CBOR, no reply): `{"handle": u64, "game": string, "variant": string, "name": string, "version": string, "focus_priority": bool|omitted}`. `focus_priority` is game-agnostic — core attaches no meaning to it beyond the Focus rule below, and any game plugin may set it on any session, not only `vosu.osu`'s manager (`docs/v2/api-v1.md` "Focus rule"). Omitting it or sending `false` is equivalent. Core keeps the first session identified with `focus_priority: true` and holds it until that session detaches. A second `SESSION_IDENTIFY` setting `focus_priority: true` while one is already held is **not** refused — refusing it would need a cross-instance check `send`'s local return status cannot make under the process runtime (§7.2), unlike every other refusal this ABI defines. Instead, the session is identified normally and exposed as usual; core simply ignores its `focus_priority` flag, logs once, and keeps the earlier holder focused (1.x's own first-manager-wins behavior).

A session is not exposed anywhere outside core — `/api/v1/sessions`, other plugins, focus selection — until its game plugin sends `SESSION_IDENTIFY` for it. If identification does not arrive within 5 seconds of `SESSION_ATTACHED`, core logs once and keeps the session hidden (it still exists internally, and the game plugin may still identify it later, which exposes it from that point on) rather than exposing a session with no `vosu.client.game`/`.variant`/`.name`/`.version`.

**Declining a match (D6).** `VOSU_OP_SESSION_DECLINE` (plugin → host, CBOR, no reply): `{"handle": u64}`. A game plugin whose matcher accepted a process it never wants to expose sends this instead of `SESSION_IDENTIFY`; declining after identification is refused with `VOSU_ERR_INVALID_ARGUMENT`. The osu! plugin declines truthy `debug-client-id` processes; it identifies `--tournament` as a `stable` session with `focus_priority: true`. Tournament spectators remain ordinary identified sessions with their IPC id in `vosu.osu.session.ipcId`. The stable manager aggregate is published on every stable session through the ordinary state ABI. Lazer pending lobby data and missing scores publish accurate null plus `tourneyAvailability`; only adapters map those facts to omission/errors. Linux conditional fallback chooses an identified variant, never a decline. On decline core detaches the plugin's handle (`reason: "declined"`) and offers a matching next plugin in install order; after no matcher remains the PID generation is dropped. Identification's five-second hidden-session rule still applies.

**Focus.** Which session is focused is decided by the Focus rule in `docs/v2/api-v1.md`, over identified sessions only; this document does not restate it. Core notifies every affected plugin when focus changes (typically two notifications: the session losing it and the session gaining it):

`VOSU_OP_FOCUS_CHANGED` (host → plugin, CBOR, no reply): `{"handle": u64, "focused": bool, "foreground": bool|absent}`. `foreground` is additive (abi:54's minor-revision rule) and is per-session, not per-plugin: `true` when the OS foreground window's pid is this session's own pid, `false` when the OS foreground window belongs to a different pid (including another session this same plugin owns), and absent entirely on a platform where core has no OS-foreground source (non-Windows). Core sends `FOCUS_CHANGED` when either `focused` or `foreground` changes. `foreground` is **not** 1.x `game.focused`, which means *any* tracked osu! process owns the OS foreground, reported on whichever instance 1.x's own selection picked (`reduce_foreground`, `src/instance.rs:681-697`) — with several sessions open, 1.x can report `true` on the selected instance while this message's own `foreground` is `false` for it, because a different session of the same plugin is the one actually in front. A consumer that wants 1.x's exact semantics (`docs/v2/state-schema.md`'s `session.focused` row) ORs `foreground` across every session this plugin owns itself, rather than reading one session's flag alone; M4a's upstream adapter does this.

`SESSION_ATTACHED` carries no `focused`/`foreground` field; a plugin that needs either waits for the first `FOCUS_CHANGED` (sent once, promptly, after the session is identified, even if neither changes relative to the previous state).

**Delivery ordering.** Core sends a plugin instance `SESSION_ATTACHED` for a handle before any other message naming that handle to it — not only to the owning game plugin, but to any plugin a handle is about to be named to for any reason (a `STATE_SUBSCRIBE`/`STATE_UPDATE` targeting that session, an `ACTION_INVOKE`/`ACTION_CALL` carrying it, a `RESOURCE_RESOLVE` for a session-scoped root). A plugin that has no use for session lifecycle itself may simply ignore the fields it does not need.
The owner's final committed state must flush to subscribers before its
`SESSION_DETACHED` is delivered (A2b).

**Non-owning plugins and session events.** `SESSION_ATTACHED`, `SESSION_DETACHED`, and `FOCUS_CHANGED` for a given session go to that session's owning game plugin always, and additionally to any other plugin that subscribed to session events: `VOSU_OP_STATE_SUBSCRIBE` (§6.12) with a selector naming the reserved pseudo-namespace `vosu.sessions` and an empty pointer, exactly as `docs/v2/api-v1.md`'s session-events WebSocket reuses its field-subscription protocol for the same purpose. A non-owning subscriber's `SESSION_ATTACHED` omits `name`/`path`/`command_line`/`argv` entirely, never with a placeholder: `path` would otherwise hand every `vosu.sessions` subscriber the install directory D2 reserves for the owning plugin. Non-owning plugins learn of a session only once it is identified: their `SESSION_ATTACHED` is sent when the owning plugin's `SESSION_IDENTIFY` arrives, not at attach. A subscribing plugin receives the three messages above instead of (not in addition to) a `STATE_UPDATE` for that pseudo-namespace, since they are already typed events; this is the one selector for which `STATE_SUBSCRIBE` changes a plugin's delivered opcode set rather than only its demand.

**Lifecycle backpressure (K6, A2b).** Subscribing or resubscribing to
`vosu.sessions` replays the current identified-session list. An attach and
detach that both remain unseen by the watcher cancel each other. If the
lifecycle backlog exceeds 4,096 undelivered entries, core sends
`SESSION_DETACHED` with `reason: "lost"` for every session already announced
to that watcher, then unsubscribes its lifecycle-session subscriptions for
non-owned sessions only, and logs a WARN. Owned-session subscriptions are
not removed. The watcher can resubscribe to replay the current list.

### 6.3 Memory read plans

A read plan (`VOSU_OP_READ_PLAN_SUBMIT`, flat layout in the header) is one base address (an absolute address or a module name plus offset, resolved by core against that session's module list so the plugin never computes an ASLR-adjusted address itself) followed by an ordered list of **stages**. Each stage has a base — either the plan's own root (stage 0, typically) or a value read by an **earlier** stage's identified op, interpreted as a pointer-width value (per the session's reported bitness) and optionally offset further — and a batch of independent `{offset, length}` ops relative to that resolved base. This expresses both a deep sequential pointer chase (one op per stage, each stage dependent on the previous) and a wide batch of sibling reads from one resolved address (many ops in one stage) in a single submission.

Core executes the whole dependent chain against the exact process that plugin's matcher accepted — never another session's process, even if the plugin names an arbitrary handle — and answers with one `VOSU_OP_READ_PLAN_RESULT` whose `results` are flat, in the same (stage, op) order as the submission: **one submission is one round trip**, regardless of stage or op count. This is the design [`docs/v2/brokered-reads.md`](brokered-reads.md)'s `BrokeredReadBench` (branch `v2/bench`) measured and recommends: a plan that could express only flat, independent reads (forcing the plugin to resolve pointers itself and issue follow-up reads) reproduces that benchmark's "naive" variant, 16–25x slower, and must not ship. The same `vosu_read_plan_submit_header`/`vosu_read_plan_result_header` pair executes unchanged whether the plugin runs in-process (core's own `ProcessMemory` answers it directly on the calling thread) or is brokered (the pair is relayed, byte for byte, to and from `vosu plugin-host` over the channel in §7.2).

A submitted plan whose encoded payload, or whose resulting response payload, would exceed `VOSU_READ_PLAN_MAX_BYTES` (64 KiB) is rejected with `VOSU_ERR_INVALID_ARGUMENT` before any read executes, bounding host allocation per request regardless of what a plugin submits. Per-read outcomes mirror `src/process.rs`'s existing `ReadStatus`/`MemoryError` vocabulary (`VOSU_READ_OK`, `VOSU_READ_NOT_READY`, `VOSU_READ_UNSUPPORTED`, `VOSU_READ_ERROR`) so a game plugin's retry/backoff logic matches today's in-tree readers. A dependent stage whose `ref_op` read a length other than the session's pointer width is a plan validation error (`VOSU_ERR_INVALID_ARGUMENT`), not a silent truncation or zero-extension.

A plugin that no longer needs an outstanding plan's result (its owning session detached, or its own logic moved on) may abandon it with `VOSU_OP_READ_PLAN_CANCEL` (plugin → host, CBOR, no reply): `{"plan": u64}`. Core discards the result if execution finishes after the cancel is processed; a result already queued for this instance is still delivered, and the plugin must treat it as stale (it may return `VOSU_ERR_CANCELLED`; core ignores the status). A cancelled id stays reserved until its execution finishes; resubmitting it earlier is refused with `VOSU_ERR_INVALID_ARGUMENT`. A plugin instance may have at most 64 **asynchronous** plans outstanding at once (submitted without `VOSU_READ_PLAN_SYNC`, awaiting their `READ_PLAN_RESULT`); a `READ_PLAN_SUBMIT` that would exceed this bound is refused with `VOSU_ERR_INVALID_ARGUMENT` — the same code an oversized or malformed submission already uses (§3), reused here rather than defining a new one, since both are "this submission cannot be accepted as given" — and submits none of it. This bounds the plugin-host delivery queue's own share of outstanding work-unit replies for this one opcode to 64, on top of whatever other work-unit opcodes (§7.2) are separately outstanding. A plan's slot is freed the moment one of three things happens, whichever comes first: its `READ_PLAN_RESULT` is delivered, the plugin sends `READ_PLAN_CANCEL` for it, or `SESSION_DETACHED` fires for the session it reads — core treats a detached session's still-outstanding plans as implicitly cancelled the same way an explicit `READ_PLAN_CANCEL` would, rather than letting them hold their slot until the plugin restarts.

**Opt-in synchronous reads (D5).** `VOSU_READ_PLAN_SYNC` (`include/vosu_plugin.h`), set in `vosu_message.flags` on a `READ_PLAN_SUBMIT`, guarantees the matching `READ_PLAN_RESULT` is dispatched back on this same submitting thread before `send` returns — directly, for an in-process plugin; through a dedicated ring-reader thread that hands the result to the blocked submitter, for a `process`-runtime plugin under `vosu plugin-host` — under a host-enforced deadline that returns `VOSU_ERR_TIMED_OUT` rather than blocking indefinitely. This is the one case where `send` itself may deliver the reply synchronously, concurrently with an ordinary `dispatch` call already in flight on this plugin instance's own serialized thread (§4); every other opcode keeps the asynchronous, serialized contract above, and a plugin using this flag must treat its own state as accessed from both threads for the duration of the call. A plugin submits a synchronous plan from its own worker thread only: submitting one from any thread the host itself dispatches onto — a `dispatch` invocation of any kind, or the state-bus publisher thread — is rejected with `VOSU_ERR_INVALID_ARGUMENT` in both runtimes, since blocking there would either deadlock a lock the plugin holds across `send` or stall publication. The Rust SDK routes a flagged result to a per-plan slot before user code ever sees it through `on_message`, and gates `PlanMemory` (a synchronous-looking read API built on this flag) behind the unpublished `unstable-sync-reads` feature; the osu! plugin is measured under the process runtime with this feature in M5 before any third party is offered it.

The current host uses a 2 s execution budget for fresh synchronous plans, checked
between dependent stages and during individual-op retries after a failed batch.
These checks use a coarse monotonic clock (~16 ms on Windows; typically 1–4 ms on
Linux), not capture timestamps. Overrides are clamped to at least 64 ms (four
Windows ticks). A successful single-stage synchronous plan makes no time query:
native reads cannot be preempted by a same-thread deadline check, and the bounded
64 KiB stage is already a finite backend batch sequence. A failed batch starts a retry budget.
The public `execute_into` executor retains its precise absolute-deadline contract.

### 6.4 Signature scanning and module enumeration

`VOSU_OP_SCAN_REQUEST` (plugin → host, CBOR): `{"session": handle u64, "module": string|null, "pattern": string, "non_zero_mask": bool, "all_matches": bool}`. `pattern` uses exactly `src/process/scanner.rs`'s `Pattern::parse` grammar (space-separated hex byte pairs and `??` wildcards). `module` scopes the scan to one module's readable regions; `null` scans every readable region of the process. `VOSU_OP_SCAN_RESULT` (host → plugin, CBOR, reply correlated by `correlation_id`): `{"addresses": [u64, ...], "error": {"code": string, "message": string}|null}` — `addresses` holds absolute target-process addresses, suitable as a later read plan's `VOSU_BASE_ABSOLUTE`, ordered ascending by memory region then ascending by offset within each region — the order core's scanner already produces, which a plugin's `scan_first` convention (taking the first address) depends on; a scan that cannot complete (its session detached mid-scan, or an unreadable target) always answers …

### 6.5 Declared file roots and resource resolution

Resources follow [ADR 0021](../decisions/0021-api-v1-and-upstream-adapters.md) and are routed by [`docs/v2/api-v1.md`](api-v1.md); this section defines only the plugin-facing protocol core uses to mint and later resolve a reference the plugin published.

**File roots.** A game folder's real path is known only at runtime, from the attached process, so the manifest (§8) names root **slots** only — bare strings, no path — and the plugin registers each slot's actual path once it knows it:

`VOSU_OP_ROOT_DECLARE` (plugin → host, CBOR, no reply): `{"handle": u64|null, "name": string, "path": string}`. `name` must be one of the manifest's declared `root_slots`; `path` is an absolute, host-native filesystem path. `handle: null` declares a plugin-wide root (most game installations have one songs/maps folder regardless of session); a non-null handle scopes the root to one session, for a plugin that can observe a different path per attached process. Core opens the declared path with [ADR 0005](../decisions/0005-root-confined-file-serving.md)'s retained-handle confinement and keyed storage; a later resolve reply naming a root the plugin never declared, or a `path` that escapes it, is refused rather than trusted. When both a session-scoped and a plugin-wide root share `name`, the session-scoped one takes precedence for that session.

**Minting a reference's id.** A `VOSU_VALUE_RESOURCE_REF`/`VOSU_VALUE_TREE_REF` (§6.6) carries `{"kind": string, "key": <CBOR>, "name": string, "content_id": <CBOR>}`: `kind` and `key` are the plugin's own lookup key (opaque to core, echoed back unchanged in a later resolve request); `name` is a file name whose extension both tells core which allowlisted type to assign ([`docs/v2/api-v1.md`](api-v1.md) owns the allowlist) and **is the published type for the lifetime of that id** — a single resource is typed once, at publish time, from `name`, and the served `Content-Type` always equals that published type, never a type derived later from wherever the resolved file actually lives; `content_id` is the content identity (a store hash, or a path+size+modification-time digest). Core mints the public id **the moment the field is published** — with no round trip to the plugin — from the inputs [`docs/v2/api-v1.md`](api-v1.md)'s id registry defines; the plugin only answers a resolve request the first time something actually requests that id's bytes or index, which may be much later or never.

**A3/R14/R17 identity encoding.** The token authenticates version, reference
class, plugin identity and the tuple, excluding all transient session, PID,
handle and instance identities. Encode the tuple as the CBOR array
`[kind, key, name, content_id]` using RFC 8949 §4.2.1 core deterministic
encoding: definite lengths, shortest integers/lengths and preferred shortest
floating representation (canonical NaN), recursively bytewise-lexicographically
sorted encoded map keys. Reject duplicate map keys with equal deterministic
encodings before minting, rather than overwriting them; reject unsupported
CBOR values rather than inventing an identity. Retain the original opaque key
for dispatch; canonical encoding is for identity only. Multiple current owners
of an identical token are tracked separately; only loss of the last owner
retires it (or explicit bounded-registry eviction). Stable mutable files use
an immutable snapshot: changed bytes need a new id, old bytes or 410 under the
old id, as [API v1](api-v1.md#id-registry) requires.

**Resolving a reference.** A resource or tree reference is always published within a session (§6.6), so core always has one to name: on the first `GET`/`HEAD` for a minted id (or its tree index), core asks the plugin to resolve the `{kind, key}` it published, naming that session's handle so the right root is chosen — the session's own declared root first, then a plugin-wide root of the same name, per "File roots" above:

- `VOSU_OP_RESOURCE_RESOLVE` (host → plugin, CBOR): `{"handle": u64, "kind": string, "key": <CBOR>}`.
- `VOSU_OP_RESOURCE_RESOLVED` (plugin → host, CBOR, reply correlated by `correlation_id`; this is also the reply opcode for `TREE_LIST` and `TREE_LOOKUP` below — the host already knows which request a reply answers from its own pending-request table keyed by `correlation_id`, so one reply opcode suffices): `{"root": string, "path": string}` (a declared root plus a relative path inside it) or `{"stream": u64, "size": u64}` (a `vosu_resource_id` naming a bounded byte stream of the given total size, served through `STREAM_READ`/`STREAM_CHUNK` below — mandatory for a `process`-runtime plugin generating content, since it may not even share filesystem visibility with a root core could open directly) or `{"error": {"code": string, "message": string}}`.
- `VOSU_OP_TREE_LIST` (host → plugin, CBOR): `{"handle": u64, "kind": string, "key": <CBOR>}` — list a tree's index. Reply (`RESOURCE_RESOLVED`): `{"entries": [{"path": string, "size": u64, "hash": string}], "truncated": bool, "revision": string}`, matching `docs/v2/api-v1.md`'s tree-index body; an entry with unknown content hashing omits `hash` entirely rather than sending it `null`. The whole reply is capped at `VOSU_FRAME_MAX_BYTES` (1 MiB, §3/§7.2): a plugin whose own listing is larger truncates its `entries` to fit and sets `truncated: true`, the same flag an index already uses for an incomplete listing, rather than the host ever splitting one `RESOURCE_RESOLVED` into several messages. Core assigns each entry's `type` itself, from `path`'s extension against the same allowlist as a single resource, when projecting the index to HTTP — the plugin never computes or sends a MIME type.
- `VOSU_OP_TREE_LOOKUP` (host → plugin, CBOR): `{"handle": u64, "kind": string, "key": <CBOR>, "path": string}` — resolve a requested alias/fallback with plugin-owned matching. A successful `RESOURCE_RESOLVED` reply is either `{"root": string, "path": string, "entry_path": string}` or `{"stream": u64, "size": u64, "entry_path": string}`. `entry_path` is the canonical logical path in the previously validated cached tree index, not the physical root-relative path. Core requires membership, derives type from that indexed path, and still applies declared-root confinement. Omitted entries of a truncated index cannot be served. Error replies retain the ordinary error shape.
- `VOSU_OP_STREAM_READ` (host → plugin, CBOR): `{"stream": u64, "offset": u64, "len": u32}` — read one byte range of a previously opened stream, `len` itself bounded by `VOSU_STREAM_CHUNK_MAX_BYTES` (`VOSU_FRAME_MAX_BYTES` − 64 bytes, `include/vosu_plugin.h`), strictly below the universal frame cap so a full-length reply's own CBOR map/key/byte-string-header overhead can never itself push the message over it; ranges may be requested in any order. `VOSU_OP_STREAM_CHUNK` (plugin → host, CBOR, reply correlated by `correlation_id`): `{"data": bytes}` (fewer bytes than requested, including zero, signals end of stream — a plugin whose source is larger than the requested range never answers with more than `len` bytes, keeping the reply within `VOSU_STREAM_CHUNK_MAX_BYTES`'s margin) or `{"error": {...}}`.
- `VOSU_OP_STREAM_CLOSE` (host → plugin, CBOR, no reply): `{"stream": u64}` — core no longer needs this stream (evicted from its bounded resource registry, or the request that opened it ended); the plugin releases whatever backs it, and core drops any cached resolution for the id that stream answered, so the next request resolves it again from scratch rather than reusing a now-dead stream. A `STREAM_READ`/`STREAM_CHUNK` exchanged for a stream after its `STREAM_CLOSE` answers `VOSU_ERR_CANCELLED` instead of proceeding.

A3/R18: each resolved stream is a request-owned lease, never a shared cached
stream handle. Completion, cancellation, HTTP disconnect, failure, timeout,
detach, instance loss and shutdown release pending slots and close each live
lease exactly once where delivery remains possible. Close invalidates its
resolution; another request must resolve a fresh lease and cannot be closed
by this request. A short/zero chunk is EOF, never fabricated bytes.
Pending resource/list/lookup/read requests are bounded, reserved before
dispatch, correlated to instance, request kind and generation, and removed on
every terminal path. Duplicate, late and wrong-instance replies cannot satisfy
another request; host-generated request ids are never reused within an instance.

AF.12 (approved 2026-11-24): this session-scoped mechanism is unchanged. A
plugin-wide root does not mint a reference, and an `ACTION_RESULT` value is
never registered as a resource. Sessionless or protected outputs use the
separate invocation-owned artifacts of §6.13, never a fabricated session or
handle 0.

A game plugin declares which common-namespace `field:` ids it fills in its manifest's `covers` list, and which core resource kinds it fills as reserved-name entries in its `[[resource_kinds]]` table (§8); [`docs/v2/catalog-v2.md`](catalog-v2.md) defines both declarations and what each covers, and this document does not restate them. Its own namespace's fields and resources need no separate coverage declaration, since publishing them is already licensed by `provides`. A `[[resource_kinds]]` entry also bounds which MIME-type families that kind may be typed as; [`docs/v2/api-v1.md`](api-v1.md) owns the allowlist itself (the family tokens are exactly `image`, `audio`, `video`, `chart-text`). The bare kind names `file`, `audio`, `background`, `set`, and `skin` always address the core kinds (`resource:chart.file`/`.audio`/`.background`/`.set`, `resource:skin`); they are forbidden only as the name of a plugin's *own* kind, which must use any other bare name and becomes `resource:<plugin-id>.<kind>` only inside a capability id (`docs/v2/catalog-v2.md` owns this grammar).

### 6.6 State publication to namespaces

A plugin publishes typed values with `VOSU_OP_PUBLISH_FIELD` (§3's flat layout): a handle, a namespace (e.g. `vosu.osu`), an RFC 6901 pointer below it (e.g. `/play/pp/current`), and one value, the whole message capped at `VOSU_FRAME_MAX_BYTES` (1 MiB, §3/§7.2) like every opcode — a `VOSU_VALUE_CBOR` array or object too large to fit is rejected with `VOSU_ERR_INVALID_ARGUMENT`, since one field's value is published atomically and has no defined way to split. Core applies [ADR 0002](../decisions/0002-immutable-latest-only-snapshots.md)'s immutable, latest-only publication per session: a plugin publishes whenever a value changes (or at its own sampling cadence while demand exists, §6.7); it never assembles or sends a whole namespace document, and never serializes to JSON itself — projection into each served API is core's job, driven by the JSON Schema each namespace publishes ([`docs/v2/state-schema.md`](state-schema.md)).

A scalar value (`VOSU_VALUE_BOOL`/`I64`/`U64`/`F64`) or a string/byte leaf (`VOSU_VALUE_STRING`/`BYTES`) replaces exactly the value at `pointer`. An array or object — `vosu.play.judgements`, `vosu.osu.keys.all`, and similar — has no leaf-by-leaf wire representation: it is published whole, as `VOSU_VALUE_CBOR`, a CBOR-encoded subtree that replaces the entire value at `pointer` in one message. A resource or tree reference is `VOSU_VALUE_RESOURCE_REF`/`TREE_REF`, trailing CBOR `{"kind": string, "key": <CBOR>, "name": string, "content_id": <CBOR>}` exactly as §6.5's "Minting a reference's id" describes; `key` is any CBOR value the plugin chooses and later receives back unchanged in a `RESOURCE_RESOLVE`/`TREE_LIST`/`TREE_LOOKUP` request, so a plugin may use it as its own lookup key (a file path, a database row id, anything).

`vosu.client` is the one namespace a plugin never publishes to directly (§6.2 fills it from the attach handshake and `SESSION_IDENTIFY`). A game plugin publishes a common-namespace field or a core resource kind only when its coverage declarations (`covers` for fields, a reserved-name `[[resource_kinds]]` entry for core resource kinds, both defined in `docs/v2/catalog-v2.md`) include it; a plugin's own namespace needs no such declaration, since `provides` already licenses it. A covered field the plugin genuinely cannot read for the current session is published as `VOSU_VALUE_NULL`, never a placeholder zero or empty value ([ADR 0020](../decisions/0020-game-agnostic-data-model.md)). Reference maps are the one exception to "NULL replaces the value": publishing `VOSU_VALUE_NULL` at a member of `vosu.chart`'s `/resources` object (for example `/resources/background`) removes that key, which is how a kind the current chart has no file for is omitted. `vosu.skin`'s `/tree` is an ordinary nullable field and is published as `VOSU_VALUE_NULL` when there is no skin tree.

**Publication edge cases (R19).** A publish for a handle before this
plugin's own `SESSION_IDENTIFY` for it, or for a handle unknown to this
plugin instance, is refused with `VOSU_ERR_INVALID_ARGUMENT`; a detached
handle is refused with `VOSU_ERR_TARGET_GONE`; a handle owned by another
plugin is refused with `VOSU_ERR_PERMISSION_DENIED`. Core creates any
missing intermediate object a pointer needs; a pointer that descends
through an existing array or scalar is refused (`VOSU_ERR_INVALID_ARGUMENT`),
since arrays are published whole (above). `VOSU_VALUE_NULL` at `/resources`
itself sets `{}`, not an absent key. `VOSU_VALUE_RESOURCE_REF`/`TREE_REF`
entries are refused with `VOSU_ERR_INVALID_ARGUMENT` until M3. A pointer is
at most 512 bytes and 32 decoded RFC 6901 tokens (matching 1.x's own
subscription limits, `src/server/v2_subscription.rs:237-247`; api:154); a
`VOSU_VALUE_CBOR` value is at most 32 levels deep, its map keys are text,
and it carries no CBOR tags — any violation is `VOSU_ERR_INVALID_ARGUMENT`.
JSON projection writes a non-finite `f64` as `null` (matching JS
`JSON.stringify`) and `VOSU_VALUE_BYTES` as standard base64; no schema in
this freeze declares a bytes field.

**Commit budget (K3, A2b).** The resulting committed state is bounded to
4 MiB per namespace and 65,536 keys per session. A `PUBLISH_FIELD` or
`PUBLISH_BATCH` that would exceed either budget returns
`VOSU_ERR_INVALID_ARGUMENT` and commits nothing; these retained-state
budgets are separate from the per-message limits.

**Atomic multi-field publication (D4).** `VOSU_OP_PUBLISH_BATCH` carries a handle plus an array of the same per-field entries `PUBLISH_FIELD` carries (`vosu_publish_batch_header`, §3's flat layout), for a plugin that must change several fields as one atomic unit (`src/state/worker.rs`'s regular and precise readers running concurrently is the motivating case: a batch keeps one reader's commit from interleaving with the other's). The whole encoded payload is capped at `VOSU_PUBLISH_BATCH_MAX_BYTES` (1 MiB, matching the WebSocket/frame limit `docs/v2/api-v1.md` "Listener placement, CORS, and limits" already enforces, since a batch's `VOSU_VALUE_CBOR` entries are not bounded the small fixed-width way a read plan's ops are). The whole batch is validated and committed as a whole, with every one of the following rejecting the entire batch and applying nothing: a malformed entry; any entry's own `handle` differing from the batch's own `handle` (every entry in one batch names the same session); an entry that would itself be semantically rejected as a standalone `PUBLISH_FIELD` (an uncovered field, or `vosu.client`, which a plugin never publishes directly); or two entries sharing an `ns` whose `pointer`s overlap — equal, or one a `/`-segment prefix of the other (e.g. `/play` and `/play/combo`), since the ancestor's value would cover the descendant's own write — with no defined ordering or last-write-wins rule; an overlap is rejected, never resolved. Otherwise a single `VOSU_OK` answers the message, never a per-entry status, and core never holds a partially-applied batch as pending state between entries. A plugin that only ever changes one field at a time keeps using `PUBLISH_FIELD`; `PUBLISH_BATCH` is for the case where several fields must become visible together or not at all.

**Plugin-only fields (D2).** A namespace schema may mark a subtree `"x-visibility": "plugins"` ([`docs/v2/state-schema.md`](state-schema.md) owns the full rule; this is a summary); only `vosu.osu`'s top-level `internal` key uses it in this freeze, and only to carry per-variant install paths and the current chart's/active skin's folder and file names (D8, R8). A plugin publishes it exactly as any other field — the ABI itself draws no distinction. Two different consumers see it differently: core's projector strips it from every API v1 document, subscription frame, encoding cache, and debug log **unconditionally**, regardless of any `requires`/`optional` declaration — a tool's API-session scope never includes it, however it is declared. A **plugin's own ABI reads** — a `STATE_UPDATE` this plugin receives as a non-owning subscriber (§6.12), or a bundled plugin's `vosu-surface` read (§7.5) — are instead gated by that plugin's manifest `requires`/`optional` naming `field:vosu.osu.internal` or a descendant of it; a whole-namespace `field:vosu.osu` requirement does not grant it, since that would let any ordinary consumer of the namespace read it by depending on the namespace as a whole. This is disclosure control, not confinement: an out-of-process plugin already has the user's own filesystem access (§7), so hiding the value from API v1's *wire* is what D2 buys, not hiding it from a plugin process that already has the equivalent access another way.

### 6.7 Demand signals

Core tracks, per session and per (namespace, pointer) selector, whether any subscriber — a WebSocket subscriber of [`docs/v2/api-v1.md`](api-v1.md)'s field-subscription protocol, or another plugin's `STATE_SUBSCRIBE` (§6.12) — currently needs it, generalizing 1.x's selected-field subscription hub. A one-shot HTTP read and an action invocation create no demand. `VOSU_OP_DEMAND_CHANGED` (§3's flat layout) tells the plugin the session's **complete current demand**, as a snapshot, whenever it changes — not a single field's transition. A plugin always samples every field it fills at its configured baseline (regular) cadence; there is no signal in this protocol that permits skipping a field entirely, regardless of subscriber count.

- `regular`/`precise`: host-computed acceleration gates, each the union, across every currently selected (namespace, pointer) pair for this session, of that field's declared `x-sampling-lane` (`docs/v2/state-schema.md#sampling-lanes`) — `regular` unions the `regular`/`both` lanes, `precise` the `precise`/`both` lanes — after core's own gameplay-status gating is already applied (`docs/v2/state-schema.md`'s `activity: "gameplay"` rule: `vosu.status.status` is `play` or `pause`). A plugin never reads `x-sampling-lane` itself to decide its own gating; core's compiler is its only consumer. `regular`/`precise` tell a plugin when to sample *above* its baseline, never whether to sample at all.
- `selectors`: the session's full current set of subscribed (namespace, pointer) pairs, so a plugin with several independently accelerated fields knows exactly which ones currently have a subscriber, since the two lane flags alone only say whether *any* do.

### 6.8 Actions

An action's declaration — name, version, display title, argument/result JSON Schemas, whether it is session-scoped, and whether it is CLI-exposed — is an `[[actions]]` manifest entry defined by [`docs/v2/catalog-v2.md`](catalog-v2.md); this document only specifies the wire messages that invoke one.

Core forwards an invocation with `VOSU_OP_ACTION_INVOKE` (host → plugin, CBOR): `{"invocation": u64, "action": "<plugin-id>.<name>", "session": u64|null, "caller": {"kind": "tool"|"plugin"|"cli", "id": string}, "args": <CBOR>}`. A3/R1 makes `action` mandatory, the full bare id used by `ACTION_CALL` and HTTP, never prefixed `action:`. Lookup is by exact registered id, never a last-dot split or a single-action fallback. `session` is null for `session="none"`; `caller.kind` is `cli` for broker calls, `plugin` for `ACTION_CALL`, and `tool` otherwise.

The callee plugin answers asynchronously, never as `dispatch`'s return value:

- `VOSU_OP_ACTION_PROGRESS` (CBOR, zero or more, no reply): `{"invocation": u64, "fraction": f64|null, "message": string|null}`.
- `VOSU_OP_ACTION_RESULT` (CBOR, exactly once): `{"invocation": u64, "status": "ok"|"error", "value": <CBOR>|null, "code": string|null, "message": string|null}`.

Both are **bidirectional**: plugin → host when the callee is answering core directly (a tool/CLI invocation), and host → plugin when core is relaying the callee's answer to the plugin that issued `VOSU_OP_ACTION_CALL` (§6.12) — core remaps the callee's own `invocation` id back to the caller's id as part of that relay, so each side only ever sees ids it recognizes as its own. `VOSU_OP_ACTION_CANCEL` is bidirectional the same way: host → plugin to ask a callee to stop, and plugin → host for a plugin cancelling its own outstanding `ACTION_CALL` (`{"invocation": u64}`, the caller's own id; core remaps and forwards it to the callee).

Core may request early termination with `VOSU_OP_ACTION_CANCEL` (CBOR): `{"invocation": u64}`; a plugin that supports cancellation stops promptly and still sends exactly one `ACTION_RESULT` with `"status": "error", "code": "cancelled"` (the status `docs/v2/api-v1.md` maps to its own `"state": "cancelled"`). A plugin that ignores cancellation simply runs to completion — core does not forcibly interrupt an in-process plugin's action (it cannot safely do so). For a `process`-runtime callee, core kills `vosu plugin-host` only under §7.6's fixed action-deadline grace (A5.4), never for a cancellation request alone; that kill also ends every other in-flight call to that plugin.

A3/MK/ML: action schemas use the bounded profile in
[catalog-v2.md](catalog-v2.md#bounded-action-schema-profile-a3), compiled once
at load without runtime `jsonschema`. Validate arguments before dispatch and
successful results before terminal publication; invalid results fail with
`invalid_result`. The core invocation policy has a 30-second default deadline,
injected into tests, independent of HTTP waits, with no manifest timeout field.
First accepted terminal result wins; timeout synthesizes `timed_out` even if an
in-process callee continues. Session detach requests cancellation without
retargeting. Public jobs and plugin relay calls share invocation semantics,
not storage: relay calls need no HTTP job. The 64-call relay slot stays reserved
until terminal delivery to the live caller, not cancellation; a bounded terminal
outbox survives dispatch pressure, while progress may coalesce. Caller destruction
explicitly tears down its slots. Use one-shot deadlines and ingress-triggered
retention cleanup, never a periodic sweep.
M3 exercises `plugin_crashed` through the production instance-loss seam.
§7.6 freezes the `process`-runtime deadline grace and loss identity (A5.4);
native process-host crash/kill evidence is M5 X0/N0, not an in-process crash claim.

AF.3 (approved 2026-11-24): work that can outlast the deadline is a
plugin-owned operation behind short `start`/`status`/`cancel` (and, where a
feature already has them, `pause`/`resume`/`retry`) actions; the plugin's
operation id is not the core invocation id ([API v1](api-v1.md#plugin-owned-operations-af3)).
Callbacks only enqueue bounded owned work; no plugin blocks `dispatch` or
holds a host or operation lock across `send`.

**Oversize core-authored action frames (G5/H4).** `ACTION_INVOKE`'s `args` and a relayed `ACTION_RESULT`'s or `ACTION_PROGRESS`'s remapped `invocation` id can each grow the encoded message past `VOSU_FRAME_MAX_BYTES` without any single CBOR value being at fault — a POST body up to API v1's own 1 MiB limit re-encodes larger as CBOR (a JSON `0.1` is 4 bytes, a CBOR `f64` is 9), and remapping a callee's own `invocation` id onto the caller's can widen a CBOR integer by up to 8 bytes. Core never sends such an oversize frame, in either of its two ways of producing an `ACTION_INVOKE`:

- **From an HTTP request** (a tool, CLI, or broker invocation): an `ACTION_INVOKE` core itself would build over the cap answers the originating request with `413 payload_too_large` instead of ever constructing the ABI message ([`docs/v2/api-v1.md`](api-v1.md) "Status codes and error bodies", extended for this case).
- **From another plugin's `VOSU_OP_ACTION_CALL`** (§6.12): there is no HTTP request to answer here, so core instead answers the *calling* plugin directly with an `ACTION_RESULT` error — `{"invocation": u64, "status": "error", "value": null, "code": "args_too_large", "message": string}` (the caller's own `invocation` id) — without ever forwarding the oversize `ACTION_INVOKE` to the callee.

A relayed `ACTION_RESULT` that would land over the cap after remapping is delivered to the calling plugin as `{"invocation": u64, "status": "error", "value": null, "code": "result_too_large", "message": string}` in place of the callee's own message, never forwarded as-is; `value: null` matches `ACTION_RESULT`'s own `<CBOR>|null` shape (§6.8 above) exactly as any other error result already carries it. A relayed `ACTION_PROGRESS` that would land over the cap after remapping is dropped instead: `ACTION_PROGRESS` is already latest-only and coalesced per invocation (§7.2), so dropping one oversize instance loses no information a later, in-bounds `ACTION_PROGRESS` or the eventual `ACTION_RESULT` does not already supersede, and the callee's own call continues unaffected — core sends neither an error nor an `ACTION_CANCEL` for this case, since the callee is still expected to answer normally in its own time.

### 6.9 Settings

A plugin declares its settings shape with `settings_schema` (§8), a path to a `settings.schema.json` sibling using exactly the overlay/tool `settings.json` schema format ([`docs/tools/protocol.md`](../tools/protocol.md)). Core owns storage, the dashboard editing UI, and `password`-type write-only handling. The plugin's effective settings object is part of the `create` vtable function's `init` parameter (`vosu_plugin.h`: CBOR `{"plugin_id", "settings", "data_dir"}`) — no round trip is needed for the initial value — and every later change arrives as `VOSU_OP_SETTINGS_CHANGED` (host → plugin, CBOR, no reply): `{"values": <CBOR map>}`, where `values` is always the complete effective settings object in `init.settings`'s own shape, never a diff of only the changed keys — matching `SETTINGS_CHANGED`'s one-per-instance coalescing (§7.2), which replaces a queued instance wholesale rather than merging it, exactly because the whole object is self-sufficient.

### 6.10 Logging

`VOSU_OP_LOG_RECORD` (plugin → host, CBOR, no reply): `{"level": "trace"|"debug"|"info"|"warn"|"error", "message": string, "fields": <CBOR map>}`. Core attaches its own structured context (plugin id, session, timestamp) and writes to the daemon's existing logging sink. Records are rate-limited per plugin instance (200/second, matching the host's existing bounded-queue discipline); records past the limit are dropped and counted, with the count itself logged once per second while dropping continues, so a runaway plugin cannot grow host memory or CPU use without bound.

### 6.11 Timers

A plugin requests periodic or one-shot callbacks instead of spawning its own OS timer thread, so the host can account for and suspend a plugin's scheduled work exactly as it does reads: `VOSU_OP_TIMER_SET` (plugin → host, CBOR): `{"timer": u64, "interval_ms": u32|null}` (`null` requests a one-shot fire after the host's minimum granularity). The host answers each due timer with `VOSU_OP_TIMER_FIRED` (host → plugin, CBOR, no reply): `{"timer": u64}`. `VOSU_OP_TIMER_CANCEL` (plugin → host, CBOR, no reply): `{"timer": u64}` stops a periodic timer; a cancelled one-shot timer that already fired is a no-op. A `TIMER_SET` for a new id past 64 outstanding timers for this instance is refused with `VOSU_ERR_INVALID_ARGUMENT` (§5), matching the outstanding-request cap on read plans, scans, and cross-plugin action calls; a fired one-shot's id no longer counts against this cap, and `TIMER_CANCEL` always frees it immediately.

### 6.12 Cross-plugin state reads and calls

A plugin reads another plugin's namespace the same way an HTTP client does, generalized to the ABI: `VOSU_OP_STATE_SUBSCRIBE` (plugin → host, CBOR, no reply): `{"session": u64|null, "selectors": [{"ns": string, "pointer": string}, ...]}` (`session: null` with the reserved `vosu.sessions` namespace subscribes to session lifecycle events instead, §6.2). Each `STATE_SUBSCRIBE` **replaces** this plugin's previous selection for the named session wholesale, exactly as 1.x's subscription establishment does — it is not additive; a plugin that wants to add a selector resends the whole set, and sends an empty `selectors` list to unsubscribe entirely from that session. A `session: null` request is valid only when `selectors` is **exactly one entry** naming `{"ns": "vosu.sessions", "pointer": ""}` — a different `ns`, a non-empty `pointer`, zero entries, or more than one entry (even if every entry names `vosu.sessions`) is refused with `VOSU_ERR_INVALID_ARGUMENT` as `send`'s status, nothing delivered (R16); a non-null `session` this instance has not been told about — it neither owns it nor received a `SESSION_ATTACHED` for it through `vosu.sessions` — is refused the same way. An internal grant (`requires`/`optional` naming `field:vosu.osu.internal` or a descendant) covers exactly the named subtree and everything below it, never the whole namespace (R18); a selector under a subtree not granted reports *absent* (`value: null, error: null`), the same as API v1, rather than being refused. Subscribing creates ordinary demand — the targeted namespace's owning plugin receives it through `DEMAND_CHANGED` like any other subscriber, which is how a provider plugin that depends on a game plugin's field gets it sampled. Core answers with `VOSU_OP_STATE_UPDATE` (host → plugin, CBOR), once immediately with the current values and again on every change, latest-only: `{"session": u64, "values": [{"ns": string, "pointer": string, "value": <CBOR>|null, "error": {"code": string, "message": string}|null}], "more": bool}` (an absent field reports `value: null, error: null`, matching the HTTP/WebSocket absence rule). `STATE_UPDATE` is capped at `VOSU_FRAME_MAX_BYTES` (1 MiB, §3/§7.2) like every opcode; core, as its author, splits one update across more than one `STATE_UPDATE` message if a session's full set of changed leaves would otherwise exceed it, setting `more: true` on every part but the last (`more: false`, the common case, on a single-message update); the receiver buffers a `more: true` part and applies the accumulated set only once a `more: false` part completes it, so a subscribing plugin never observes a half-delivered update the way it would a half-applied `PUBLISH_BATCH` (§6.6) — core never splits in the middle of one `values` entry, only between entries. An entry that cannot fit within the cap — whether its own `value` alone is too large, or it simply does not fit once its `ns`/`pointer` keys and surrounding envelope are counted — is never truncated: it carries `value: null` and a sibling `"error": {"code": "value_too_large", "message": string}` (distinct from an ordinary absent value, which carries `error: null` too), and the plugin treats it as it would any other field it cannot currently read.

**Delivery coalescing and ordering (H1/H3/I1).** The plugin-host delivery queue (§7.2) never drops a `STATE_UPDATE`'s data, and holds at most two things *waiting* for one session at once: a split sequence still in flight (core has sent some but not yet all of its parts) and a separate pending **merged update** that has not yet begun delivery. An update still carrying an outstanding `more: true` part is never merged into or across — it is not complete until its own `more: false` part arrives, so it occupies only the in-flight slot, never the merged one. Once an update's last part has arrived (a single-message update is already complete; a split sequence completes when its `more: false` part arrives), it is complete, and merges into the session's pending merged update if one is already waiting: the two are merged by (`ns`, `pointer`) key — the newer entry wins per key, equivalent to replaying both updates in that order — rather than the newer replacing the older wholesale, so a key the newer update left unchanged still delivers the older update's value for it; otherwise it simply becomes the new pending merged update. A merged update leaves the pending slot the moment `dispatch` begins delivering its first re-split part, exactly like any other in-progress sequence, freeing the slot for a new pending merged update to start accumulating later complete updates for that same session while the first is still being delivered. A merged update can exceed `VOSU_FRAME_MAX_BYTES` even though every update that fed it did not, so delivery re-splits it at the same cap into one or more parts exactly as core's own splitting does, setting `more: true` on every part but the last — this is why `STATE_UPDATE`'s `more` flag means "more parts of this delivery," not "more parts as core first sent them." Core and `vosu plugin-host` additionally deliver every `STATE_UPDATE` for one session strictly in the order core produced them — one part's `dispatch` call completing before the next part (or the next update) for that same session is dispatched — even when the plugin declares `concurrent_dispatch` (§4): a session's own `STATE_UPDATE` sequence is always serialized, though two different sessions', or two different opcodes', calls to the same instance may still run concurrently under the worker pool.

A plugin invokes another plugin's declared action with `VOSU_OP_ACTION_CALL` (plugin → host, CBOR): `{"invocation": u64 (caller-assigned; never reused for another call within this plugin instance's lifetime), "action": "<plugin-id>.<name>", "session": u64|null, "args": <CBOR>}`. `action` is the bare `{plugin-id}.{name}` form `docs/v2/api-v1.md`'s routes and `vosu.invoke` use, never the `action:` capability-id prefix, which exists only in manifests and the catalog. Core checks that the calling plugin's own manifest `requires`/`optional` ([`docs/v2/catalog-v2.md`](catalog-v2.md)) names that action's capability id before routing it to the target plugin as `ACTION_INVOKE` with `caller: {"kind": "plugin", "id": "<calling plugin id>"}`; the result and any progress messages are delivered back to the calling plugin as `ACTION_RESULT`/`ACTION_PROGRESS` (§6.8) correlated by its own `invocation` id, exactly as a tool's HTTP invocation would see them, and the calling plugin may cancel its own call the same way (§6.8). A plugin instance may have at most 64 `ACTION_CALL`s outstanding at once (issued, awaiting their relayed `ACTION_RESULT`); an `ACTION_CALL` that would exceed this bound is refused with `VOSU_ERR_INVALID_ARGUMENT`, the same code §6.3 and §6.4 reuse for their own outstanding-request caps. A call's slot is freed only by its relayed `ACTION_RESULT`, never by anything else, so core always produces one eventually: when the callee's own `process`-runtime host crashes or is killed after the §6.8 cancellation timeout, or the callee itself exceeds its action's timeout, core synthesizes `{"invocation": u64, "status": "error", "value": null, "code": "plugin_crashed"|"timed_out", "message": string}` for the calling plugin exactly as `docs/v2/api-v1.md`'s "Job object" synthesizes the same two outcomes for an HTTP-originated job — the caller never sees its call left outstanding by the callee simply disappearing.

### 6.13 Invocation-owned artifacts (AF.12)

Approved 2026-11-24 as an additive minor revision; B0 adds the opcodes,
SDK codecs and header constants below in one change, and until then the
values are reserved. A host or plugin that predates them answers
`VOSU_ERR_UNSUPPORTED` (§5); a plugin then fails its action with a clear
error and never falls back to a public resource. [API v1](api-v1.md#job-artifacts-af12)
owns the HTTP route, authorization, lifetime and bounds.

- `VOSU_OP_ARTIFACT_DECLARE = 80` (plugin → host, CBOR, nonzero
  `correlation_id`): `{"invocation": u64, "key": <CBOR>, "name": string,
  "content_id": <CBOR>, "size": u64}`. Valid only while that invocation is
  live (before its `ACTION_RESULT`) and only when its caller kind is `tool`
  or `cli`, which own an API v1 job; a relayed `ACTION_CALL` invocation has
  no job and cannot own an artifact. `key` is the plugin's opaque lookup key,
  echoed back unchanged; `name` is one file-name component (no `/`, `\`,
  NUL or control characters, not `.`/`..`, at most 255 UTF-8 bytes) and fixes
  the served type; `size` is the exact byte length. No host path is ever
  sent.
- `VOSU_OP_ARTIFACT_DECLARED = 81` (host → plugin, CBOR, reply correlated
  by `correlation_id`): `{"artifact": string}` or
  `{"error": {"code": string, "message": string}}`, with codes
  `invocation_finished`, `caller_not_supported`, `too_many_artifacts` and
  `invalid_name`. A refused declaration does not end the invocation.
- `VOSU_OP_ARTIFACT_OPEN = 82` (host → plugin, CBOR, correlated):
  `{"artifact": string, "key": <CBOR>}`, sent per download. The reply is a
  `VOSU_OP_RESOURCE_RESOLVED` with no session: `{"root", "path"}` against a
  plugin-wide (`handle: null`) declared root, opened with the same
  retained-handle confinement and identity checks as §6.5, or
  `{"stream", "size"}` served through `STREAM_READ`/`STREAM_CHUNK`/`STREAM_CLOSE`
  as a request-owned lease (A3/R18 above), or an error. A size different from
  the declared `size` fails the download.
- `VOSU_OP_ARTIFACT_RELEASE = 83` (host → plugin, CBOR, no reply):
  `{"artifact": string, "key": <CBOR>}`, sent once after the artifact leaves
  core's registry (job retention ended or evicted, package disabled or
  uninstalled) and after every lease has received `STREAM_CLOSE`. The plugin
  may then delete what backs it. Nothing is sent to a lost instance.

Artifact ids are opaque, unguessable and valid only under their own job;
they are not resource ids and carry no session, PID or handle.

## 7. Execution and trust

### 7.1 Runtime request and the host's decision

A manifest requests `runtime = "in-process"` or `runtime = "process"` (§8). `in-process` is granted only to bundled plugins, plugins from the official sources, or a source the user has explicitly marked trusted ([ADR 0023](../decisions/0023-authenticated-catalog-sources.md)); every other plugin may request it but is refused outright (`VOSU_ERR_PERMISSION_DENIED` at load time, logged, plugin left unloaded) rather than silently downgraded — a manifest cannot use its own runtime request to bypass the trust decision.

`runtime = "process"` is available to any plugin regardless of trust tier. **Approval** (the same user-approval flow 1.x applies to user tools, required before the plugin runs at all) applies only to a plugin that is **not** eligible for `in-process` (`user` trust, per the table above); a bundled, official, or user-trusted-source plugin that happens to request `process` runs without a separate approval step, since the user already trusted its source. An approval binds the exact package content and runtime, not an id or directory; its record, persistence and management routes are [catalog-v2.md "Process-plugin approval and management"](catalog-v2.md#process-plugin-approval-and-management-a55-approved-2026-11-24) (A5.5). Debug builds keep one explicit developer exception: a `--plugin <dir>` manifest requesting `in-process` loads in-process in a `debug_assertions` build only; prerelease and stable release builds never grant it.

**A6b.7, approved 2026-11-24:** source trust and native approval are checked
at plan, confirmation, load and restart using exact source/artifact provenance
and current policy. Credentials, private-network permission and a correct hash
are not author trust. User-process approval binds exact id/source/version/hash
and runtime; changed bytes require approval again. Revocation invalidates plans
and prevents new loads. A previously source-trusted process plugin stops and
requires approval under its new user-trust tier. Running in-process code is
disabled for next load and reported as **revocation pending restart**; vosu
does not automatically restart or claim unsafe unloading erases native access.

The [M6b registry seam](implementation-plan.md#m6b-frozen-service-interfaces)
passes validated package location, target library and exact artifact approval
to M5's execution backend. A process request must never be validated by
parent-side `dlopen` or replaced by in-process execution. Until real M5 runtime
and its security prerequisites exist, source/solver work may land but native
user-plugin execution evidence remains pending. M5's A5.1 Linux privilege
bootstrap decision remains owner-pending; M6b does not amend §7.3.

### 7.2 The `vosu plugin-host` process protocol

For a `process`-runtime grant, core spawns `vosu plugin-host` once per plugin instance (not shared across plugins, so one crash is isolated to its own plugin).

**Channel setup.** Before spawning, core creates two fixed-capacity shared-memory regions, one per direction (host → plugin-host and plugin-host → host), each organized as a single-producer/single-consumer byte ring with a small atomic header (read offset, write offset, capacity) plus a paired wakeup primitive: on Linux, an anonymous `memfd_create` region and an `eventfd` per direction; on Windows, an **unnamed** file mapping (`CreateFileMappingW` with a `NULL` name, `MapViewOfFile`) and an **unnamed** auto-reset event object (`CreateEventW` with a `NULL` name) per direction — **the Windows mechanics are the one piece of this transport marked "to be measured" by [`docs/v2/brokered-reads.md`](brokered-reads.md#open-questions) and do not block this ABI freeze**; the ring/frame format below is unconditional. "Single-producer" is a design constraint on each ring's own protocol, not a limit on how many plugin threads may call `vosu_host_services::send`: the plugin-host → host ring's one logical producer is `vosu plugin-host`'s relay itself, which serializes every concurrent `send` call — from the dispatch thread, a `concurrent_dispatch` worker, or a plugin's own spawned thread — behind one lock before writing a frame, so the ring never sees two writers at once regardless of how many plugin threads call `send` concurrently. Core hands both regions and both wakeup primitives to the child via inherited descriptors/handles at spawn only (on Linux the `memfd` fds and `eventfd`s are simply not marked close-on-exec; on Windows the handles are marked inheritable and passed through the process creation call, never through a named object another process could open by guessing or enumerating its name). Because the channel is reachable only through handles the child inherited — never a discoverable name or path a different local process could open or connect to — **no separate bearer token or authentication frame is needed**: possession of the inherited handle is the whole handshake. `vosu plugin-host` and core always ship as the same vosu release, so there is no ABI-version negotiation at this hop either; version negotiation happens only at the plugin-library boundary (§1), identically whether that library is loaded by core or by `vosu plugin-host`.

Once its capability drop (§7.3) or job-object confinement (§7.4) completes, `vosu plugin-host` `dlopen`s the plugin library named in the manifest's per-target `library` path, resolves `vosu_plugin_entry_v1`, validates `abi_version`/`struct_size` exactly as an in-process load would, and calls `create` with a local `vosu_host_services` whose `send` writes a frame into the plugin-host → host ring and signals its wakeup primitive. The host → plugin-host ring has exactly one reader, matching its single-producer/single-consumer design (above, "Channel setup"): a dedicated ring-reader thread, woken by the ring's primitive, that only sorts frames — it never calls `dispatch` itself, and it always drains the ring; nothing in this design ever blocks it. A `VOSU_OP_READ_PLAN_RESULT` answering a `VOSU_READ_PLAN_SYNC` submission (§6.3, D5) goes straight to that plan's waiting submitter, which calls `dispatch` on its own thread, exactly as the in-process path does. Every other frame goes onto the instance's per-instance delivery queue, whose size is bounded, never unbounded, by construction rather than by any queue-level limit of its own: a latest-only notification opcode — `VOSU_OP_STATE_UPDATE` keyed by `session` (merged per §6.12's rule once complete, capped per session at one in-flight split sequence plus one pending merged update, never replaced wholesale), `VOSU_OP_DEMAND_CHANGED` keyed by `handle`, `VOSU_OP_TIMER_FIRED` keyed by `timer`, `VOSU_OP_FOCUS_CHANGED` keyed by `handle`, `VOSU_OP_SETTINGS_CHANGED` (one per instance, always the complete settings object per §6.9), and `VOSU_OP_ACTION_PROGRESS` keyed by `invocation` (an oversize instance of this one is dropped rather than queued, §6.8) — coalesces with any of the same key already queued, so this part of the queue is bounded by distinct keys (sessions, handles, timers, invocations) times a small constant, never by message rate; a work-unit opcode (`VOSU_OP_ACTION_INVOKE`, a relayed `VOSU_OP_ACTION_RESULT` answering a plugin's own `ACTION_CALL`, `RESOURCE_RESOLVE`, `TREE_LIST`/`TREE_LOOKUP`, `STREAM_READ`, a `SCAN_REQUEST`'s eventual `SCAN_RESULT`, and an asynchronous `READ_PLAN_RESULT`) is bounded instead by how many such requests core itself ever keeps outstanding to one plugin instance at once — a limit enforced at the point it would create a new one (64 for read plans, §6.3; 64 for scans, §6.4; 64 for cross-plugin calls, §6.12; `ACTION_INVOKE` itself by api-v1's own 1024-concurrent-job cap plus 64 per calling instance, §6.12), not by refusing to deliver an already-sent one. Every one of these caps is local to one plugin instance's own count, so `vosu plugin-host` enforces each of them itself before writing the refused `SUBMIT`/`SCAN_REQUEST`/`ACTION_CALL` frame to the ring at all — unlike `focus_priority`'s cross-instance rule (§6.2), which `send`'s synchronous return cannot check from inside one instance, this is a per-instance counter `vosu plugin-host` already keeps for its own accounting, so no round trip to core is needed to refuse early. The instance's ordinary `dispatch` thread (or, under `concurrent_dispatch`, its worker pool) drains this queue independently of the ring-reader, so a slow `dispatch` call delays only this plugin instance's own notifications and work, never the ring-reader's delivery of a concurrent synchronous result or any other instance's traffic. The plugin library's own code is unaware it is being relayed: it only ever makes and receives ordinary in-process vtable calls into `vosu plugin-host`'s local implementation of `vosu_host_services`.

**Framing.** Each ring carries length-prefixed frames, written and read as a contiguous sequence of bytes (wrapping at the ring's capacity): a 4-byte little-endian total length (excluding itself), then `opcode` (u32), `flags` (u32), `correlation_id` (u64), `payload_len` (u32), and `payload_len` bytes of payload — the same fields as `vosu_message`, concatenated. Every opcode, in either direction, is bounded by one universal cap, `VOSU_FRAME_MAX_BYTES` (1 MiB): `VOSU_OP_PUBLISH_FIELD`/`PUBLISH_BATCH`'s whole-CBOR values, a `RESOURCE_RESOLVED` answering `TREE_LIST`, a `STREAM_CHUNK`, and a host-authored `STATE_UPDATE` are each capped (or, where the content is naturally splittable — a tree listing's existing `truncated: bool`, `STATE_UPDATE`'s `more` continuation flag, §6.12, or a `STREAM_CHUNK`'s own narrower cap, §6.5 — split) at or below this same bound, never only `PUBLISH_BATCH` (§6.5, §6.6, and §6.12 state each case). Each ring's fixed capacity is required to be at least `VOSU_FRAME_MAX_BYTES` plus one frame's length-prefix and header overhead, so a legal frame can never itself overflow an empty ring. `send` rejects a frame whose payload exceeds the cap with `VOSU_ERR_INVALID_ARGUMENT` before it is ever written to the ring, exactly as the in-process path already does; only a frame that is legal on its own but arrives while the ring is otherwise occupied can still overflow the *current* free space, and that writer blocks (parking and retrying) for up to 2 seconds before the connection is treated as unresponsive and the crash-handling path below applies — this keeps each ring's capacity bounded rather than growing without limit, per `AGENTS.md`'s unbounded-queue rule, without ever treating an oversize-but-illegal frame as a crash. Frames may be answered out of order; `correlation_id` (or, for read plans, `plan`) is how each side matches a reply to its request. A flat-layout opcode's frame is exactly its `vosu_message` as `dispatch`/`send` would otherwise receive it — the relay never reinterprets or re-encodes it, only copies it into and out of the ring.

**Pressure, loss and restart.** Idle silence on either ring is normal and never a loss: a healthy plugin may publish nothing indefinitely, and nothing in this protocol polls, heartbeats, or times out an empty ring. Loss requires an outstanding progress obligation that is not met — a legal frame that has waited 2 seconds for ring space (either direction), a status or `SYNC` waiter backstop (§7.6), a peer protocol violation — or the plugin host's OS exit. Each of these marks every session that plugin owned `detached` (`reason: "lost"`) and is handled exactly once per process generation by §7.6's supervisor: restart with the 1, 2, 4, 8, 16-second backoff and the five-in-five-minutes `failed` latch defined there. An in-process plugin crash is not survivable — it is the daemon crashing — which is exactly the cost the trust table in §7.1 prices in.

**Fixed capacity (A5.2).** Each direction's ring has exactly 2 MiB (2,097,152 bytes) of frame storage plus its header, a compile-time constant of the shared executable generation. The value is never read from the peer. It holds one maximum legal frame (`VOSU_FRAME_MAX_BYTES` + 24 bytes of prefix and header) together with the 24-byte private frame that precedes an acknowledged frame (§7.6). A plugin host therefore maps 4 MiB of ring storage. Shared ring headers are untrusted peer memory: cursors and lengths are validated against the trusted capacity with checked arithmetic before any copy or allocation, and a violation is a protocol loss of that channel only.

### 7.3 Linux: privilege drop and confinement before loading plugin code

**Amended by A5.1, owner decision 2026-11-24, option (b).** The earlier requirement to empty the bounding set is withdrawn. Option (a), adding `CAP_SETPCAP` to `vosu --setup`, was rejected because it broadens the daemon executable's authority.

`vosu --setup` grants the `vosu` executable `CAP_SYS_PTRACE` (and, where eBPF process-event backends are available, `CAP_BPF`, `CAP_PERFMON`, `CAP_SYS_RESOURCE`) so core itself can read arbitrary same-user process memory. That list is unchanged: plugin hosting gives the daemon no new privilege, and core never runs `setcap`, a privileged helper or an elevation prompt to launch a plugin. `vosu plugin-host` must never hold any of these: every memory read it needs goes through `VOSU_OP_READ_PLAN_SUBMIT` to core, which executes it against only the one process that plugin matched.

**Launch (accepted 2026-11-24 with M5 L0).** Core `execve`s the same file-capability executable, so without further care the kernel would grant the child those file capabilities and clear any parent-death signal set before the exec. Core's launcher therefore runs these steps in the forked child before `execve`, using only async-signal-safe syscalls:

1. `prctl(PR_SET_PDEATHSIG, SIGKILL)`, then `getppid()` must still be core. This covers the window between `fork` and `execve`.
2. `prctl(PR_SET_NO_NEW_PRIVS, 1)`. With `no_new_privs` set, the `execve` grants no file capabilities, so the plugin host never holds one, not even while the dynamic loader and runtime start. The exec is then not a privileged exec, and the parent-death signal survives it.
3. `setpgid(0, 0)`. Each plugin host gets its own process group, so terminal job-control signals aimed at core's group (for example Ctrl-C or a hangup) cannot kill plugin hosts behind core's controlled teardown (§7.6). Core-only death still kills the host through the parent-death signal.
4. It clears the signal mask, installs the null device on 0–2 and exactly the four endpoint objects on 3–6, and closes every other descriptor.

A failure at any of these steps is startup stage `spawn` (§7.6), reported to core with the step and `errno`. The §7.3 sequence below still runs in full after the exec and still verifies every set: it must not rely on these pre-exec steps, and with them its steps 4–5 are no-ops that must still succeed.

**Kernel requirement.** Process plugins on Linux require kernel 5.4 or later: core holds every plugin host through a pidfd (`pidfd_open`, `waitid(P_PIDFD)`), so that killing it can never reach a reused PID and its exit can be awaited without polling. On an older kernel every process-plugin launch fails deterministically with startup stage `spawn`, is not retried and is not a crash. In-process plugins and core are unaffected.

**Sequence.** On its initial thread, before it opens the plugin library and before any plugin code runs, `vosu plugin-host` performs the following steps and checks every return value. Step 1 runs immediately after argument parsing. Steps 2–6 run after the endpoints are validated and mapped, so that a failure can be reported over the channel (§7.6 startup). The process must have exactly one thread throughout (`/proc/self/status` `Threads: 1`, checked first; an unreadable procfs fails closed).

1. `prctl(PR_SET_PDEATHSIG, SIGKILL)`, then `getppid()` must equal the core PID named on its command line (§7.6). Re-arming after the exec is required: the plugin host must not depend on how it was launched, and a privileged exec clears any signal set before it.
2. `prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)`.
3. `seccomp(SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_TSYNC, …)` installs a filter that denies creating or entering a user namespace:
   - `unshare` and `clone` whose flags include `CLONE_NEWUSER` fail with `EPERM`;
   - every `clone3` fails with `ENOSYS`, because its flags live in user memory a filter cannot inspect, and libc then falls back to `clone`;
   - every `setns` fails with `EPERM`, which also blocks joining an existing user namespace the same user owns;
   - any syscall made under a non-native audit architecture, or with the x32 bit set, kills the process (`SECCOMP_RET_KILL_PROCESS`), so the deny-list cannot be bypassed through another syscall table.

   Every other syscall is allowed. The filter is a barrier against regaining privilege, not a sandbox.
4. `prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_CLEAR_ALL, 0, 0, 0)`.
5. `capset(2)` with empty effective, permitted and inheritable sets.
6. Readback through syscalls:
   - `capget` reports empty effective, permitted and inheritable sets;
   - `PR_CAP_AMBIENT_IS_SET` is 0 for every capability up to the kernel's last capability (`/proc/sys/kernel/cap_last_cap`);
   - `PR_GET_NO_NEW_PRIVS` is 1;
   - `PR_GET_SECCOMP` is 2;
   - `unshare(CLONE_NEWUSER)` fails with exactly `EPERM`;
   - `getresuid` reports no UID 0.

**Postcondition.** `CapInh = CapPrm = CapEff = CapAmb = 0`, `NoNewPrivs = 1`, the step-3 filter active (`Seccomp: 2`), and no root UID. `CapBnd` is **not** part of the postcondition and normally stays the full default set (for example `000001ffffffffff`). With `no_new_privs` set, `execve` cannot add permitted capabilities from file capabilities or set-user-ID bits. The ambient and inheritable sets are empty, and the remaining route to capabilities, a new or joined user namespace, is denied. Emptying the bounding set would need `CAP_SETPCAP`, which setup deliberately does not grant. Tests assert the five sets as observed and assert `CapBnd` only as recorded data.

**Fail closed.** A step-1 failure is startup stage `parent` (§7.6). If any of steps 2–6 fails or any readback differs, `vosu plugin-host` reports startup stage `security` with the failing step and `errno`, then exits. It never opens the plugin library, never creates a thread, and has no fallback with partial privilege. It must never ignore `EPERM`, retry a weaker sequence, or continue with only `CapEff` cleared. Core marks that plugin `failed` with the reason and does not retry automatically. The daemon and every other plugin keep running.

The `security` stage's `FAILED` `code` (§7.6) is one of:

| Code | Meaning |
| --- | --- |
| `security_unavailable` | The environment refuses `no_new_privs` or the step-3 filter (no `CONFIG_SECCOMP_FILTER`, a container policy, an architecture without a verified filter), or hides the procfs files the checks read. |
| `not_single_threaded` | Another thread existed when the sequence started or when the filter was synchronized, so the drop could not cover it. |
| `drop_failed` | Step 4 or 5 failed, or a step-6 readback syscall failed. |
| `readback_mismatch` | Every step succeeded but the step-6 readback differs from the postcondition; the message names the first differing field. |

The failing step is reported as `threads`, `no_new_privs`, `seccomp`, `ambient`, `capset` or `readback`.

**Without setup or capabilities.** The same sequence and postcondition apply whether or not setup ran.

- If the daemon has no file capabilities (setup never ran, setup failed, capabilities were removed, or a portable archive sits on a filesystem without xattrs), the child starts with empty sets. Steps 4–5 are no-ops that must still succeed, and process plugins load normally. Process plugins do not require `vosu --setup`. Core's own memory access is unchanged by this.
- An environment that refuses the filter makes every process plugin a `security` startup failure with code `security_unavailable`. Examples are a kernel without `CONFIG_SECCOMP_FILTER` and a container policy that blocks `seccomp` or `prctl`.
- There is no configuration switch, environment variable or build feature that runs a plugin host without this sequence.
- A daemon running as root is refused through the step-6 UID check; packaged vosu never runs the daemon as root.

**Scope.** This policy is only for `vosu plugin-host`. Plugin code may still spawn helpers; they inherit `no_new_privs` and the filter. Core death kills the plugin host through the death signal, but Linux has no job-object equivalent here: descendants a plugin spawns itself are not killed by core death unless the plugin arranges it.

**Descendant cleanup follow-up.** M5 N0 owns this gap and evaluates a cgroup v2 confinement for it: each plugin host gets its own cgroup leaf, either a transient systemd user scope created through the user manager's `StartTransientUnit`, or a delegated cgroup v2 subtree that core already owns. Core empties that leaf with `cgroup.kill` on every loss and teardown, and the leaf's lifetime is bound to core so that a core-only `SIGKILL` also removes every descendant.

N0 accepts an option only if all of these hold:

- it needs no new privilege and no setup change;
- it works without a systemd user session, or fails visibly when there is none;
- its native evidence shows a plugin-spawned grandchild gone after a core-only kill.

L0 implements the accepted option. If no option qualifies, X2 records the gap as an accepted residual with N0's receipt, and the gap stays documented rather than open-ended.

The D16 vosulay supervisor child is a different launch policy ([implementation plan D16](implementation-plan.md#10-adopted-decisions-and-risks)). It must keep zero active capabilities and must **not** inherit `no_new_privs` or this filter. Chromium's Linux sandbox needs either the setuid helper, which `no_new_privs` disables, or unprivileged user namespaces, which this filter denies. S1 observed the user-namespace sandbox. Process isolation is not an OS sandbox: a plugin host keeps the user's filesystem and network access.

### 7.4 Windows: job objects

Same-user `PROCESS_VM_READ` needs no elevated privilege on Windows, so no capability-drop equivalent is required. `vosu plugin-host` instead gets the same lifecycle confinement 1.x tool services use, made race-free (A5, approved 2026-11-24):

1. Core creates one job per plugin-host process. The job uses `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, without `JOB_OBJECT_LIMIT_BREAKAWAY_OK` or `SILENT_BREAKAWAY_OK`, and its handle is not inheritable.
2. Core creates the child with `CREATE_SUSPENDED` and a `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` that holds exactly the two section handles and the per-direction event handles of §7.2. The job handle, process or game handles, credentials and the daemon's standard handles are never on that list. The child gets no standard handles at all: `STARTF_USESTDHANDLES` with null input, output and error handles, and `DETACHED_PROCESS`, so it has no console. Opening the null device instead would put more handles on the list. Rust's standard library and the C runtime treat a missing standard handle as an empty input and a discarding output.
3. Core calls `AssignProcessToJobObject`, checks that it succeeded, and only then resumes the thread.

If creation, assignment or resume fails, core terminates the suspended child, waits for it, closes every partial handle, and records a deterministic startup failure (§7.6). It never resumes the child unconfined.

**Inheritance gate.** A handle list keeps every other handle out of the plugin host, but the four duplicates core places on it are inheritable while `CreateProcessW` runs. Another process creation in the daemon that inherits handles without a list, as Rust's `std::process::Command` and `tokio::process::Command` do, would receive copies. Every process creation in the daemon therefore goes through one process-wide gate, `vosu_sys::process_spawn`. A plugin-host spawn holds the gate from creating its duplicates until it has closed them, and every other spawn holds it only around the creation call, never while waiting for a child. `clippy.toml` disallows the direct `spawn`, `output` and `status` calls and the third-party launchers that create processes, so no unchecked path remains.

**Child-side endpoint checks.** The plugin host takes the four handle values from its endpoint argument. Before mapping anything, it checks that the values are distinct and canonical, that each names an open handle of the expected object type (`Section` or `Event`), and that each section's size equals the trusted region length rounded up to whole pages. A whole-section view, the only size a process can observe without undocumented calls, reports the size with page granularity. Core maps only the trusted length, so a larger section grants nothing. The plugin host then clears the inheritance flag on all four handles, so processes the plugin starts never inherit core's channel.

Core keeps the job handle until it has reaped the process, so closing core's handles, including an unclean core exit, terminates the plugin host and every descendant it created. Nested jobs are expected, and only the result of the specific assignment counts. A generic `IsProcessInJob` answer is never evidence of membership in this job. Core's evidence is `IsProcessInJob` against this job's own handle together with the job's own process list, which must hold exactly the child before it resumes.

The plugin host runs with the daemon's token. When the daemon is elevated to read an elevated client, process plugins run elevated too: approval remains a trust decision, not a sandbox. The Medium-integrity launch requirement belongs only to the D16 vosulay supervisor policy, not to every process plugin.

**Crash reporting (M5 C0, 2026-11-25).** Before it maps its channel or loads any plugin code, the plugin host calls `SetErrorMode` adding `SEM_NOGPFAULTERRORBOX | SEM_FAILCRITICALERRORS` to its error mode, which processes it starts inherit. Without it, Windows Error Reporting held a crashed plugin host before the process exited. In the VM, a real `std::process::abort` fail-fast was held for 1.2–5.6 s and an access violation for 1.3–5.8 s, so a constructor or `create` crash reached core as a startup `timeout`, and a loss after `READY` came late. With the flag set, both exited within about 125 ms of the fault. `JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION` was measured too: it ended the access violation promptly but not the fail-fast, so the job keeps exactly its §7.4 limit flags. A plugin that changes its own error mode afterwards gets the old delay back, but it is still classified as a crash or a loss once it exits.

**Transport choice (A5.6).** The unnamed section and auto-reset event transport of §7.2 is the production mechanism. M5 P1 also measures a named-pipe variant, which is measurement-only. Production adopts the pipe only if all of the following hold:

- it is faster under the tie rule frozen in the [benchmark guide](../testing/benchmarks.md#m5-d17-contract), measured on the same dependent plans with identical bytes;
- a separately approved amendment provides an endpoint handoff as non-discoverable as inherited unnamed handles;
- the ring frame format and payloads are unchanged.

### 7.5 Bundled-only HTTP surfaces (D3)

The ABI itself carries no HTTP opcode, and an external or out-of-process plugin never accepts an inbound HTTP connection directly: ADR 0019's "one plugin interface" and ADR 0021's "plugins never serve HTTP" apply to every such plugin without exception. The one documented exception is the bundled first-party set (D3): the dashboard, overlay hosting, tool hosting, and every upstream-compatible adapter additionally use a second, HTTP-only in-process crate interface, `vosu-surface`, never exposed to an external or out-of-process plugin, exhaustively:

- route and WebSocket-feed registration against one of core's two listener roles — dashboard-origin or overlay-origin ([ADR 0016](../decisions/0016-separate-dashboard-origin.md)'s origin split);
- state reads that go through the same subscription, demand, and `requires`/`optional` capability path as `VOSU_OP_STATE_SUBSCRIBE` (§6.12) — a second entry point onto the same state bus, not a second data path, so `/websocket/v2/precise` and every other adapter route keeps receiving precise-lane sampling exactly as a normal subscriber would;
- root-confined file responses resolved against the `ROOT_DECLARE` handles declared by the **game plugin that owns the session** (§6.5), never against a path string read off the bus (D2) — an adapter's file route (e.g. `GET /Songs/{path}`) has no root of its own and no path string to open one with; it asks, through `vosu-surface`, for the osu! plugin's already-declared root and a relative path inside it, the same confinement a resource reference resolves through;
- management-route provenance: a `vosu-surface` management route (for example the dashboard plugin's own settings-save route) still requires dashboard-origin provenance and `X-Vosu-CSRF` exactly as `docs/v2/catalog-v2.md` "API placement" specifies for the catalog routes `vosu-core` itself serves — `vosu-surface` grants no exemption from that rule for any route it carries.

The permanent P0/R0 operation and lifetime contract is
[Bundled adapter surfaces](adapter-surfaces.md): atomic multi-selector capture
with an identified coherent roster, demand-free one-shot reads, RAII
subscriptions and route/feed registrations, opaque retained-root operations,
and capability-checked bounded resource readers. These are the exhaustive
in-process operations covered by the bullets above, not additional C opcodes.
Provider callbacks and filesystem/async work run outside host locks.

Route providers return asynchronous `ResponseFuture`s. Requests contain decoded
query pairs (at most 128), at most 32 allow-listed conditional/range headers,
and at most 16 KiB combined query/header bytes; credentials stay in core.
Responses contain a checked status, at most 32 allow-listed headers / 16 KiB,
and empty/shared bytes or a bounded range of an R0 `OpenedFile` lease.
Feeds expose connection-owned filter replacement, latest-only receive,
write-completion acknowledgement and synchronous cancellation through the
surface API. Transport admission and frame limits remain core-owned.

M6b catalog/source/credential/trust/plan/install/job handlers remain **core**
dashboard-listener handlers. Root and later M7 bundled dashboard render bounded
redacted views and call those guarded HTTP routes; they receive no secret values,
generic configuration writer or core service handle through `vosu-surface`.
M7's management-presentation seam is a separate dependency, not a new ABI
operation. Moving presentation must retain the same exact methods, listener,
provenance/CSRF and write-only credential semantics.

`vosu-surface` grants none of: `ProcessMemory` access outside a plugin's own matched session (a `vosu-surface` plugin still reads memory only through `VOSU_OP_READ_PLAN_SUBMIT` like any other plugin), configuration writes, or credentials — a bundled plugin that needs one of these uses the ordinary ABI services, exactly as any other plugin does. M4a deletes the interim typed snapshot/control bridge and the `vosu-osu → vosu-surface` edge: the game plugin publishes through the ABI, and the three ordinary adapters read through their own surface grants. No global snapshot hook, phase task or root control input remains. Host-only operational diagnostics and confined display-path metadata are not plugin authority and are not exposed through surface. A future external HTTP opcode remains possible as an additive minor ABI revision, not a breaking one, but none is defined by this freeze.

**Private child transports (AF.13, approved 2026-11-24).** A plugin's own
private child processes are not a plugin HTTP surface and not a 1.x tool
service: the replay renderer's render worker and private replay-overlay
backend, and vosulay's CEF preview. Such a transport is owned by one plugin
instance, prefers non-network IPC for control, and is never registered with
core: no core route, listener, proxy, service token or launch session. A
loopback listener that a child genuinely needs (frame-sized render data, a
browser that must load overlay pages) binds an ephemeral loopback port for
that child's lifetime only, admits only that child through a per-run secret
or equivalent private origin, carries no shell, filesystem-path or action
authority, and is torn down and reaped with the child. It is never a
fallback data path for first-party tools, overlays or UIs. Upstream-compatible
routes may appear only inside the private replay-overlay backend, to render
third-party overlays in a replay; migrated v2 overlays read replay state
there through API v1 playback semantics (a replay session identity and
deterministic render-clock barriers), never live daemon state.

### 7.6 Process supervision protocol (A5, approved 2026-11-24)

This section freezes the private contract between core and one `vosu plugin-host` generation. Its frames are transport-local: plugin code never sees them, `send` and `dispatch` never accept them, and they add no ABI opcode. Plugin code shares the child's address space and can write that process's ring memory directly. Core therefore treats every child → core frame, including the private ones below, as an untrusted claim that grants nothing beyond what plugin `send` can already do. Core validates each claim against its own state machine. A violation is a protocol loss of that instance only.

**Launch.** Core starts the exact executable generation it is running. On Linux it executes `/proc/self/exe`. On Windows it uses the image path captured at startup, and the build check below catches an updater replacement.

- `argv[1]` is exactly `plugin-host`. `src/main.rs` dispatches it before `vosux`, before any Tokio runtime or thread exists, and before any daemon service, listener, updater, discovery, ownership lock or log file.
- The remaining arguments are exactly these, in order (E0, accepted 2026-11-25):
  - the compile-time build identity of the executable, `vosu/<package version>/plugin-host-1`;
  - on Linux only, the core PID in canonical decimal (no sign, no leading zero, at most `i32::MAX`);
  - the inherited endpoint values: on Linux exactly `3,4,5,6` (host → plugin mapping and doorbell, then plugin → host), on Windows `h2p=<section>:<event>,p2h=<section>:<event>` (§7.4).

  The full command line is therefore `vosu plugin-host <identity> <core pid> 3,4,5,6` on Linux and `vosu plugin-host <identity> h2p=…,p2h=…` on Windows.
- The child checks the argument shape first, then the build identity. A missing, extra, duplicate, empty or malformed argument exits with `arguments` (70) even when the identity also differs; a well-formed command line with another build identity exits with `generation` (71). An endpoint that is not an open inherited descriptor or handle of the expected kind and of the fixed trusted size exits with `endpoints` (73). All of these exit before the child maps anything, and none starts a normal daemon, listener, updater, discovery, lock, log or configuration file.
- **Build identity residual (accepted 2026-11-25).** The identity names the package version. On Linux the child is always the running image because core execs `/proc/self/exe`. On Windows core starts the image path captured at startup, so a replacement image with the *same* version is not detected. This is accepted because the updater always installs a different version, so every in-place update is refused as `generation` (`executable_updated`).
- Linux core spawns plugin hosts only from a dedicated launcher thread that lives until shutdown has reaped every plugin host. `PR_SET_PDEATHSIG` fires when the *thread* that created the child exits, so spawning from a transient pool thread would kill healthy children.
- The child's environment is inherited and is not a secret boundary. Its standard handles are the null device. Plugin diagnostics use `VOSU_OP_LOG_RECORD`.

**Private frames.** These use the §7.2 frame layout. A frame is private iff `opcode & 0x8000_0000` is set; the header reserves that bit as `VOSU_OPCODE_PRIVATE_BIT` and the SDK exports it under the same name. Every public ABI opcode is below that bit, so a plugin `send` naming any such opcode is refused locally with `VOSU_ERR_UNSUPPORTED`, exactly as an unknown opcode is in-process.

A private frame always has `flags = 0`. Its `correlation_id` field carries the private request ID, or 0 for lifecycle frames. A public frame's `correlation_id` and payload are never rewritten, reused or wrapped.

| Opcode | Name | Direction | Payload |
| --- | --- | --- | --- |
| `0x8000_0001` | `CREATE` | core → child | CBOR: `generation`, `plugin_id`, absolute `library` path, 32-byte `library_sha256` (the approved digest), `concurrent_dispatch`, `sync_budget_ms`, `init` (`{"plugin_id", "settings", "data_dir"}`, exactly the in-process `create` init) |
| `0x8000_0002` | `READY` | child → core | CBOR: `generation`, the plugin vtable's `struct_size` and `abi_version` |
| `0x8000_0003` | `FAILED` | child → core | CBOR: `generation`, `stage`, `code`, optional `errno`, `message` of at most 512 bytes |
| `0x8000_0004` | `DESTROY` | core → child | empty |
| `0x8000_0005` | `DESTROYED` | child → core | empty |
| `0x8000_0010` | `ACK_NEXT` | child → core | empty. Its request ID applies to the one public plugin → host frame that immediately follows it in the ring. |
| `0x8000_0011` | `STATUS` | core → child | `u32` little-endian `vosu_status` for that request ID |
| `0x8000_0012` | `REPLY_NEXT` | core → child | empty. The next frame is the `READ_PLAN_RESULT` answering that request ID's `SYNC` submission. |

Any other private opcode, nonzero private flags, an `ACK_NEXT` or `REPLY_NEXT` not immediately followed by a public frame of the required direction, or a lifecycle frame out of order is a protocol violation. Examples of out-of-order frames: `READY` twice, `READY` after `FAILED`, or `DESTROYED` without a `DESTROY`. Request IDs are `u64` values allocated by the child, strictly increasing within one generation and never reused. Core replies only to the latest generation's IDs. A reply for an unknown or expired ID is discarded and counted, never delivered.

**Lifecycle payloads (E0, accepted 2026-11-25).** `CREATE`, `READY` and `FAILED` are CBOR maps with exactly the keys above; a missing or unknown key is a protocol violation by the sender.

- `CREATE`: `generation` (`u64`, starting at 1, never reused while core runs), `plugin_id` (text), `library` (text path), `library_sha256` (a byte string of exactly 32 bytes), `concurrent_dispatch` (bool), `sync_budget_ms` (`u32`), and `init`, the same `{"plugin_id", "settings", "data_dir"}` map the in-process `create` receives. The child passes its re-encoding of `init` to `create`, which equals the in-process bytes. A `CREATE` whose encoding would exceed `VOSU_FRAME_MAX_BYTES` is the `create` stage (`init_too_large`), detected by core before spawn.
- `READY`: `generation`, `struct_size`, `abi_version` (`u32` each). Core checks the generation and the header with the same minimum-size and major-version checks an in-process load uses; a mismatch is the `abi` stage.
- `FAILED`: `generation`, `stage` (text), `code` (text of at most 64 bytes), `errno` (`i32`, omitted when none), `message` (text of at most 512 bytes; the child truncates it at a character boundary). A child may name only the stages it detects: `security`, `approval`, `library`, `abi` and `create`. A `FAILED` naming any other stage, or over either cap, is a violation. A failure before the child read `CREATE` (the Linux `security` stage) carries generation **0**, which no real generation uses; core accepts 0 or its own generation.

Child-reported codes: `security` uses the §7.3 codes; `approval` is `digest_mismatch`; `library` is `not_absolute`, `open_failed`, `read_failed`, `load_failed` (including a missing `vosu_plugin_entry_v1`) or `missing_entry` (the entry returned null); `abi` is `abi_mismatch`, `descriptor_too_small` or `missing_required_function`; `create` is `create_rejected` (null `create`) or `init_invalid`.

**Startup.** The child performs these steps in order:

1. Parse its arguments.
2. Arm the Linux parent-death signal and check the parent (§7.3 step 1).
3. Validate and map its endpoints.
4. Run §7.3 on Linux. On Windows, job assignment happened before resume.
5. Read `CREATE`.
6. Open the library at its absolute path and compute its SHA-256 by reading that open object, then compare it with `library_sha256`. The object stays open until loading finishes. On Linux the loader opens `/proc/self/fd/<n>` of the descriptor that was hashed, so a rename or replacement of the path cannot substitute other bytes. On Windows the file is opened with read sharing only (no write or delete sharing), and that handle stays open across `LoadLibrary`; an open writer makes the open fail with stage `library`.
7. Load the library, resolve `vosu_plugin_entry_v1`, and validate `abi_version`, `struct_size` and the required functions with the same pure checks an in-process load uses.
8. Start the ring reader, so that acknowledged sends made from `create` receive their status, then call `create`.
9. Start its dispatch workers.
10. Send `READY`.

The library stays mapped until the process exits, because plugin threads may outlive `destroy`.

Core never `dlopen`s or `LoadLibrary`s a process-runtime library, not even to validate it, because constructors run at load. Core may read the library's bytes only to compute the approval digest. The child never parses `manifest.toml`; core sends the SDK-validated fields.

Core publishes the instance only after `READY`. No session offer, action invocation, resource request or demand reaches the instance before then, and anything core staged for it is rolled back exactly once on any startup failure. Startup has a 5-second deadline, measured from spawn on Linux and from resume on Windows. Every startup failure reaps the process and closes its endpoints.

**First load (M5 C0, 2026-11-25).** Windows evaluates each never-seen image on its first load: Smart App Control and user-mode code integrity, and an on-access scan where the directory is not excluded. In the VM, a fresh 0.8 MB DLL took 0.5–1.1 s to load and a fresh 31 MB executable about 3 s to start, and larger or concurrent first loads take longer. That cost lands inside the startup deadline of a newly installed or updated plugin's first start. Core therefore gives the first start of each library digest in a core run a **30-second** first-load deadline. Once a digest has reached `READY`, every later start of it in that run, automatic restarts included, uses the 5-second deadline. The rule is the same on Linux. The cost is that a `create` that genuinely hangs on a library's first start is detected after 30 s instead of 5 s. A first start that times out is a `timeout` failure like any other, and the digest stays unseen, so the next start again gets 30 s. Excluding only the library load from the deadline would need a new private frame, so this rule changes no frame. The native evidence is `plugin_host_native` `restart::restart_a_never_seen_library_gets_the_first_load_deadline`.

| Startup stage | Detected by | Outcome |
| --- | --- | --- |
| `spawn` (including a Linux pre-exec launch step or a kernel older than 5.4, §7.3; on Windows, building the handle list or command line, `CreateProcessW`, or `ResumeThread`), `job` (Windows: creating the job or reading back its limits, assignment, or the job-specific membership check) | core | `failed`, not retried, not a crash |
| `arguments`, `generation` (build mismatch, for example after an in-place update), `parent`, `endpoints` | exit before the channel exists | `failed`, not retried, not a crash. `generation` reports `executable_updated` and asks the user to restart vosu. |
| `security` (codes `security_unavailable`, `not_single_threaded`, `drop_failed`, `readback_mismatch`; §7.3 "Fail closed"), `approval` (digest mismatch), `library`, `abi` | `FAILED` frame, then exit | `failed`, not retried, not a crash |
| `create` (non-OK `create`, or init over the frame cap), `timeout`, `crash` (exit before `READY` without `FAILED`) | `FAILED`, deadline, or OS exit | On the initial load: `failed`, waiting for a manual restart. During an automatic restart: one crash event (below), and backoff continues. |

Before the channel is mapped, the child reports its stage only through its exit status. Exit codes 70 (`arguments`), 71 (`generation`), 72 (`parent`) and 73 (`endpoints`) are private to the shared executable generation and are not a public interface. After the channel exists the child also uses 74 (it reported `FAILED`), 75 (the channel ended without a controlled `DESTROY`: core gone, a core protocol violation or a stalled write) and 76 (a waiter backstop expired), accepted 2026-11-25. Core takes the stage from `FAILED` when one arrived and maps only 70–73 to a stage; any other exit before `READY` is `crash`.

**Exit before `READY`.** When a child exits during startup, core first reads every frame the child completed before its exit (a `FAILED` written just before exiting names the stage), then settles the startup: the reported stage, else the stage of exit codes 70–73, else `crash`. Its channel then refuses every further write.

**Acknowledged sends (A5.2).** Each status below is identical to what the same call returns in-process.

- **Local refusal.** `send` first applies exactly the in-process pre-handler checks: payload size and null data, unknown opcode (including the private range), unrecognized flags, SDK decode of the payload, `SYNC` submission from the instance's own dispatch thread, and the per-instance caps (64 asynchronous plans, 64 scans, 64 action calls, 64 timers). A check that fails returns that status without writing a frame or reserving a waiter.
- **Acknowledgement.** Every other plugin → host opcode except `VOSU_OP_LOG_RECORD`, whose status is `VOSU_OK` whenever it decodes, is status-acknowledged. Publication validation is included. The child reserves one of at most **64 waiters per instance** (shared by status and `SYNC`, separate from the async caps). It writes `ACK_NEXT` and the unmodified public frame back to back under the single producer lock, so no other sender's frame can come between them, releases the lock, and blocks the calling thread until core's reply. When the second frame must wait for ring space, the writer first wakes a parked consumer, since the `ACK_NEXT` already published may be all it has to read. An oversize payload is refused before either frame is written. A host → plugin opcode a plugin sends has no effect in-process and returns `VOSU_OK` there; the child returns `VOSU_OK` for it without writing.
- **Overload.** A 65th concurrent waiter is refused with `VOSU_ERR_INVALID_ARGUMENT` before any write. This is the documented overload status of §5.
- **Core's reply.** Core validates the frame exactly as in-process. It answers `STATUS`, or for an accepted `SYNC` plan, `REPLY_NEXT` and then the result. A non-OK status means nothing was applied and no later public message follows: a refused read plan, scan or call produces no result, ever. For an accepted asynchronous operation, the `STATUS` always precedes its later result on the same ring.
- **`SYNC` reads.** Core enforces the §6.3 budget (`sync_budget_ms`) and answers `STATUS VOSU_ERR_TIMED_OUT` itself on expiry. The waiting thread, not the ring reader, dispatches a `SYNC` result before `send` returns.
- **Backstops.** A status waiter's backstop is 2 s after its write completes, and a `SYNC` waiter's is `sync_budget_ms` + 2 s. Reaching a backstop means core stopped serving the channel. The child does not fabricate a status: it closes the channel and exits, and core records one `transport_timeout` loss.
- **Waiter release.** Waiters are released on reply, local refusal or loss. A late reply never satisfies a newer waiter.
- **Core → child.** Frames in this direction are not acknowledged. Core already ignores `dispatch` statuses in-process.

The ring reader only validates, copies into aligned storage and routes:

- `STATUS` and `REPLY_NEXT` results go to their waiter;
- other frames go to §7.2's per-instance delivery queue, whose latest-only keys and work-unit caps are unchanged;
- lifecycle frames go to the lifecycle handler.

It never calls plugin code, never blocks on a plugin lock, and never inserts into a full queue. Core's per-instance writer applies the same semantic coalescing, sends work units and terminal results through their existing reserved admission and outboxes, and never refuses an admitted work unit or terminal result because a generic dispatch queue is full.

**Relay details (M5 T1, 2026-11-25).** A5 left these open; the implementation fixes them as follows. Each is transport-local and changes no plugin-visible status.

- **Pairing enforced by the receiving policy.** In core's direction every public frame except `VOSU_OP_LOG_RECORD` follows its own `ACK_NEXT`, and a `LOG_RECORD` never does. More than 64 unanswered acknowledged frames is the child's protocol violation, because the child refuses its own 65th waiter before writing. Core closes a request in its ledger *before* writing the reply, so a child that reuses a freed waiter at once never meets the cap. In the child's direction, a `STATUS` must carry a defined `vosu_status` and is never `VOSU_OK` for a `SYNC` request. A `REPLY_NEXT` never answers a status request, and its result is at most 64 KiB. A reply naming an id nobody waits on (never issued, or released) is discarded and counted.
- **Core ingress.** Core's ring reader never handles a message or writes a reply itself, because a reply written from the reader could park on a ring whose space only that reader announces. It hands each frame to one of two routes. Acknowledged frames other than `SYNC` plans, and log records, go to one ordinary worker, which handles them in ring order and writes each `STATUS`. Each `SYNC` plan is queued on core's bounded broker workers (M5 B0, below), so a slow plan delays neither ordinary traffic nor another `SYNC` caller. While the ordinary worker handles a request and writes its `STATUS`, any frame another thread dispatches to that child waits, so an accepted request's `STATUS` precedes the results it causes. Unacknowledged log records are bounded by 1 MiB of queued bytes per instance; past that a record is dropped and counted, as §6.10 already does past its rate limit. A `SYNC` handler that reports `VOSU_OK` without delivering a result is answered `VOSU_ERR_BACKEND`.
- **Child delivery lanes.** An asynchronous `READ_PLAN_RESULT`, a `SCAN_RESULT`, a relayed `ACTION_RESULT` and a `STREAM_CLOSE` are terminal. They are dispatched before ordinary work, as core's reserved outbox runs ahead of its ordinary queue. Latest-only notifications coalesce in place, keeping the queue position of the frame they replace. Work units and session lifecycle frames keep arrival order. The queue holds at most 64 MiB of payload. Core's own bounds keep it far below that, so exceeding it is a core protocol violation that ends the channel, never a dropped message.
- **Per-session order under `concurrent_dispatch`.** The plugin host runs four dispatch workers, the same pool size core gives in-process `concurrent_dispatch` plugins. Frames naming a session (`STATE_UPDATE`, `DEMAND_CHANGED`, `FOCUS_CHANGED`, `SESSION_ATTACHED`, `SESSION_DETACHED`) never run concurrently with, or overtake, another frame of the same session. A session's final state therefore precedes its detach. Other frames may run concurrently.
- **Resubscription.** When core accepts a `STATE_SUBSCRIBE` naming a session, it clears what it had not yet delivered for that session (§6.12). The plugin host then clears the same session's undelivered pending update, together with the rest of a split sequence still arriving. The new selection's baseline follows on the ring, so the merged update keeps within the 256-selector bound.
- **Local cap accounting.** An asynchronous plan's slot is freed by its `READ_PLAN_RESULT`, an accepted `READ_PLAN_CANCEL`, or its session's `SESSION_DETACHED` or accepted `SESSION_DECLINE`. A scan's slot is freed by its `SCAN_RESULT` (same correlation id), or its session's detach or decline. An action call's slot is freed only by its relayed `ACTION_RESULT`. A timer is freed by an accepted `TIMER_CANCEL`, or by its `TIMER_FIRED` when it is one-shot. A refused request frees its slot again. Plan ids and scan correlations are counted once per submission, never refused as duplicates locally, because only core knows whether an id is still reserved.

**Brokered reads (M5 B0, 2026-11-25).** These fix how core executes a `process`-runtime instance's read plans and scans. None changes a plugin-visible status or payload.

- **One executor, one round trip.** Core relays the `READ_PLAN_SUBMIT` and `READ_PLAN_RESULT` payloads byte for byte. It executes the whole dependent chain itself, through the same executor, ownership check, cached session process handle, lazily built module table (cached only after a successful open) and cancel-id reservation an in-process plugin uses. Scans use the same authorized session memory; the child never opens or enumerates a target.
- **Broker workers.** A `SYNC` plan runs on the same fair, bounded worker pool as asynchronous plans and scans, in its instance's own sub-queue. The pool worker stands in for the submitting thread: it delivers the result as `REPLY_NEXT` plus the result, or a refusal as `STATUS`. A host without broker workers refuses the plan with `VOSU_ERR_BACKEND`.
- **Core's `SYNC` budget.** Core measures `sync_budget_ms` from receipt. When it expires, a per-channel timer answers `STATUS VOSU_ERR_TIMED_OUT` itself, also for a plan blocked inside one native read or still queued behind busy workers. Exactly one answer is written per request: the timer, a refusal and a result race to close the request in the ledger, and only the winner writes. A late result is discarded, and a plan the timer already answered is never started.
- **Generation isolation.** A broker worker answering one channel's `SYNC` request writes its result only to that channel. If the result reaches another endpoint, for example a newer generation that reuses the instance id, nothing is written there, and the old request is answered with its refusal.
- **Terminal results are reserved.** An asynchronous plan reserves its result delivery from a plan-result outbox class (64 per instance) before admission; a scan reserves from a scan-result class (64). The reservation travels with the work, never as a later lookup by instance id, so a result is never dropped behind a full ordinary dispatch queue, and never reaches a later instance that reuses the id. A plan's reservation is released when its result is taken for dispatch, or when a cancel or detach frees the plan's slot, as §6.3 frees the slot itself. A submission that finds its class full is refused with `VOSU_ERR_INVALID_ARGUMENT`, the cap status.
- **Handle status.** Read plans and scans apply §5's UnknownHandle rule: a never-issued handle answers `VOSU_ERR_INVALID_ARGUMENT`, a retired one `VOSU_ERR_TARGET_GONE`, and another plugin's live session `VOSU_ERR_PERMISSION_DENIED`. These are the same in both runtimes.

**Destroy and shutdown.** Stop, disable, approval revocation, uninstall teardown and daemon shutdown all use the controlled per-plugin teardown operation:

1. Cancel any restart timer.
2. Stop admitting new work.
3. Send `DESTROY`.

The child stops routing frames to dispatch when it reads `DESTROY`, lets the dispatch worker finish what was already queued and joins it, then runs `destroy` exactly as in-process destruction does (including §4's 4.5-second worker-join budget), sends `DESTROYED` and exits 0; its ring reader keeps serving status replies for sends `destroy` makes. Core waits up to **5 seconds** after writing `DESTROY` for the OS exit, then kills it (`SIGKILL`, or `TerminateJobObject`) and reaps it. Session, demand, root, resource, job and relay cleanup follows the in-process instance-removal path. A controlled teardown is never a crash, never restarts, and logs `destroy_timeout` if it needed the kill. Uninstall and revocation also unregister the plugin's schemas, matchers and actions exactly once. Daemon shutdown tears down every process instance concurrently, and no restart fires after shutdown begins.

**Per-instance teardown seam (M5 C0, 2026-11-25).** `PluginHost::teardown_instance(id)` is the one host API for the controlled teardown of a single instance of either runtime. It serves manual stop, approval revocation, and M6b uninstall's `RootRemoval::stop`. In order, it:

1. cancels a process instance's restart delay and marks it `stopped`, so nothing restarts it, and waits for a start already in progress, which tears down what it starts;
2. unregisters the instance's matcher and detaches every session it owns with reason `lost`, while the instance is still published, so its final committed state and `SESSION_DETACHED` reach it before `destroy`;
3. takes the instance out of dispatch and releases its resources, streams, actions, jobs, relays, subscriptions and timers;
4. drains queued dispatch and runs `destroy` within the 5-second grace. For a process instance that is `DESTROY`, then a kill and reap past the grace.

It returns once teardown has completed. `Err(DestroyTimedOut)` means the grace ran out; a process child is already dead by then. Limit: an in-process plugin whose `destroy` or queued dispatch never returns cannot be stopped. Its thread keeps running, and its context and library stay mapped (leaked, never freed under it) until the daemon exits, although its host state is released. Uninstalling such a plugin's files is safe only after the daemon restarts. Process status (`starting`, `running`, `backoff`, `failed`, `stopped`, last failure, restarts, crash events and `generation`) is `PluginHost::process_status`; a manual restart is `PluginHost::restart_process_instance`.

**Generation-checked management (M5 U0, 2026-11-25).** `ProcessStatus::generation` counts an instance's starts (initial load, automatic and manual restart); it only grows within a daemon run, also across a stop and a later load of the same instance id. The dashboard's restart and stop name the generation they showed and use `PluginHost::restart_process_generation` and `PluginHost::stop_process_generation`. These compare it under the supervisor lock that begins the next generation or marks the instance `stopped`, and refuse with `HostError::GenerationChanged` before anything is torn down or started, so an automatic restart between the read and the request never lets the request act on the new generation. Approval revocation and M6b uninstall keep the unconditional `teardown_instance`.

**Loss identity.** One owner per generation coalesces into exactly one loss, using the first observed reason:

- OS exit;
- ring protocol violation;
- a 2-second parked write;
- waiter backstop expiry;
- an action-grace kill.

An unexpected exit with code 0 is still a loss. The owner cancels and collects work under locks, then notifies after unlocking, in this order:

1. Final committed state reaches subscribers before detach.
2. Owned sessions detach with `reason: "lost"`.
3. Demand, roots and reference memberships are retired.
4. Pending read, scan, resource, stream and waiter operations end.
5. Running jobs and relay calls whose provider was the lost instance fail with `plugin_crashed`.

A job that already timed out stays `timed_out`, and an already-terminal job never changes. Old endpoints are closed and the process reaped before any restart. A restart is a new generation with new handles, re-offered sessions and identification before public exposure. Nothing from an old generation can complete a new generation's request.

**Restart (A5.3).** Each of these counts as exactly one crash event:

- a loss;
- a crash-stage startup failure during an automatic restart.

Deterministic startup refusals, controlled teardown and approval revocation are never crash events.

- **Failed latch.** Core keeps the timestamps of the last five events. The fifth event inside a rolling 300-second window sets `failed` instead of scheduling a restart.
- **Backoff.** An accepted restart waits 1, 2, 4, 8, then 16 s, saturating. The step counts events since the window was last empty, so 16 s is reached only after older events age out.
- **Timers.** A backoff delay is one cancellable one-shot timer. Stop, disable, shutdown and manual restart cancel it.
- **Manual restart.** A manual restart through the management route (catalog-v2.md) resets the window and the latch, and always starts a new generation.

**Action deadline grace (A5.4).** `DELETE` and session-detach cancellation stay advisory and never kill.

- When the 30-second core deadline expires for an invocation whose callee is a `process`-runtime instance, the job becomes `timed_out` (unchanged), core sends `ACTION_CANCEL`, and that invocation gets a grace deadline 5 seconds later.
- Any `ACTION_RESULT` for that invocation from the callee before its grace deadline proves the instance responsive and clears that grace. The late result is discarded, and the job stays `timed_out`.
- If several invocations are in grace, core kills the instance at the earliest grace deadline that is still unanswered. That kill is one loss, reason `action_grace_expired`, counted once.
- The kill fails the instance's other running jobs and relay calls with `plugin_crashed`. Terminal and timed-out jobs keep their state.
- Relay calls use the same deadline and grace. In-process callees are never killed. There is no manifest timeout field.

**Linux parent death (A5.8).** The child exits on any of:

- the parent-death signal;
- a parent mismatch at startup;
- its own 2-second parked write or waiter backstop;
- a core protocol violation;
- controlled `DESTROY`.

On Windows, closing the job does the same. Startup and controlled destroy both use the 5-second bounds above.

## 8. `manifest.toml` runtime fields for `kind = "plugin"`

Every package (plugin, tool, overlay) ships one `manifest.toml` at its root with a required top-level `kind`; [`docs/v2/catalog-v2.md`](catalog-v2.md) owns the unified layout, the `kind` rule, and which sections each kind may use. This document defines only the fields `kind = "plugin"` adds for the ABI itself. Shared identity/display metadata (`description`, `author`, `homepage`, `icon`, `banner`, `replaces`, `host`), capability declarations (`provides`/`requires`/`optional`, `[[actions]]`), and the common-field coverage list `covers` are defined by `docs/v2/catalog-v2.md`, which also defines what a reserved-name `[[resource_kinds]]` entry covers and mirrors the runtime-relevant fields below into `catalog-v2.json` plugin entries so the resolver can plan an install without downloading every candidate package.

```toml
kind = "plugin"                   # required top-level field, every manifest.toml (docs/v2/catalog-v2.md)
manifest_version = 1              # required; this host accepts only 1
id = "vosu.osu"                   # required; naming-contract plugin id
name = "osu!"                     # required shared field (docs/v2/catalog-v2.md)
version = "2.0.0"                 # required, semver of this plugin
host = ">=2.0.0, <3.0.0"          # required shared field (docs/v2/catalog-v2.md)
abi_version = 1                   # required; major ABI version this binary targets
runtime = "in-process"            # required; "in-process" | "process" (request only, see 7.1)
concurrent_dispatch = false       # optional, default false; see "Threading model"
settings_schema = "settings.schema.json"  # optional, path relative to the manifest
root_slots = ["songs"]            # declared root-slot names only, no paths (§6.5); paths register at runtime via VOSU_OP_ROOT_DECLARE
covers = ["field:vosu.play.combo", "field:vosu.chart.title"]  # common-namespace fields this game plugin fills (`docs/v2/catalog-v2.md` defines; §6.6 consumes it)

[targets.x86_64-unknown-linux-gnu]   # omitted entirely for bundled plugins
library = "lib/libvosu_osu.so"       # path relative to the manifest

[targets.x86_64-pc-windows-msvc]
library = "lib/vosu_osu.dll"

[match]                           # game plugins only; schema in §6.1
process_names = ["osu!.exe", "osulazer.exe"]
process_path_patterns = []
command_line_patterns = []
linux_name_contains = ["osu!", "osulazer.exe"]   # A1b/DB
comm_hints = []

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
    /// A host -> plugin message this plugin's dispatch received. The SDK
    /// serializes calls behind a lock, so this is safe to implement
    /// regardless of what the manifest declares.
    fn on_message(&mut self, message: Message) -> Result<(), PluginError>;
}

/// For a plugin whose manifest sets `concurrent_dispatch = true` (D22): the
/// host dispatches from its own worker pool instead of one dedicated
/// thread, so `on_message` takes `&self`, not `&mut self`, and the plugin's
/// own state must be `Send + Sync` and internally synchronized. A plugin
/// implements exactly one of `Plugin`/`ConcurrentPlugin`, never both; the
/// registration macro (below) picks the dispatch path from which trait the
/// plugin's type implements.
pub trait ConcurrentPlugin: Sized + Send + Sync {
    fn create(host: HostServices, init: Init) -> Result<Self, PluginError>;
    fn on_message(&self, message: Message) -> Result<(), PluginError>;
}

pub struct Init { pub plugin_id: String, pub settings: ciborium::Value, pub data_dir: PathBuf }

pub struct HostServices { /* wraps `*const vosu_host_services` + its ctx pointer */ }

impl HostServices {
    pub fn publish(&self, handle: SessionId, ns: &str, pointer: &str, value: Value);
    pub fn identify_session(&self, handle: SessionId, game: &str, variant: &str, name: &str, version: &str, focus_priority: bool);
    pub fn declare_root(&self, handle: Option<SessionId>, name: &str, path: &Path);
    pub fn submit_read_plan(&self, handle: SessionId, root: PlanBase, stages: &[Stage]) -> PlanId;
    pub fn scan(&self, handle: SessionId, request: ScanRequest) -> CorrelationId;
    pub fn log(&self, level: Level, message: &str, fields: impl Into<CborMap>);
    pub fn set_timer(&self, interval: Option<Duration>) -> TimerId;
    pub fn cancel_timer(&self, timer: TimerId);
    pub fn subscribe_state(&self, session: Option<SessionId>, selectors: &[(String, String)]);
    pub fn call_action(&self, action: &str, session: Option<SessionId>, args: impl Into<CborMap>) -> InvocationId;
}

// One macro call generates `vosu_plugin_entry_v1` for an external build.
// Bundled registration into the explicit `BUNDLED` array is provided by the
// unpublished `vosu-plugin-internal` crate (AF.9, implementation plan D11),
// never by the published SDK; the plugin author's code is identical either way.
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
