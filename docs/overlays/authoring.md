# Authoring an overlay

An overlay is a static browser page (for example, an OBS browser source) served by vosu on the overlay listener, `http://127.0.0.1:24050` by default. It reads **API v1** state, resources and trees under the listener's peer and browser-Origin policy. It does not get a launch session, cannot invoke plugin actions and must not receive a broker credential or API key for state reads. Use a [web tool](../tools/authoring.md) if you need a dashboard-authorized UI for actions instead.

Start from the [overlay quickstart template](https://github.com/18-lab-dev/vosu-overlay-quickstart) on its 2.x line. The [API v1 worked examples](../v2/api-v1.md#worked-examples) describe the protocol without a library. osu! is the supported game; use common fields where they express what your overlay needs.

## Declare a static package

Place `manifest.toml` and `index.html` at the root of the ZIP. A `kind = "overlay"` package can declare `resolution = [width, height]` and `[[requires]]`/`[[optional]]` for `field:` and `resource:` capabilities. Put a capability in `optional` if the overlay can render without it. It cannot declare `provides`, `action:` requirements, a native runtime, a service or a `[ui]` section. Port old `metadata.txt` fields into the manifest; if both files exist, the manifest wins. Settings remain in the usual separate file (see [overlay settings](../../README.md#overlay-settings-profiles)). Follow the [overlay manifest and catalog contract](../v2/catalog-v2.md#kind--overlay) when publishing an `any` package in `catalog-v2.json`.

## Choose state and handle focus

Read one state document with `GET /api/v1/sessions/{id|active}/state` (or one namespace with `/state/{namespace}`). For live values use `WS /api/v1/sessions/{id|active}/subscribe` and send a JSON array of non-overlapping RFC 6901 JSON Pointers as the first message. Prefer the [common namespaces](../v2/state-schema.md): for a combo and accuracy display, select `/vosu.play/combo`, `/vosu.play/accuracy` and `/vosu.play/accuracyKind`. Accuracy is a fraction (`0`–`1`); multiply by 100 to display a percentage and use `accuracyKind` to identify the formula. Use `/vosu.osu/…` for facts that are specific to osu!, such as pp and catcher state.

A feed addresses values by index into the requested pointer array. Replace the entire baseline on frame `[0, revision, pairs]`, apply changed pairs and delete removed indices on `[1, revision, pairs, removed]`, and show the error and reconnect as appropriate on `[2, code, message]`. Absence or `null` means unknown; do not keep stale values or show zero. `active` follows focus and a new focus sends a fresh initial frame. The initial upgrade can return 404 when no game is focused: display a waiting state and retry with backoff. An explicit id stays on that session and its feed ends on detach; use [session discovery or events](../v2/api-v1.md#sessions) to choose another. Close feeds you no longer need.

**Precision follows the selection.** There is no separate API v1 precise feed and HTTP reads create no sampling demand. Subscribe only to the fields you show: for example `/vosu.osu/play/hitOffsets`, `/vosu.osu/play/keyHitErrors` or `/vosu.osu/keys/overlay` when you need those facts. Subscribe to the physical `keys.overlay` slots rather than inferring them from the logical `keys.all` list. Close or narrow the subscription when you stop displaying those values.

## Load media by reference, never by path

Use the `url` from `/vosu.chart/resources/background` as an image source, or the audio reference as an audio source. The file, audio and background are [resource references](../v2/api-v1.md#resources-and-trees); the chart set and `/vosu.skin/tree` are tree references. Fetch the tree URL for its index, then fetch a listed relative member under that tree URL. No host path is needed or available from API v1. A retired reference can return 410: reread the current state and use the replacement. Do not construct a local file path from state, scrape legacy path fields or embed private data into an overlay.

## When the dashboard reports a removed API

The notice means an overlay or unidentified client requested a removed route. It is advisory: **the overlay remains enabled**. Update the overlay or port the affected feature using the [removed-route replacement table](../v2/migration-from-1x.md#overlays-that-used-removed-apis). The dashboard names installed overlays only when it can attribute the request, never from Origin alone. The old selected-field socket and skin index have API v1 replacements; `/api/ingame` has none and its notice points to vosulay. See the [2.0 migration guide](../migration/2.0.md) for operator steps.

A separate companion website may run its own optional sidecar and read API v1 as allowed by the listener policy. That is the website's deployment, not part of an installable static overlay ZIP or a dependency of the native vosu daemon.
