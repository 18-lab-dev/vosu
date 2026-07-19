# Precise input validation

This guide is the maintainer workflow for validating vosu's additive precise-input data on osu!stable and osu!lazer. The user-facing wire contract remains the [precise API description in the root README](../README.md#http-api). The relevant implementations are [`src/api/precise.rs`](../src/api/precise.rs), the [stable key-overlay reader](../src/memory/stable/readers.rs), the [lazer key-overlay reader](../src/memory/lazer/reader.rs), and the [validation harness](../tools/input-validation/input_validation.py).

The harness validates two different properties:

1. **Source-reported press counts:** counter changes establish how many presses the attached client reported.
2. **Sampled edge observation:** state changes and their gameplay timestamps are recorded only when an edge is visible at vosu's precise polling cadence.

These are not equivalent. A complete short tap can occur between samples: its press count still advances, but neither its press nor release state may be observed. The capture is therefore **not a lossless edge log**. `countDelta` is authoritative for source-reported presses over a capture segment; `observedPresses` and `observedReleases` count only sampled edges. `estimatedMissedPresses` is `countDelta - observedPresses`, clamped at zero.

## Command scope

Run harness commands from the repository root with the shown prefix:

- `capture` reads `/websocket/v2/precise` for a fixed interval and writes an accounting artifact.
- `scan-maps` recursively finds osu!mania maps at or above a requested long-note ratio. It helps select sustained-input workloads; it does not inspect a running client.
- `navigate` emits the known keyboard path from an unobstructed idle logo to song select or multiplayer, optionally entering song-search text. It does not verify the resulting screen.
- `autoplay` parses an `.osu` file into a deterministic osu!, taiko, catch, or mania input plan and runs it from a local monotonic-clock origin after `--start-delay`. It is useful for plan and geometry checks but is not synchronized to the live game timeline.
- `live-autoplay` runs the same plan against `/websocket/v2`, waits for a fresh `selectPlay` to early-`play` transition, and schedules actions against live gameplay time. Optional seeded jitter applies only to mania.

Examples:

```console
cd tools/input-validation && uv run python run.py scan-maps --songs "<songs-directory>" --min-ln-ratio 0.50 --limit 10
```

```console
cd tools/input-validation && uv run python run.py capture --base-url "http://127.0.0.1:24050" --label "<platform>-<client>-<scenario>" --out "<artifact.json>" --timeout 60
```

```console
cd tools/input-validation && uv run python run.py navigate --client stable --target song-select --backend dry-run --search "<map-search-text>"
```

```console
cd tools/input-validation && uv run python run.py autoplay "<map.osu>" --backend dry-run --tap-duration 40 --keys "D,F,J,K"
```

```console
cd tools/input-validation && uv run python run.py live-autoplay "<map.osu>" --base-url "http://127.0.0.1:24050" --backend dry-run --no-start-key --tap-duration 40 --keys "D,F,J,K" --start-timeout 15
```

Use `uv run python run.py <command> --help` as the authority for mode-specific defaults and accepted key names.

## Prerequisites and platform backends

Common prerequisites:

- Run vosu and the intended osu! client as the same desktop user. The precise and normal v2 WebSockets at `--base-url` must be reachable.
- Know the exact `.osu` file loaded in the client. Keep the map, ruleset, mods, key bindings, and harness `--keys` mapping fixed between repetitions.
- Start from an unobstructed, known UI state. Input injection is global; focus the intended client before a real run and keep other applications away from the injected controls.
- Install `uv` and allow it to resolve the harness environment from `tools/input-validation/pyproject.toml`.

Backends accepted by `navigate`, `autoplay`, and `live-autoplay` are:

- `dry-run`: no operating-system input injection. Use it first to inspect navigation or the parsed action plan. A dry-run `live-autoplay` still connects to live vosu state and waits for the game transition; with `--no-start-key`, the maintainer starts the map manually.
- `windows`: keyboard scan-code and absolute-pointer injection through Windows `SendInput`. Run in the same interactive desktop session as the game. `auto` selects this backend on Windows.
- `uinput`: Linux virtual keyboard and pointer devices through `/dev/uinput`. The current user needs appropriately provisioned write access. Configure access with the host's normal device/udev policy; do not run vosu as root and do not make the device globally writable. `auto` selects this backend on Linux.

Real osu! and catch plans require `--playfield "x,y,width,height"`. On Linux, pointer plans also require `--desktop "x,y,width,height"`, and the playfield rectangle must lie within that desktop rectangle. Mania and taiko plans are keyboard-only.

### Windows osu!stable prerequisites

Stable precise keys come from the active stable profile's in-game key overlay. Before capture:

- Ensure the **active profile used by the running client exposes the in-game key overlay**. The configuration observed to work used `KeyOverlay = 1`.
- Confirm vosu is attached to the intended stable process/client, not another stable instance, lazer instance, tournament client, or stale process. The mode, map, gameplay time, and key names returned by the API must correspond to the window receiving injected input.
- Treat an empty `keypresses.keys` array as a failed prerequisite, not as evidence that no keys were pressed.

Do not copy profiles containing credentials into artifacts, and do not weaken host security to make process attachment or injection work.

## Dry-run-first workflow

1. Select a representative map. For mania, use `scan-maps` when long notes or overlapping holds are part of the target workload.
2. Dry-run navigation from the same clean idle state intended for the real run. Check every emitted key and the search text before using `--backend auto`.
3. Dry-run `autoplay` for the exact map and key mapping. Count planned key-down actions per physical key; these are the expected source-reported press deltas once a baseline sample has been established.
4. For osu! or catch, verify pointer coordinates and rectangles in dry-run output before supplying real `--playfield` and, on Linux, `--desktop` geometry.
5. If validating synchronization, dry-run `live-autoplay` with `--no-start-key` and manually perform one fresh map start. This checks the selected client and `selectPlay` to `play` flow without injecting gameplay actions.
6. Only then replace `dry-run` with the explicit platform backend or `auto`. Keep emergency access to the client and terminal; the harness releases keys and closes virtual devices on normal and exceptional exits, but focus loss or process termination should still be checked manually.

A successful dry run proves parsing, mapping, and—in the live case—timeline synchronization. It does not prove that the game accepts real injected input or that vosu samples every edge.

## Repeatable end-to-end live validation

Use a quiet, fixed scenario and preserve the exact inputs used for each repetition.

1. Start vosu, start exactly the intended client, and verify that normal v2 state identifies the expected map, mode, state, and gameplay time. On stable, also satisfy the active-profile key-overlay requirement above.
2. Select the exact map but do not start it. Keep the client focused.
3. Dry-run the exact plan mapping. Record expected key-down counts per physical control and the intended physical-control-to-API-key-identity mapping.
4. Before the measured capture, complete a disposable warm-up play with the same ruleset and key bindings, exercising every mapped physical control at least once. Return to the exact selected map without changing the client, profile, ruleset, or bindings.
5. Run a short preflight capture without generated input, let it finish, and inspect the completed artifact to confirm that every expected API accounting identity is present:

   ```console
   cd tools/input-validation && uv run python run.py capture --base-url "http://127.0.0.1:24050" --label "<platform>-<client>-preflight" --out "<preflight-artifact.json>" --timeout 5
   ```

   `capture` emits no live readiness signal and writes its artifact only when it finishes. The preflight is therefore a separate completed run, not a live gate inside the measured capture.
6. In terminal A, start a fresh measured capture. Record and wait a fixed lead interval spanning several configured precise polling periods before any generated press:

   ```console
   cd tools/input-validation && uv run python run.py capture --base-url "http://127.0.0.1:24050" --label "<platform>-<client>-<map-and-settings>" --out "<artifact.json>" --timeout 60
   ```

7. Only after the recorded lead interval, run the synchronized plan in terminal B. A mania example is:

   ```console
   cd tools/input-validation && uv run python run.py live-autoplay "<map.osu>" --base-url "http://127.0.0.1:24050" --backend auto --tap-duration 40 --keys "D,F,J,K" --start-timeout 15 --start-key ENTER
   ```

   If the map must be started manually, replace `--start-key ENTER` with `--no-start-key`. For a controlled robustness run on mania, add `--jitter-ms 2 --seed 1`; preserve both values with the artifact.

8. Let the measured capture run past the final release so the ending state is sampled. Do not change ruleset, restart the map, switch clients, or reload the active profile during the measured interval.
9. Review both JSON artifacts against the evidence checklist below. Repeat with the same map and parameters on each client/platform under validation. Keep each artifact separate and label it unambiguously.

`live-autoplay` exiting successfully means its plan completed without a detected timeline/backend error. `capture` exiting successfully means it completed and encountered keypress data. **Neither exit code is the evidence gate.** Likewise, nonempty `events` proves only that at least one edge was sampled; it does not prove that every short press or release was observed.

## Capture artifact schema

The artifact is a JSON object with:

- `metadata`
  - `label`: the exact operator-supplied label; it is not an independently detected client identity.
  - `baseUrl` and `endpoint`: capture source (`endpoint` is `/websocket/v2/precise`).
  - `startedUnixNs`: wall-clock Unix nanoseconds at capture start.
- `events`: one entry per **sampled state transition**.
  - `key`: accounting identity, including any duplicate/segment suffix.
  - `edge`: `press` or `release`.
  - `apiGameplayMs`: the API's nullable gameplay timestamp for that sampled edge.
  - `receiveNs`: local monotonic nanoseconds when the WebSocket frame was received. It is suitable for within-process gap comparisons, not wall-clock or cross-host correlation.
  - `count`: source counter in that sampled frame.
- `summary`
  - `keys`: per-key, per-segment accounting.
    - `mode`, `segment`: ruleset number and accounting segment.
    - `firstCount`, `lastCount`, `countDelta`: baseline, final counter, and their difference.
    - `observedPresses`, `observedReleases`: edges visible in sampled state.
    - `estimatedMissedPresses`: `max(0, countDelta - observedPresses)`.
    - `pressDurationsMs`: `releasedAt - pressedAt` only for sampled press/release pairs.
  - `receiveFrameGapsNs`: count, minimum, maximum, and mean spacing between received precise frames.
  - `invariantFailures`: timestamp consistency failures detected during accounting.
  - `frames`: number of processed precise frames.

Repeated source names are disambiguated with `#2`, `#3`, and so on. A mode change, an empty key list, or a source counter decrease closes the current accounting segment; later identities receive `@2`, `@3`, and so on. Compare counts only within one identity/segment. A new segment is not automatically a defect, but an unplanned one invalidates a supposedly continuous run and needs an explanation.

### Interpreting the evidence

- **Counts:** before comparing planned physical-control counts with `countDelta`, record the physical-control-to-API-accounting-identity mapping. Require a completed warm-up/preflight that contains every expected identity, then start the measured capture with a recorded lead interval before its first generated press and sample every identity after its last. Counts validate source-reported presses even when complete taps fell between precise samples.
- **Observed edges:** `observedPresses`, `observedReleases`, and `events` describe only state transitions that survived sampling. Equality with expected counts in one run supports complete observation for that run; it is not a general losslessness guarantee.
- **Miss estimate:** a positive `estimatedMissedPresses` explicitly says some source-counted presses were not observed as press edges. It does not estimate missed releases and must not be added to `countDelta`.
- **Timestamps:** `pressedAt`/`releasedAt` become `apiGameplayMs` only when the corresponding edge is sampled. They are gameplay time, not receipt time. Do not synthesize timestamps for counter jumps. `pressDurationsMs` covers only paired sampled edges and is not a duration distribution for all presses.
- **Receive gaps:** compare `receiveFrameGapsNs` with the configured precise polling cadence and with other runs. Large gaps increase the opportunity for short taps to disappear between samples; low gaps still do not prove lossless observation.
- **Invariant failures:** `missing` means a sampled edge lacked its corresponding timestamp; `changedWithoutEdge` means a timestamp changed without the matching sampled state transition; `regressing` means a non-null timestamp moved backward within an accounting identity. Any entry needs investigation before using timestamp/edge evidence.

## Pass and evidence checklist

A capture is acceptable evidence only when all items relevant to the claim are recorded and reviewed:

- [ ] Artifact `metadata` identifies the platform, client, map/scenario, and settings without secrets, and `baseUrl`/`endpoint` are expected.
- [ ] The daemon was independently confirmed attached to the intended client; API mode, map, state, and timeline matched the focused window.
- [ ] A completed preflight after the disposable warm-up contains every expected API accounting identity; client, profile, ruleset, and bindings remained unchanged before the measured run.
- [ ] The measured capture started before the first generated press with a recorded lead interval spanning several configured polling periods, and sampled every expected identity after its final release.
- [ ] Map, ruleset, mods, key bindings, harness mapping, tap duration, backend, geometry, start behavior, and optional jitter/seed are fixed and recorded.
- [ ] Expected planned key-down counts are recorded per physical control.
- [ ] The physical-control-to-API-accounting-identity mapping, including duplicate/segment suffixes, is recorded before comparing counts; each mapped identity appears in the intended mode segment and its `countDelta` equals the expected press count.
- [ ] Any extra segment or duplicate-name suffix is explained; otherwise the run is rejected and repeated.
- [ ] `invariantFailures` is empty for claims involving sampled edges or timestamps.
- [ ] Receive-gap statistics are plausible for the configured cadence, with anomalies explained.
- [ ] `observedPresses`, `observedReleases`, `estimatedMissedPresses`, and paired durations are reported rather than hidden.
- [ ] The conclusion says **validated source-reported counts and sampled edges**, not “lossless edges,” unless a separate lossless source was used to establish that stronger claim.

For a claim that every edge in this particular workload was sampled, additionally require expected presses to equal `observedPresses`, expected releases to equal `observedReleases`, `estimatedMissedPresses == 0`, and an explainable press/release sequence for every key. Even then, the claim is limited to that artifact and workload. Exit code 0 and nonempty `events` are never substitutes for this review.

## Troubleshooting

### Empty or absent keys

- On stable, verify the active profile exposes the in-game overlay; the observed working setting was `KeyOverlay = 1`. Restart or reload the client/profile as normally required by stable, then re-check before capturing.
- Confirm the client is in a state where its key-overlay collection exists. Lazer may transiently expose an empty list while the player is loading.
- Verify vosu's precise endpoint rather than the legacy fixed-key object. A capture that never receives keypress data fails; an empty interval can also create a segment boundary.

### Wrong client or key names

- Match API map, ruleset, state, and gameplay time to the focused window before injection.
- Remove ambiguity from concurrently running clients/instances and restart vosu attachment if necessary. Do not trust the free-form capture `label` as proof of attachment.
- Ensure `--keys` uses the physical controls configured in that exact client/profile and matches the plan's number/order of controls.

### Timeline does not start or resets

- `live-autoplay` requires a fresh `selectPlay` to early-`play` transition. Begin with the intended map selected; use `--no-start-key` if Enter is not the correct start action.
- A restart, retry, ruleset change, client switch, or return to song select during execution can reset gameplay time. Repeat from a clean selection instead of combining pre- and post-reset evidence.
- If startup is legitimately slow, increase `--start-timeout`; this changes only how long the harness waits for the fresh start flow.

### Pointer geometry is wrong

- Re-measure the desktop-space playfield rectangle; it is `x,y,width,height`, not two corners.
- Supply `--playfield` for real osu!/catch runs. On Linux uinput also supply the full virtual `--desktop` rectangle and keep the playfield within it.
- Re-run with `--backend dry-run` after any window move, resize, display-scale, resolution, or monitor-layout change.

### Injection permissions or ignored input

- On Linux, confirm `/dev/uinput` exists, the uinput module is available, and the current user has narrowly provisioned access. Use normal host policy rather than root execution or permissive global device modes.
- On Windows, keep the harness and game in the same interactive desktop and avoid privilege/session mismatches that prevent `SendInput` from reaching the client.
- Confirm focus and key bindings with a harmless dry run before repeating capture.

### Short taps have counts but no edges

This is expected when both state transitions occur between precise samples. Confirm the press through `countDelta`; expect `estimatedMissedPresses` to rise and do not invent timestamps or releases. To exercise edge/timestamp observation, repeat a controlled workload with a longer `--tap-duration` and enough separation for multiple precise samples per state. Preserve the original short-tap artifact: it is useful evidence that count accounting survives sampled-edge loss.

## Cleanup and retention

After a run:

- Verify no generated key remains pressed and that any Linux virtual input device created by the harness has disappeared. If the harness or interpreter was forcibly terminated, release affected controls through the normal desktop/client workflow before another run.
- Restore temporary client/profile, key-binding, polling, window, and overlay settings. Do not leave a shared profile altered.
- Retain the raw JSON artifact, exact `.osu` input identity, expected per-key plan counts, client/platform versions, relevant non-secret settings, and the exact harness arguments together. Never store account tokens, profile credentials, or unrelated configuration dumps.
- Keep failed and anomalous artifacts when they explain a bug; label them as failed evidence rather than overwriting them with a later pass.
- Re-run this workflow when precise polling/accounting changes, stable or lazer memory readers/offsets change, backend scheduling or injection changes, capture schema changes, supported client builds change, or a platform/client upgrade alters attachment, overlay, or timing behavior.
