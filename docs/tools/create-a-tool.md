---
type: Tutorial
title: "Create a tool"
description: "Build a 2.0 web tool, connect it to live state and a plugin action, and publish an installable catalog."
---

# Create a tool

A vosu **tool** is a static web application hosted beside your overlays. vosu serves its files, the dashboard opens it, and it reads live game state and calls plugin actions through [API v1](../v2/api-v1.md) from the browser. A tool runs no process of its own: native work belongs to a [plugin](../v2/plugin-abi.md) action. Here you will build **Hello web**: a live chart and play readout with the chart's background, a setting, and an optional beatmap import action, then publish it for other users.

## 1. Prepare

You need a vosu 2.0 build, a browser, and a vosu source checkout for the maintained example. Python 3.11+ is used only by the packaging step. No JavaScript build or running game client is needed. The commands below use a Linux POSIX shell.

From the vosu repository root:

```sh
cargo build --locked
export VOSU_SOURCE="$PWD"
export VOSU_BIN="$VOSU_SOURCE/target/debug/vosu"
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
export LAB="$(mktemp -d /tmp/vosu-tool-tutorial.XXXXXX)"
export TOOL="$LAB/hello-web"
mkdir -p "$TOOL/web"
```

**Check:** `"$VOSU_BIN" --version` prints a 2.0 version and `$TOOL` is empty except for `web/`. Keep this shell open. If you use an installed vosu instead, set `VOSU_BIN` to its absolute executable path and keep `VOSU_SOURCE` pointing at the checkout.

## 2. Give it a manifest

Create `$TOOL/manifest.toml`:

```toml
manifest_version = 1
kind = "tool"
id = "hello-web"
name = "Hello web"
version = "0.1.0"
host = ">=2.0.0-alpha.1, <3.0.0"

[ui]
root = "web"
```

Create `$TOOL/web/index.html`:

```html
<!doctype html>
<html lang="en">
<meta charset="utf-8">
<title>Hello web</title>
<link rel="stylesheet" href="/tools/_host/v1/theme.css">
<body class="v-app">
<header class="v-bar"><span class="v-bar__title">Hello web</span></header>
<main class="v-main"><section class="v-panel"><div class="v-panel__body">Your first tool.</div></section></main>
</body>
</html>
```

`host` names the vosu versions the package supports; an alpha build matches only a range that includes its prerelease. See the [package reference](protocol.md#package).

## 3. Run an isolated vosu

Do not reuse your normal config, data, cache, runtime directory or listener ports. This setup leaves an already-running vosu untouched, disables startup browser opening and automatic updates, uses spare ports **25850/25851**, and keeps all tutorial state in `$LAB`. Prerelease builds keep their configuration in a `prerelease/` subdirectory.

```sh
export HOME="$LAB/home"
export XDG_CONFIG_HOME="$LAB/config"
export XDG_DATA_HOME="$LAB/data"
export XDG_CACHE_HOME="$LAB/cache"
export XDG_STATE_HOME="$LAB/state"
export XDG_RUNTIME_DIR="$LAB/runtime"
export VOSU_PROCESS_EVENTS=poll
CONFIG_DIR="$XDG_CONFIG_HOME/vosu"
case "$("$VOSU_BIN" --version)" in *-*) CONFIG_DIR="$CONFIG_DIR/prerelease" ;; esac
mkdir -p "$HOME" "$CONFIG_DIR" "$XDG_DATA_HOME" \
  "$XDG_CACHE_HOME" "$XDG_STATE_HOME" "$XDG_RUNTIME_DIR" "$LAB/static"
chmod 700 "$XDG_RUNTIME_DIR"
cat > "$CONFIG_DIR/vosu.toml" <<EOF
ENABLE_AUTOUPDATE = false
OPEN_DASHBOARD_ON_STARTUP = false
SERVER_IP = "127.0.0.1"
SERVER_PORT = 25850
DASHBOARD_IP = "127.0.0.1"
DASHBOARD_PORT = 25851
STATIC_FOLDER_PATH = "$LAB/static"
ALLOWED_IPS = "127.0.0.1,localhost,absolute"
EOF
"$VOSU_BIN" --tool "$TOOL"
```

Leave vosu in the foreground; Ctrl+C stops it. Open <http://127.0.0.1:25851/tools> in a browser. Under **Hello web**, choose **Open**.

**Check:** the Tools page lists Hello web as running and the new tab says “Your first tool.” The daemon prints both listener URLs. If these ports are occupied, stop this instance and change both numbers consistently.

For a Linux test with a private loopback and no outside network access, launch the last command inside a private network namespace:

```sh
unshare -rn sh -c 'ip link set lo up && exec "$VOSU_BIN" --tool "$TOOL"'
```

Run the headless browser or test client in that same namespace: its loopback is separate. This requires unprivileged user namespaces and `ip` from iproute2. Do not use root to work around restrictions.

vosu watches the package: edits to `manifest.toml`, `settings.json` and the UI apply without restarting it. Reload the tab to see UI changes.

## 4. Connect the bridge

In a second shell, re-export `TOOL` and `VOSU_SOURCE` to their absolute paths (`printf '%s\n' "$TOOL" "$VOSU_SOURCE"` in the first shell). Replace the starter page with the maintained complete page and its art:

```sh
cp "$VOSU_SOURCE/examples/tools/hello-web/web/index.html" "$TOOL/web/index.html"
cp "$VOSU_SOURCE/examples/tools/hello-web/web/icon.svg" "$TOOL/web/icon.svg"
cp "$VOSU_SOURCE/examples/tools/hello-web/web/banner.svg" "$TOOL/web/banner.svg"
```

Read the page alongside the next steps. It imports the [bridge](protocol.md#browser-bridge) and waits for it before deciding what the tab may do:

```js
import { vosu } from '/tools/_host/v2/bridge.js';
```

```js
try {
  await vosu.ready;
} catch (error) {
  toast(error.message, 'error');
}
$('access').textContent = vosu.canWrite ? 'Session' : 'Read-only';
$('run').disabled = !vosu.canWrite;
```

**Open** gave this tab a single-use launch code in the URL fragment. The bridge removed it from the URL before anything else ran and exchanged it for a session that lives only in the page's memory. Now open <http://127.0.0.1:25850/tools/hello-web/> **directly** in another tab, or reload: the badge says **Read-only** and **Import** stays disabled. That is by design: only a tab the dashboard opened can invoke actions, and reopening from the dashboard restores it. Never store anything that grants authority in `localStorage`, `sessionStorage`, IndexedDB or cookies, never open same-origin pages with an opener, and never frame overlays: overlays share this origin.

**Check:** the launched tab shows **Session**; the direct tab shows **Read-only**, and its address bar never contained a launch code.

## 5. Follow live state

The page subscribes to [API v1 JSON Pointers](protocol.md#state-through-api-v1) into the focused game's session document:

```js
const fields = [
  '/vosu.client/name',
  '/vosu.chart/title',
  '/vosu.chart/artist',
  '/vosu.chart/resources/background',
  '/vosu.status/status',
  '/vosu.play/combo',
  '/vosu.play/accuracy',
];
const values = new Map();
const stop = vosu.subscribe(fields, frame => {
  if (frame[0] === 2) { toast(frame[2], 'error'); return; }
  if (frame[0] === 0) values.clear();
  for (const [index, value] of frame[2]) values.set(index, value);
  for (const index of frame[3] || []) values.delete(index);
  render();
}, status => { /* "connecting" | "open" | "closed" */ });
addEventListener('pagehide', stop, { once: true });
```

Frames address fields by their index in `fields`: initial `[0, revision, pairs]`, update `[1, revision, pairs, removed]`, error `[2, code, message]`. An initial frame replaces everything; it arrives after every reconnect and whenever another game session takes focus. A removed index means the value is now unknown, not zero. Declare what you read in the manifest so the install plan can show it:

```sh
cat >> "$TOOL/manifest.toml" <<'EOF'

[[requires]]
id = "field:vosu.chart"
version = "^1.0.0"

[[requires]]
id = "field:vosu.play"
version = "^1.0.0"
EOF
```

**Check:** with no game running the bar says **Waiting for a game**: the focused-session subscription is refused until a game is focused, and the bridge retries with backoff. With a game running, the chart and play values follow it; restarting vosu reconnects on its own.

## 6. Show the chart background

State never carries file paths. `/vosu.chart/resources/background` is a resource reference, `{"url": "/api/v1/resources/<id>", "type": "image/…"}`, and the page uses its `url` as an ordinary image source:

```js
const background = values.get(3);
if (background?.url) $('background').src = background.url;
else $('background').removeAttribute('src');
```

The image request carries no session: reads are public under the overlay listener's peer policy. Declare the optional capability:

```sh
cat >> "$TOOL/manifest.toml" <<'EOF'

[[optional]]
id = "resource:chart.background"
version = "^1.0.0"
EOF
```

**Check:** selecting a chart in the game shows its background; a chart without one hides the image instead of showing a broken link.

## 7. Add a setting

```sh
cp "$VOSU_SOURCE/examples/tools/hello-web/settings.json" "$TOOL/settings.json"
```

Its whole content is:

```json
[
  {"uniqueID":"decimals","type":"number","title":"Accuracy decimals","description":"Digits shown after the decimal point.","value":2}
]
```

On the Tools page, choose this tool's **Settings**, change **Accuracy decimals** and save. The page reads `await vosu.settings()` at load and whenever its tab regains focus. That read is public, so `password` settings are never returned: a web tool cannot hold a secret.

**Check:** after saving and focusing the tool tab again, accuracy shows the new number of decimals.

## 8. Call a plugin action

Tools reach native work only through plugin actions. The official osu! plugin declares `vosu.osu.beatmap.import`, which imports a `.osz` archive into the focused osu! client. Grant it as optional, so the tool still works without osu!:

```sh
cat >> "$TOOL/manifest.toml" <<'EOF'

[[optional]]
id = "action:vosu.osu.beatmap.import"
version = "^1.0.0"
EOF
```

The page invokes it on the focused session with progress and cancellation:

```js
const result = await vosu.invoke('vosu.osu.beatmap.import', { sourcePath: $('source').value }, {
  signal: controller.signal,
  onProgress: progress => $('meter').style.setProperty('--v-value', String(progress?.fraction ?? 0)),
});
```

The capability id is `action:<plugin-id>.<name>`; the call uses the bare `<plugin-id>.<name>`. This action's session mode is `required`, so the call omits `session` (the focused session); an action declared `none` takes `session: null`. Aborting the signal cancels the job, even while it is still being created. Errors carry a `code`: `forbidden` without a session or a resolved grant, `unknown_session` when no game is focused, `invalid_arguments`, `cancelled`, or the plugin's own code. See [Actions](protocol.md#actions).

**Check:** in the launched tab with no game, **Import** reports that no osu! session is focused. In the direct tab the button stays disabled. With osu! running and a real `.osz` path, stable reports the import as queued and lazer as submitted.

## 9. Package and publish

Stop the development daemon before testing installation: a `--tool` override intentionally replaces an installed tool of the same id. The maintained [finished example](../../examples/tools/hello-web/) is also a comparison point. Use its final manifest (with description, author and art), license and dependency-free packager:

```sh
cp "$VOSU_SOURCE/examples/tools/hello-web/manifest.toml" "$TOOL/manifest.toml"
cp "$VOSU_SOURCE/examples/tools/hello-web/LICENSE" "$TOOL/LICENSE"
cp "$VOSU_SOURCE/examples/tools/hello-web/package.py" "$TOOL/package.py"
python "$TOOL/package.py" \
  --base-url https://github.com/YOUR-ACCOUNT/YOUR-REPO/releases/download/v0.2.0 \
  --out "$LAB/dist"
python -m zipfile -l "$LAB/dist/vosu-tool-hello-web-0.2.0-any.zip"
```

Replace `YOUR-ACCOUNT/YOUR-REPO` with **your** publishing repository before generating the catalog. Keep the example's license when redistributing its code and add your own for your changes. Before sharing, give your project a unique id, name, author and description; the packager reads them from the manifest.

**Check:** the output has a ZIP, `catalog-v2.json` and `extracted/tools/hello-web/any/`. The ZIP holds `manifest.toml`, `settings.json`, `LICENSE` and `web/…` at its root, not inside an enclosing `hello-web/` directory. The catalog entry mirrors the manifest and records target `any`, the exact ZIP byte size and its SHA-256.

Validate both with the SDK's [package checker](../v2/catalog-v2.md#package-checker-af10), which runs the same manifest validation as vosu. Its registry context names the providers your capability ids refer to; the optional import action belongs to the bundled `vosu.osu` plugin:

```sh
python - "$VOSU_SOURCE" "$LAB/registry.json" <<'EOF'
import json, sys
defs = json.load(open(sys.argv[1] + '/docs/v2/schemas/common-defs.schema.json'))
json.dump({'format': 1, 'official_sources': [], 'namespace_revisions': [], 'common_defs_schema': defs,
           'registrations': [{'kind': 'plugin', 'id': 'vosu.osu', 'source': 'bundled',
                              'official': True, 'builtin': True, 'replaces': []}]},
          open(sys.argv[2], 'w'))
EOF
cargo run --locked -p vosu-plugin-api --example vosu-package-check -- \
  --catalog "$LAB/dist/catalog-v2.json" --packages-root "$LAB/dist/extracted" \
  --host-version "$("$VOSU_BIN" --version | awk '{print $NF}')" \
  --source YOUR-ACCOUNT/YOUR-REPO --registry-context "$LAB/registry.json"
```

**Check:** it prints `vosu-package-check: passed: 1 packages` and exits 0. Without the `vosu.osu` registration it fails with `manifest.unknown-namespace` at the action id. Publishers without a vosu checkout install the checker from the registry, as the checker section describes.

Publish only when you intend to distribute:

1. Create a GitHub release tagged `v0.2.0` and upload **both** the ZIP and `catalog-v2.json` as release assets. The ZIP filename and the catalog URL must match exactly. Inspect the ZIP for private data first.
2. Users open **Settings → Catalog sources** and add `YOUR-ACCOUNT/YOUR-REPO@v0.2.0` (pinned) or `YOUR-ACCOUNT/YOUR-REPO` (latest release). A source can also be any HTTPS URL ending in `/catalog-v2.json`.
3. On **Tools → Available**, they choose **Install…**. vosu shows an install plan with every package, its source and the capabilities the tool requests, and installs only after they confirm it once. Your source is third-party: the tool stays untrusted until they trust it, then enable and open it from the dashboard.

Do not ask users to disable HTTPS or package verification; production downloads reject private and reserved hosts, including localhost. For a local automated installation test only, build `cargo build --locked --features test-harness`, serve `$LAB/dist` with `python -m http.server 25860 --bind 127.0.0.1 --directory "$LAB/dist"`, regenerate the catalog with `--base-url https://catalog.invalid:25860`, set `VOSU_CATALOG_TEST_SOCKET=127.0.0.1:25860` on the isolated daemon, and add `https://catalog.invalid:25860/catalog-v2.json` as the source. That loopback exception exists only in test-harness builds. Launch without `--tool` for this test.

For updates, bump the manifest version, regenerate the ZIP, digest and catalog, and publish new assets; users update through their configured source. See [Catalog sources and installation](protocol.md#catalog-sources-and-installation).

**Final check:** a fresh isolated vosu installs Hello web from the catalog with no development override; it stays untrusted until approved, then its live state, settings and read-only boundary work as before. Stop every tutorial process when finished, and open a fresh shell to return to your normal HOME and XDG environment; keep or remove the temporary `$LAB` directory.
