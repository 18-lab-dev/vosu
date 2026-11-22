---
type: Contract
title: "Catalog v2"
description: "Manifest capability fields, catalog-v2.json format, the install resolver, and catalog sources for vosu 2.0."
---

# Catalog v2

This spec covers the 2.0 ecosystem contract: the capability fields every plugin,
tool, and overlay manifest declares; `catalog-v2.json`; the install resolver;
catalog sources and credentials; and the 1.x-to-2.0 transition. It extends
[ADR 0022](../decisions/0022-install-resolver-and-2-0-transition.md) and
[ADR 0023](../decisions/0023-authenticated-catalog-sources.md); their context
and alternatives are not repeated here. The 1.x tool protocol
([docs/tools/protocol.md](../tools/protocol.md)) and `src/tools/manifest.rs`,
`src/tools/catalog.rs`, `src/tools/catalog_sources.rs` are the baseline this
spec changes; differences from 1.x are called out explicitly.

Plugin ABI runtime fields (`manifest_version`, `abi_version`, `runtime`,
`concurrent_dispatch`, `settings_schema`, `[targets.*]`, `[match]`,
`root_slots`, and `[[resource_kinds]]`), the process matcher's matching
a plugin implements against the C ABI are defined in `docs/v2/plugin-abi.md`;
this spec references them by name without redefining them, and owns every
other `plugin.toml` field (below). Field semantics are defined in
`docs/v2/state-schema.md`; action invocation (the HTTP route, session header,
and authorization) is defined in `docs/v2/api-v1.md`. The shared naming
contract (plugin ids, capability ids, namespaces, sessions) is normative for
every identifier in this document.

## Capability fields (plugin, tool, and overlay manifests)

Every plugin, tool, and overlay manifest carries three optional array-of-tables
fields, using capability ids from the naming contract (`field:…`, `resource:…`,
`action:…`, `game:…`):

```toml
[[provides]]
id = "field:vosu.osu"              # a bare namespace id covers the whole namespace
version = "1.0.0"                  # the owning namespace's schema version, not the plugin's own release

[[requires]]
id = "field:vosu.play.combo"
version = "^1.0.0"                 # semver requirement the provider's `provides` version must satisfy

[[optional]]
id = "resource:chart.background"
version = "^1.0.0"
```

- **`provides`**: only a plugin manifest may declare it. A tool or overlay
  manifest with a non-empty `provides` is invalid. A plugin may provide
  several capabilities, each independently versioned:
  - For a `field:` id, `version` must equal the owning namespace's current
    schema version (`docs/v2/state-schema.md`; every namespace schema starts
    at `1.0.0`) — a field's shape, not the plugin release that happens to
    implement it, is what a requirer actually depends on. A plugin cannot
    publish a namespace at a version its own build doesn't match the
    corresponding schema revision for.
  - For an `action:` id, `version` is the plugin's own chosen version for
    that action and must equal the matching `[[actions]]` entry's `version`
    (below).
  - For a `resource:` id naming a plugin's own kind
    (`resource:<plugin-id>.<kind>`), `version` is the plugin's own chosen
    version for that resource kind. For a `resource:` id naming a core kind
    (`resource:chart.file`, `chart.audio`, `chart.background`, `chart.set`,
    or `skin`), `version` is pinned to the owning namespace's schema version
    (`vosu.chart` or `vosu.skin`), exactly as for `field:` ids; a plugin must
    not list a core kind in its own `provides` at all (see "Built-in
    providers" below).
  - For a `game:` id, `version` must be the literal string `"*"` (see
    "Game capabilities" below); game support is a presence check, not a
    version negotiation.
- **`requires`** and **`optional`**: any manifest kind may declare them. An id
  must not appear in both lists of the same manifest. A plugin must not
  require or optionally depend on a capability id that falls under its own
  `provides` namespace (no self-dependency); depending on a different
  plugin's namespace, or on a core-owned namespace such as `field:vosu.play`,
  is ordinary and common. Core-owned namespaces and bundled plugins' own
  capabilities are always resolvable without installing anything (see
  "Built-in providers" below); a game plugin needs no `requires` entry at all
  to have most of its session's `vosu.client` fields filled: core fills
  `session`, `pid`, `focused`, and `plugin` itself from session discovery,
  and the game plugin fills `game`, `variant`, `name`, and `version` through
  its own `SESSION_IDENTIFY` message (`docs/v2/plugin-abi.md`), neither of
  which is a `provides`/`requires` declaration.
- `version` in `requires`/`optional` is a semver requirement string
  (`semver::VersionReq` syntax, matching 1.x's `Requirement.version`), except
  for a `game:` id, whose requirement must be the literal string `"*"`.
- An id with an unrecognized capability prefix, or a malformed semver/semver
  requirement, is a manifest validation error (same rejection style as 1.x
  `Manifest::parse`/`Catalog::validate`).
- A tool's `requires`/`optional` list is also its permission surface: it may
  invoke only `action:` ids present there, after they resolve to an installed
  provider, through the route `docs/v2/api-v1.md` defines
  (`POST /api/v1/sessions/{id|active}/actions/{plugin-id}.{name}`, authorized
  by the dashboard-minted tool session that spec names). There is no separate
  "actions it invokes" list; declaring the capability *is* declaring the
  intent to call it. Overlays never invoke actions
  ([ADR 0019](../decisions/0019-plugin-runtime-and-trust.md)): an overlay
  manifest with any `action:` id in `requires`/`optional` is invalid, and may
  depend only on `field:`/`resource:` ids.

### Capability coverage

A provided id satisfies a required id when they are equal, or when the
required id is the provided id plus one or more further dotted segments (the
provided id is an ancestor path of the required one, in the same namespace):
`field:vosu.osu` (a bare namespace) satisfies `field:vosu.osu.play.pp.current`,
and `field:acme.game` satisfies `field:acme.game.combo`. The reverse never
holds: a `provides` entry for a leaf path does not satisfy a `requires`/
`optional` entry naming an ancestor of it, since the resolver cannot assume a
plugin publishes everything beneath a path it only partially declared. The
same rule applies to `resource:` and `action:` ids, though in practice those
are published one capability per concrete kind/action rather than as a
covering namespace.

### Namespace and kind parsing

Plugin ids are 2–4 dotted segments, so a `field:`/`action:`/`resource:` id's
namespace boundary is ambiguous from the string alone. The resolver parses it
by the **longest registered namespace** — a core-owned namespace
(`vosu.client`, `vosu.status`, `vosu.chart`, `vosu.play`, `vosu.skin`, the
reserved `vosu.sessions`, below) or a bundled/installed/catalogued plugin id,
across every enabled source — that is a prefix of the capability id's path;
everything after that prefix is the field path, action name, or resource
kind. A plugin id that extends another already-registered plugin id or any
core-owned namespace (for example `a.b.c` when `a.b` is already bundled,
installed, or catalogued, or a plugin id starting with the segment `vosu`)
is rejected at install time and at catalog validation time, since it would
make this parse ambiguous.

Resource kinds are one segment, `[a-z0-9][a-z0-9-]{0,31}`. The bare names
`file`, `audio`, `background`, `set`, and `skin` always mean the core kinds
(`chart.file`, `chart.audio`, `chart.background`, `chart.set`, `skin`) and
are reserved: a plugin's own `[[resource_kinds]]` entry (`docs/v2/plugin-abi.md`)
must not use any of them as its bare `kind`. A plugin id must not start with
the segment `chart` or `skin` either, so a `resource:` id is unambiguous:
`resource:chart.background` is always the core kind,
`resource:<plugin-id>.<kind>` is always that plugin's own kind. A bare kind
name (as written in `[[resource_kinds]]`) carries no plugin-id prefix; the
`resource:` capability id is the only place that prefix (or its absence, for
a core kind) appears.

The pseudo-namespace `vosu.sessions` is reserved for session lifecycle
events delivered to a subscribing plugin (`docs/v2/plugin-abi.md`'s
`VOSU_OP_STATE_SUBSCRIBE` session-events use); it is never a plugin id, a
published data namespace, or a `field:`/`resource:`/`action:` capability —
no manifest, schema, or catalog entry may use it as such.

### Game capabilities

`game:<game-id>` ids use the grammar `^[a-z][a-z0-9-]{0,31}$` (matching the
`game` field of `vosu.client`, `docs/v2/schemas/vosu.client.schema.json`).
Every `provides`/`requires`/`optional` entry for a `game:` id uses the
literal version `"*"`; a `game:` entry with any other version string is a
validation error.

## Plugin manifest v2 (`plugin.toml`, metadata fields)

Runtime fields — `manifest_version`, `abi_version`, `runtime`,
`concurrent_dispatch`, `settings_schema`, `[targets.*]`, `[match]`,
`root_slots`, and `[[resource_kinds]]` — are defined in
`docs/v2/plugin-abi.md` and not repeated here.
This section defines the remaining fields of the same `plugin.toml`: identity
and display metadata, `provides`/`requires`/`optional` (above), and
`[[actions]]`.

```toml
id = "vosu.osu"                   # naming-contract plugin id: dotted, 2-4 segments
name = "osu!"                     # display name
version = "2.0.0"                 # the plugin's own release version (distinct from any `provides` field: version)
description = "…"                 # optional
author = "18-LAB"                 # optional
homepage = "https://…"            # optional
icon = "icon.svg"                 # optional, path inside the plugin directory
banner = "banner.webp"            # optional
host = ">=2.0.0, <3.0.0"          # required; semver requirement on the running vosu version
replaces = []                     # optional, default []; see "replaces" below

# [[provides]] / [[requires]] / [[optional]]: see "Capability fields" above.

[[actions]]
name = "beatmap.import"           # dot-segments, each [a-z0-9][a-z0-9-]{0,31}; full id is action:<plugin-id>.<name>
version = "1.0.0"                 # must equal the matching `provides` entry for action:<plugin-id>.<name>
title = "Import beatmap"          # display name shown in install plans and tool permission prompts
args_schema = "schemas/beatmap-import-args.schema.json"     # path inside the plugin package
result_schema = "schemas/beatmap-import-result.schema.json" # path inside the plugin package
session = "required"              # "required" | "optional" | "none": whether invocation needs a session
cli = true                        # also exposed as `vosux <plugin-id> beatmap.import [json-args]`
```

- `id`, `name`, `version` are required; `description`, `author`, `homepage`,
  `icon`, `banner` are optional, with the same meaning and validation as the
  1.x tool manifest's identically-named fields (`docs/tools/protocol.md`).
  `icon`/`banner` are paths inside the plugin directory, same file-type rules
  as tools. `id` must not extend, or be extended by, another registered
  plugin id (see "Namespace and kind parsing" above).
- `host` is the same `semver::VersionReq` field `catalog-v2.json` mirrors per
  entry (below); a plugin's own manifest and its catalog entry must agree.
- `replaces` is a list of former plugin ids this plugin's current id
  supersedes, validated with the **naming-contract plugin id grammar**
  (dotted), not the 1.x tool/overlay id grammar: not the plugin's own id, no
  duplicate or ambiguous claim, and never naming another current plugin id
  from the same source. Tool and overlay `replaces` (below) keep the 1.x id
  grammar, since tool and overlay ids are unchanged from 1.x.
- Every `action:<plugin-id>.<name>` entry in `provides` has exactly one
  `[[actions]]` entry with that `name`, and vice versa; their `version`
  fields are equal. A `provides` action entry with no matching `[[actions]]`
  entry, or an `[[actions]]` entry with no matching `provides` entry, is a
  manifest validation error.
- `session` controls the invocation route `docs/v2/api-v1.md` defines:
  `"required"` means `{id|active}` must resolve to an attached session,
  `"optional"` means the call is valid with or without one, `"none"` means
  the action takes no session at all (a utility action a tool or another
  plugin calls independent of any attached client).
- `cli: true` (default `false`) additionally exposes the action as
  `vosux <plugin-id> <name> [json-args]` through the native loopback broker
  credential (`docs/v2/api-v1.md`'s broker-credential authorization applies
  unchanged); this is how [ADR 0019](../decisions/0019-plugin-runtime-and-trust.md)'s
  "tool command lines become commands that plugins provide" is implemented.
  `cli: false` exposes the action only to tools and other plugins.

## Tool manifest v2 (`tool.toml`, `manifest_version = 2`)

Tools are web-only in 2.0: no `[service]`, `[cli]`, `[api]`, or
`[permissions]` ([ADR 0019](../decisions/0019-plugin-runtime-and-trust.md)). A
v1 manifest with any of those fields cannot be a valid v2 manifest; see
[First-start migration](#first-start-migration-from-1x).

```toml
manifest_version = 2
id = "skill-tracker"              # unchanged 1.x id grammar: ^[a-z0-9][a-z0-9-]{0,31}$
name = "Skill Tracker"
version = "0.1.0"                 # semver
description = "…"                 # optional
author = "…"                      # optional
homepage = "https://…"            # optional
icon = "web/icon.svg"             # optional, path inside the tool directory
banner = "art/banner.webp"        # optional
host = ">=2.0.0, <3.0.0"          # required; semver requirement on the running vosu version

[ui]                              # required; a tool with no UI is an overlay or a plugin
root = "web"
entry = "index.html"              # default "index.html"

[[requires]]
id = "field:acme.game"
version = "^1.0.0"

[[optional]]
id = "field:vosu.osu.play.pp.current"
version = "^1.0.0"
```

`settings.json`, `icon`/`banner` serving, and the overlay-origin static rules
are unchanged from 1.x (`docs/tools/protocol.md`). `[[requires]]`/`[[optional]]`
replace `[api].read`/`[api].write`/`[[requires]]` (1.x plugin-identified) and
`[permissions].host`: a tool's dashboard-granted session now scopes strictly
to the capability ids it declared, resolved to the providers the resolver
chose, rather than to a fixed host-permission vocabulary (`docs/v2/api-v1.md`
defines the session header and route). The install plan (below) shows these
as the permissions a tool is requesting. The example above requires a
catalog-installed plugin's capability (triggering a dependency install, see
"Install plan" below) and optionally reads a built-in capability that needs
no install at all (see "Built-in providers" below).

## Overlay manifest v2 (`overlay.toml`, new in 2.0)

1.x overlays carry only a free-text `metadata.txt` and no `requires`; 2.0
overlays need declared capabilities for the resolver to install their
dependency plugins, so 2.0 introduces `overlay.toml` at the overlay root,
alongside `index.html`. `metadata.txt`-based overlays remain readable
(existing community/user content keeps working unmanaged); only a catalog-v2
overlay entry requires `overlay.toml`.

```toml
manifest_version = 2
id = "pp-counter"                 # same id grammar as tools
name = "PP Counter"
version = "1.0.0"
description = "…"                 # optional
author = "…"                      # optional
homepage = "https://…"            # optional
icon = "icon.svg"                 # optional
banner = "banner.webp"            # optional
resolution = [400, 150]           # optional [width, height]
host = ">=2.0.0, <3.0.0"          # required; semver requirement on the running vosu version

[[requires]]
id = "field:acme.game.combo"
version = "^1.0.0"

[[optional]]
id = "field:vosu.play.combo"
version = "^1.0.0"
```

`requires`/`optional` may contain only `field:`/`resource:` ids (see above).
`provides` is always empty and rejected if present. The example above
requires one leaf field of a catalog plugin's namespace, satisfied by that
plugin's whole-namespace `provides` entry per "Capability coverage" above.

## `catalog-v2.json`

```json
{
  "catalog_version": 2,
  "name": "Example catalog",
  "published": "2026-11-21T00:00:00Z",
  "plugins": [{
    "id": "acme.game",
    "name": "Acme game plugin",
    "version": "1.0.0",
    "description": "…",
    "author": "Acme",
    "homepage": "https://…",
    "icon_url": "https://…",
    "banner_url": "https://…",
    "host": ">=2.0.0, <3.0.0",
    "runtime": "process",
    "provides": [
      { "id": "field:acme.game", "version": "1.0.0" },
      { "id": "game:acme-game", "version": "*" },
      { "id": "action:acme.game.song.import", "version": "1.0.0" }
    ],
    "requires": [],
    "optional": [],
    "actions": [{
      "name": "song.import",
      "version": "1.0.0",
      "title": "Import song",
      "args_schema": "schemas/song-import-args.schema.json",
      "result_schema": "schemas/song-import-result.schema.json",
      "session": "required",
      "cli": true
    }],
    "resource_kinds": [{
      "kind": "replay",
      "tree": false,
      "families": ["chart-text"]
    }],
    "replaces": [],
    "matcher": {
      "process_names": ["AcmeGame.exe", "acme-game"],
      "process_path_patterns": [],
      "command_line_patterns": []
    },
    "packages": {
      "x86_64-unknown-linux-gnu": {
        "url": "https://example.org/releases/v1.0.0/vosu-plugin-acme-game-1.0.0-x86_64-unknown-linux-gnu.zip",
        "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "size": 12345
      },
      "x86_64-pc-windows-msvc": {
        "url": "https://example.org/releases/v1.0.0/vosu-plugin-acme-game-1.0.0-x86_64-pc-windows-msvc.zip",
        "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "size": 12345
      }
    }
  }],
  "tools": [{
    "id": "skill-tracker",
    "name": "Skill Tracker",
    "version": "0.1.0",
    "description": "…",
    "author": "…",
    "homepage": null,
    "icon_url": null,
    "banner_url": null,
    "host": ">=2.0.0, <3.0.0",
    "requires": [{ "id": "field:acme.game", "version": "^1.0.0" }],
    "optional": [{ "id": "field:vosu.osu.play.pp.current", "version": "^1.0.0" }],
    "replaces": [],
    "packages": {
      "any": {
        "url": "https://example.org/releases/v0.1.0/vosu-tool-skill-tracker-0.1.0-any.zip",
        "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "size": 12345
      }
    }
  }],
  "overlays": [{
    "id": "pp-counter",
    "name": "PP Counter",
    "version": "1.0.0",
    "description": "…",
    "author": "…",
    "homepage": null,
    "icon_url": null,
    "banner_url": null,
    "resolution": [400, 150],
    "preview": "https://…",
    "host": ">=2.0.0, <3.0.0",
    "requires": [{ "id": "field:acme.game.combo", "version": "^1.0.0" }],
    "optional": [{ "id": "field:vosu.play.combo", "version": "^1.0.0" }],
    "replaces": [],
    "packages": {
      "any": {
        "url": "https://example.org/releases/v1.0.0/vosu-overlay-pp-counter-1.0.0-any.zip",
        "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "size": 12345
      }
    }
  }]
}
```

`acme.game` above is a fictional illustrative plugin for a generic rhythm
game, not any real supported or planned integration.

- `catalog_version` is `2` (catalogs with any other value are rejected by the
  2.0 fetcher, same rejection style as 1.x).
- Three entry arrays: `plugins`, `tools`, `overlays`; each defaults to `[]`.
  An id is unique within its own array; the same id may appear in different
  arrays (a plugin and a tool do not collide).
- `host` is a `semver::VersionReq` string checked against the running vosu
  version; an entry whose `host` the running version does not satisfy is
  listed as unsupported (installable-never, same surfacing as 1.x `supported`)
  rather than hidden.
- `provides` is accepted only on `plugins` entries (empty/absent elsewhere) and
  must equal the `provides` declared by the package's own `plugin.toml`; the
  resolver uses the catalog copy to plan an install without downloading every
  candidate package, so catalog and package must agree — a published mismatch
  is a publisher defect, not something the resolver reconciles.
- `requires`/`optional` mirror the manifest fields above, `{"id","version"}`,
  in the same order, for the same reason 1.x mirrors `requires`
  (`docs/tools/protocol.md`): resolving without downloading every package.
- `actions` appears only on `plugins` entries and mirrors the manifest's
  `[[actions]]` verbatim (`{"name","version","title","args_schema",
  "result_schema","session","cli"}`), empty when the plugin declares none.
  Tool and overlay entries carry no `actions` key: they only invoke actions,
  never declare them.
- `resource_kinds` appears only on `plugins` entries and mirrors the
  manifest's `[[resource_kinds]]` verbatim (`{"kind","tree","families"}`,
  `docs/v2/plugin-abi.md`), empty when the plugin fills no kind of its own
  (it may still fill a core kind, declared through coverage, not here).
- `runtime` appears only on `plugins` entries and mirrors the manifest's
  runtime request (`"in-process"` or `"process"`, `docs/v2/plugin-abi.md`)
  verbatim; the install plan (below) uses it.
- `matcher` appears only on `plugins` entries that are game plugins and
  mirrors the manifest's `[match]` table verbatim as
  `{"process_names", "process_path_patterns", "command_line_patterns"}`
  (`docs/v2/plugin-abi.md` owns the matching semantics: regex dialect,
  anchoring, and case rules). A non-game plugin omits it.
- `packages` is a map from target to `{"url","sha256","size"}`, identical
  field rules to 1.x `Package`: absolute HTTPS URL without embedded
  credentials, 64-character hex SHA-256 of the compressed ZIP, exact positive
  byte size, at most 512 MiB (`MAX_PACKAGE`, unchanged from 1.x). Plugin
  entries use real target triples (`x86_64-unknown-linux-gnu`,
  `x86_64-pc-windows-msvc`; a matching native package takes precedence, no
  matching package means unsupported on that host). Tool and overlay entries,
  being web-only, use exactly the key `"any"`. This replaces 1.x's single
  `package` object on overlay entries with the same `packages: {"any": …}`
  shape tools already use, so every web-only entry kind shares one schema.
- `icon_url`, `banner_url`, and `preview` follow 1.x's art rules unchanged:
  absolute HTTPS, no embedded credentials, validated like package URLs,
  fetched through the dashboard's `/api/catalog/art?url=` proxy for the same
  reason 1.x proxies them (redirect targets rarely set a renderable
  `Content-Type`).
- `replaces: ["old-id", …]` (default `[]`) is accepted on every entry kind. A
  plugin entry's `replaces` is validated with the **plugin-id grammar**
  (dotted); tool and overlay entries keep the 1.x tool-id grammar, exactly as
  their manifests do (above). Within one array, in one source: not the
  entry's own id, no duplicate or ambiguous claim, and never naming another
  current id in that array. A plugin cannot replace a tool; replacement never
  crosses kind.
- ZIP layout, extraction limits, and conventional (non-authoritative) asset
  file names carry over from 1.x: `vosu-tool-<id>-<version>-<target>.zip` for
  tools, `vosu-overlay-<id>-<version>-any.zip` for overlays, and
  `vosu-plugin-<id>-<version>-<target>.zip` for plugins. A plugin ZIP has
  `plugin.toml` (schema split between this spec and `docs/v2/plugin-abi.md`)
  at the archive root; a tool ZIP has `tool.toml`; an overlay ZIP has
  `overlay.toml` and `index.html`, both at the archive root.

## API placement

Every `/api/catalog/*` route this spec defines — install, plan confirmation,
sources list/replace, credential, trust, and private-network — is
dashboard-listener only, like other 1.x management mutations
(`docs/tools/protocol.md`), and requires dashboard-origin provenance plus
`X-Vosu-CSRF` on every `POST`/`DELETE` ([ADR 0016](../decisions/0016-separate-dashboard-origin.md)).
`<source>`, `<plan_id>`, and `<id>` path segments are percent-encoded, since a
source string routinely contains `/` (`owner/repo`, or a full URL).

## Resolver algorithm

### Built-in providers

Before consulting catalogs, the resolver has a fixed **built-in provider
set** that is never installed, never uninstalled, and never counted in any
reference count:

- core's own namespaces (`vosu.client`, `vosu.status`, `vosu.chart`,
  `vosu.play`, `vosu.skin`), each at its schema's current version
  (`docs/v2/state-schema.md`);
- the core resource kinds `resource:chart.file`, `resource:chart.audio`,
  `resource:chart.background`, and `resource:chart.set` (version = the
  `vosu.chart` schema version), and `resource:skin` (version = the
  `vosu.skin` schema version). No plugin may list any of these in its own
  `provides`; whether a given game plugin actually fills a given core kind
  for a session is per-plugin declared coverage
  (`[[resource_kinds]]`/common-field coverage, `docs/v2/plugin-abi.md`), the
  same way common `vosu.play`/`vosu.chart` field coverage is declared, not a
  separate capability negotiation;
- every bundled plugin's `provides` ([ADR 0019](../decisions/0019-plugin-runtime-and-trust.md)'s
  bundled set: the dashboard, overlay hosting, tool hosting, the osu!
  plugin, and the upstream adapters). Their exact `provides` lists live in
  their own manifests, compiled into `vosu`, never in any catalog.

A requirement satisfied by a built-in provider needs no catalog lookup, no
download, and contributes no entry to the install plan's `adds`; it simply
resolves for free, with no `source`. Built-in providers are never a
`replaces` target, an uninstall target, or an autoremove candidate: they
exist whenever `vosu` itself does.

### Provider selection

For a requirement the built-in set does not satisfy, the resolver builds one
map from capability id to candidate plugin providers across every enabled,
successfully-fetched source's `plugins` entries (own source and
source-fetch-order are part of each candidate). A candidate is eligible for a
requirement when its `provides` version satisfies the requirement's
`VersionReq` and its `host` is satisfied by the running vosu version.

Selection order among eligible catalog candidates for one capability id is
strict and needs no user choice:

1. **Official first.** A candidate is official when the exact source string
   it came from equals one of the fixed official catalog source identities
   (the same exact-string allowlist mechanism 1.x uses for trust, generalized
   to every source form ADR 0023 adds: a GitHub URL spelling of an official
   repository is not itself the official identity, matching 1.x's existing
   "tagged, differently cased, HTTPS … spellings are not aliases" rule).
2. **Then source order.** Among remaining candidates, the one whose source
   appears earliest in the user's ordered, enabled source list wins (1.x's
   existing "earlier sources win" shadowing rule, extended from tool/overlay
   ids to capability ids).
3. **Then catalog order.** Within one source, the first matching entry in
   that catalog's `plugins` array wins.

This is deterministic and needs no prompt, matching ADR 0022.

### Version solving

A capability requirement resolves to a *plugin*, and a plugin is installed at
one version, so solving works per plugin, not per capability id:

1. Start the frontier at the requested items (the tool/overlay/plugin the
   user asked to install) plus every plugin already installed and kept.
2. For each requirement (`requires`, and `optional` only when a satisfying
   provider is already built-in, already selected, or already installed — an
   unmet `optional` is dropped silently, never an error, and never triggers a
   new install) of a frontier item, resolve its capability id to the built-in
   set first, then to a catalog plugin via provider selection above.
3. If two different requirers need the same capability id with ranges that no
   single candidate version satisfies, resolution fails with a conflict naming
   both requirers, the capability id, and both ranges — fixing the version
   solving to "one installed version per plugin satisfies every requirer of
   its capabilities, or resolution fails" rather than silently installing two
   versions of one plugin side by side.
4. The newly selected plugin's own `requires` entries join the frontier
   (`optional` entries too, but failing to resolve one is not an error).
5. A requirement that resolves back to a plugin still on the open frontier
   stack is a cycle; resolution fails naming the capability id chain, not
   installing anything in the cycle (1.x's `Catalog::order` cycle detection,
   generalized from tool ids to capability ids).
6. The output is a dependency-first install order (dependencies before
   dependents), like 1.x `Catalog::order`.

### Install plan

Installing any catalog item is one user-facing confirmation, not one per
dependency (ADR 0022). `POST /api/catalog/install {"id","kind","source"}`
resolves and returns a plan rather than installing immediately:

```json
{
  "plan_id": "…",
  "adds": [
    {"id": "skill-tracker", "kind": "tool", "source": "…", "version": "0.1.0", "reason": "requested", "native": false, "trust": null, "runtime": null, "needs_approval": false},
    {"id": "acme.game", "kind": "plugin", "source": "https://gitlab.example.com/acme/game-plugin", "version": "1.0.0", "reason": "dependency", "native": true, "trust": "user", "runtime": "process", "needs_approval": true}
  ],
  "errors": []
}
```

(The tool's `optional` requirement on `field:vosu.osu.play.pp.current` from
its manifest example above resolves to the built-in osu! plugin and adds
nothing to the plan.)

`reason` is `"requested"` for the item the user asked for, `"dependency"` for
everything the resolver added; a requirement satisfied by a built-in provider
never produces an add at all. `native` is `true` only for `kind: "plugin"`
entries (tools and overlays never run native code in 2.0). `trust` mirrors
ADR 0019's eligibility (`"official"`, `"trusted"` for a user-trusted source,
or `"user"` otherwise; `null` for non-plugin entries, which carry no runtime
trust decision). `runtime` mirrors the plugin's manifest request (`null` for
non-plugin entries). `needs_approval` is `true` for any `"user"`-trust plugin
add, matching 1.x's existing user-tool approval requirement, regardless of
its requested `runtime`; confirming the plan when any add still needs
approval is rejected until the caller also lists that add's id in
`POST /api/catalog/plans/<plan_id>/confirm {"approve": ["acme.game", …]}`.

A plugin add whose `runtime` is `"in-process"` while its `trust` would be
`"user"` is never silently downgraded to `"process"` or merely
approval-gated: it is a plan **error** instead (`"errors"` gains an entry
such as `"acme.game requests in-process but <source> is not trusted"`), the
item never reaches `adds`, and the plan cannot be confirmed — matching the
host's own load-time refusal for an ineligible `in-process` request
(`docs/v2/plugin-abi.md`). Any other non-empty `errors` means resolution
failed (conflict or cycle messages, see above); `adds` is then empty and the
plan cannot be confirmed. Confirming returns `{"job_id"}`, polled exactly as
1.x catalog jobs are.

### Dependency-only autoremove

Each installed plugin's origin is recorded as `installed_for: "requested"` (the
user chose to install it directly, including when accepted from game
detection below) or `"dependency"` (the resolver added it). Built-in
providers carry no such record at all: they are never installed, so they are
never counted or removed. This flag is the only persisted state for a
catalog-installed plugin; its current reference count is always recomputed
from the currently-installed set's manifests at resolution time (requires a
fresh graph walk, not an incrementally-maintained counter, so it cannot drift
out of sync with what is actually installed).

Uninstalling a tool, overlay, or `installed_for: "requested"` plugin removes
it, then recomputes every remaining plugin's reference count; any
`installed_for: "dependency"` plugin whose count is now zero is uninstalled
too, recursively (its own now-unreferenced dependency-only plugins are
removed next). A `"requested"` plugin is never auto-removed regardless of its
reference count; only an explicit uninstall of it removes it.

### Game detection

Game plugins carry a process matcher (`docs/v2/plugin-abi.md` owns its
matching semantics; the catalog mirrors its JSON shape, above). Core scans
running processes against the `matcher` field of every *installed* game
plugin, plus the `matcher` field of every *catalog* game plugin from enabled
sources that is not installed (surfaced in `catalog-v2.json` exactly for
this, see above, so core never has to download a package to test a
matcher). When a running process matches an uninstalled catalog game plugin,
the dashboard offers "Add support for `<name>`"; accepting it runs the
normal install plan flow above with `reason: "requested"`.

## Sources and credentials

### Source forms

A source string is one of:

- `owner/repo` or `owner/repo@tag` — GitHub shorthand (unchanged from 1.x).
- A repository URL on any host: GitHub, GitLab (including nested groups),
  Codeberg, or a self-hosted GitLab, Gitea, or Forgejo instance.
- An HTTPS URL ending in `/catalog-v2.json` on any web server.

### Forge detection

Detection runs once per source, cached until the source string changes:

1. Host is exactly `github.com` → GitHub forge, no probe.
2. Host is exactly `gitlab.com` → GitLab forge, no probe.
3. Otherwise, for a repository-URL-shaped source (not already ending in
   `/catalog-v2.json`), probe in order, each an unauthenticated GET with a
   5-second timeout, first success wins:
   - `<origin>/api/v4/version` returns 200 with a JSON `version` field →
     self-hosted GitLab.
   - `<origin>/api/v1/version` returns 200 with a JSON `version` field →
     Gitea-compatible (covers both Gitea and Forgejo: their release-attachment
     authentication and asset API are identical for this spec's purposes, so
     detection does not distinguish the two).
   - Neither answers → detection fails: "could not detect forge type for
     source".
4. A source already ending in `/catalog-v2.json` needs no forge detection: it
   is fetched as a plain static HTTPS URL (HTTP Basic credential only, no
   forge API).

### Per-forge private asset retrieval

- **GitHub**: `GET /repos/<owner>/<repo>/releases/latest` (or
  `/releases/tags/<tag>`) via the API host, `Authorization: Bearer <token>`
  when a token credential is set; find the asset named `catalog-v2.json`; GET
  its `url` field (the API asset endpoint, not `browser_download_url`) with
  `Accept: application/octet-stream` and the same bearer header. The API
  response is a redirect to presigned storage; the downloader follows it
  itself (`src/net/streaming.rs`'s existing no-auto-redirect client) and drops
  `Authorization` once the redirect target's origin differs, per origin
  scoping below.
- **GitLab**: `GET /api/v4/projects/<url-encoded path>/releases/<tag-or-permalink/latest>`
  with `Authorization: Bearer <token>`; find the release's asset link whose
  name is `catalog-v2.json`; GET its `direct_asset_url` with the same bearer
  header, dropped on an origin change.
- **Gitea-compatible**: `GET /api/v1/repos/<owner>/<repo>/releases/latest`
  (or `/tags/<tag>`) with `Authorization: token <token>`; find the asset named
  `catalog-v2.json`; GET its `browser_download_url` with the same header,
  dropped on an origin change.
- **Static HTTPS source**: GET the literal configured URL; attach HTTP Basic
  auth only when a `basic` credential is configured for that exact source.

### Credentials

`catalog-credentials.toml` in the config root, one array-of-tables per
credentialed source, replaced atomically in the same directory (0600 on
Linux, matching ADR 0015's `api-keys.toml` handling):

```toml
[[sources]]
source = "https://gitlab.example.com/me/my-plugins"   # exact configured source string
kind = "token"
token = "glpat-…"

[[sources]]
source = "https://plugins.example.net/catalog-v2.json"
kind = "basic"
username = "me"
password = "…"
```

`source` matches the configured source string verbatim (same matching rule as
`record_source`'s exact-string comparisons). `kind` is `"token"` (GitHub,
GitLab, Gitea-compatible) or `"basic"` (static HTTPS only). The dashboard API
(API placement above) is write-only:

- `POST /api/catalog/sources/<source>/credential` body `{"kind":"token","token":"…"}`
  or `{"kind":"basic","username":"…","password":"…"}` → `204`. Replaces any
  existing credential for that source.
- `DELETE /api/catalog/sources/<source>/credential` → `204`.
- `GET /api/catalog/sources` never includes `token`/`username`/`password`;
  it reports only `"has_credential": true|false` per source, matching ADR
  0015's "never returns it" rule for stored secrets.

Credentials, once set, are invisible to plugins, tools, and overlays; catalog
`packages`/`icon_url`/`banner_url` URLs are always fetched credential-free
(ADR 0023), regardless of origin, even when the catalog/release-metadata
fetch that produced them used a credential.

### Origin scoping on redirects

"Origin" is `(scheme, host, port)`. A credential header (`Authorization`,
`PRIVATE-TOKEN`, HTTP Basic) attaches only to a request whose target origin
equals the source's exact configured origin. The fetch client parses `3xx`
`Location` itself (no automatic redirect following, as 1.x already does for
package downloads) and drops every credential header before re-requesting
whenever the new origin differs from the one the header was sent to. This
applies to the catalog/release-metadata fetch and the forge's own asset
response; it never applies to `packages`/`icon_url`/`banner_url` downloads,
which carry no credential at all per the previous section.

### Private networks

A source gains an optional `private_network: bool` (default `false`),
settable only by the user for their own added source:
`POST /api/catalog/sources/<source>/private-network {"allowed": true|false}`.
When `true`, catalog and credentialed-asset requests to that source's own
exact configured host may target a private or reserved destination; HTTPS
remains mandatory and certificate validation still uses the OS trust store
(so an installed internal CA is honored). The exception never extends to
`packages`/`icon_url`/`banner_url` URLs, even when they resolve to the same
host, and never extends to any other source's host (ADR 0023). This narrows,
for that one host, the private-destination rule `AGENTS.md` otherwise states
for all downloads.

### Trust

`POST /api/catalog/sources/<source>/trust {"trusted": true|false}` sets a
per-source trust flag (default `false` for every source except the fixed
official identities, which are always trusted and cannot be untrusted through
this route). Setting it `true` requires the dashboard to have shown this exact
warning immediately before the request, every time (no "already warned"
skip, since each toggle is a fresh grant):

> Plugins from this source can run in-process, with the same memory access as
> vosu itself. Only trust sources you control or know well.

A `trusted` source's plugins are eligible for `runtime = "in-process"`,
exactly as ADR 0019's eligibility table describes for "a source the user
explicitly trusts". Untrusted-source plugins remain eligible only for
`runtime = "process"`, after the same per-plugin approval 1.x already
requires for user tools (the install plan's `needs_approval` above).

## 1.x coexistence

Every official repository (vosu-tools, vosulay, vosu-overlays) publishes both
`catalog.json` (1.x, unchanged format and resolution:
`releases/latest/download/catalog.json` or `releases/download/<tag>/catalog.json`)
and `catalog-v2.json` (same resolution pattern:
`releases/latest/download/catalog-v2.json` or
`releases/download/<tag>/catalog-v2.json`) in every release during the 1.x
support window (ADR 0022). A GitHub-shorthand 2.0 source resolves to the
`catalog-v2.json` asset; the generic-URL source form is the literal
`…/catalog-v2.json` URL. 1.x readers reject `catalog_version != 1` and so
ignore `catalog-v2.json` entirely; 2.0 readers reject `catalog_version != 2`
and ignore `catalog.json` entirely. Neither format's fetcher needs to inspect
the other.

## First-start migration from 1.x

On first 2.0 start, migration is one-time and idempotent (same pattern as
`src/config/migration.rs`'s `tosu.env` migration):

- **Sources**: the existing `catalog-sources.json` list migrates verbatim
  (same source strings, same enabled flags, same order). Every migrated
  source starts `trusted: false` except the fixed official identities, which
  are always trusted; 1.x had no independent "source trust" concept
  (officialness there came from per-installed-item records, not the source
  list), so this is a fresh, stricter default, not a downgrade of anything a
  1.x user had explicitly granted.
- **Installed official tools/overlays**: for each, look up whether any
  `catalog-v2.json` entry of the same kind, from the same official source,
  lists the 1.x id in its `replaces`. If so, queue that 2.0 entry for install
  using the existing no-overwrite-rename data/settings/trust/enabled/autostart
  migration 1.x already performs for `replaces`-based updates
  (`docs/tools/protocol.md`'s "catalog identity updates"), not a new
  mechanism. If no 2.0 entry claims it, the 1.x install is left in place,
  unmanaged by the 2.0 catalog, until its source publishes one.
- **User tools with a `[service]`**: 2.0 tools cannot run a service
  (`[service]` is removed from v2 manifests), so these migrate to
  `enabled: false`, `status: "disabled"`, `status_detail: "needs-2.0-update"`.
  Durable data and settings are kept untouched so re-enabling after the
  author ships a v2 manifest needs no data migration of its own.
- **User tools without a `[service]`** (UI-only 1.x tools): also migrate to
  `disabled`/`needs-2.0-update`, since a 1.x manifest's `[api]`/`[[requires]]`
  cannot be automatically translated into 2.0 capability ids; only an
  author-published v2 manifest (with `[[requires]]`/`[[optional]]`) re-enables
  one.
- **User overlays**: unaffected and left enabled. Overlays never used the
  removed host API, so service removal does not touch them; they keep working
  unmanaged until a v2 `overlay.toml`-bearing update appears.

## Day-0 release check

The release workflow checks, for the exact fixed official identities
`18-lab-dev/vosu-tools`, `18-lab-dev/vosulay`, and `18-lab-dev/vosu-overlays`,
plus the overlay quickstart `18-lab-dev/vosu-overlay-quickstart`: `GET /repos/<owner>/<repo>/releases/latest`
(unauthenticated GitHub API). Pass requires, for every one of the three
catalog repositories, a response whose asset list contains `catalog-v2.json`;
for the quickstart repository (which is a template, not a catalog source and
so carries no `catalog-v2.json`), pass requires only that its latest release
tag parses as semver with major version `2`. The gate fails closed: a missing
repository, a failed request, or any one of the four checks failing fails the
whole gate, and 2.0 is not published (ADR 0022).

## Open questions

- **Untrusted-source warning copy.** The exact warning text above is this
  spec's proposal, since no other owner currently specifies catalog-source
  dashboard copy; it may need a design/localization pass before shipping.
- **512 MiB package cap for plugins.** This spec keeps 1.x's `MAX_PACKAGE`
  unchanged for plugin packages. Game plugins are expected to stay far below
  it (they are thin memory readers), but a plugin bundling large static assets
  could need a higher cap; revisit if ecosystem data shows otherwise.
- **`overlay.toml` as a new file.** Introducing a manifest file for overlays
  is a deliberate new surface beyond what ADR 0022/0023 state explicitly; it
  is needed so the resolver can plan an overlay's dependencies without
  parsing `metadata.txt` free text. Confirm this is in scope for 2.0 rather
  than a later overlay-ecosystem follow-up.
