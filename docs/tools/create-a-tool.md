---
type: Tutorial
title: "Create a tool"
description: "Build a Python tool, connect its UI and CLI, and publish an installable catalog."
---

# Create a tool

A vosu **tool** is a local application hosted beside your overlays. It can be a static browser UI, an HTTP service in any language, a terminal command, or a combination. vosu discovers its manifest, supervises its service, serves its UI, and brokers settings and selected-game context. It is a separate process with your OS permissions, **not a sandboxed plugin**. Here you will build **Hello Python**: a counter with a write button, live score, selected beatmap, settings, and a CLI, then publish it for other users.

## 1. Prepare

You need vosu 1.2.0, Python 3.11+ available as `python`, a browser, and a vosu source checkout for the maintained example. No pip packages, JavaScript build, or running osu! client are needed. The commands below use a Linux POSIX shell; the example itself also runs on Windows with Python on PATH. For a native Rust variant, use [vosu-tool-sdk](authoring.md#rust-with-vosu-tool-sdk) from [vosu-tools](https://github.com/shiruviaa/vosu-tools); the host contract is identical.

From the vosu repository root:

```sh
cargo build --locked
export VOSU_SOURCE="$PWD"
export VOSU_BIN="$VOSU_SOURCE/target/debug/vosu"
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
python --version
export LAB="$(mktemp -d /tmp/vosu-tool-tutorial.XXXXXX)"
export TOOL="$LAB/hello-python"
mkdir -p "$TOOL/web"
```

**Check:** Python reports 3.11 or newer and `$TOOL` is an empty directory except for `web/`. Keep this shell open. If you use an installed vosu instead, set `VOSU_BIN` to its absolute executable path and keep `VOSU_SOURCE` pointing at the checkout.

## 2. Give it a manifest

Create `$TOOL/tool.toml`:

```toml
#:schema https://raw.githubusercontent.com/shiruviaa/vosu/main/docs/tools/tool.schema.json
manifest_version = 1
id = "hello-python"
name = "Hello Python"
version = "0.1.0"

[ui]
root = "web"
```

Create `$TOOL/web/index.html`:

```html
<!doctype html>
<html lang="en">
<meta charset="utf-8">
<title>Hello Python</title>
<link rel="stylesheet" href="/tools/_host/v1/theme.css">
<body class="v-app">
<header class="v-bar"><span class="v-bar__title">Hello Python</span></header>
<main class="v-main"><section class="v-panel"><div class="v-panel__body">Your first tool.</div></section></main>
</body>
</html>
```

The schema line gives supporting editors completion; vosu also checks semantic versions, IDs, paths, and dependencies. No service is required for a UI-only tool. See the [manifest reference](protocol.md#manifest-tooltoml).

A native process used only by your UI or CLI can declare `[service] role = "backend"` with `startup = "on-demand"`. It remains supervised but does not appear as a separate SERVICE interface. Background services (the default role) expose service controls and **Start with vosu**; see [service roles and startup](protocol.md#manifest-tooltoml).

**Check:** `python -c 'import os,tomllib; print(tomllib.load(open(os.environ["TOOL"]+"/tool.toml","rb"))["id"])'` prints `hello-python`.

## 3. Run an isolated vosu

Do not reuse your normal config, data, cache, runtime directory, or listener ports. This setup leaves an already-running vosu untouched. It disables startup browser opening and automatic updates, uses spare ports **25850/25851**, and keeps all tutorial state in `$LAB`.

```sh
export HOME="$LAB/home"
export XDG_CONFIG_HOME="$LAB/config"
export XDG_DATA_HOME="$LAB/data"
export XDG_CACHE_HOME="$LAB/cache"
export XDG_STATE_HOME="$LAB/state"
export XDG_RUNTIME_DIR="$LAB/runtime"
export VOSU_PROCESS_EVENTS=poll
mkdir -p "$HOME" "$XDG_CONFIG_HOME/vosu" "$XDG_DATA_HOME" \
  "$XDG_CACHE_HOME" "$XDG_STATE_HOME" "$XDG_RUNTIME_DIR" "$LAB/static"
chmod 700 "$XDG_RUNTIME_DIR"
cat > "$XDG_CONFIG_HOME/vosu/vosu.toml" <<EOF
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

Leave vosu in the foreground; Ctrl+C stops it and its service. Open <http://127.0.0.1:25851/tools> in a browser. Under **Hello Python**, choose **Open**.

**Check:** the Tools page lists a ready UI tool and the new tab says “Your first tool.” The daemon prints both listener URLs. If these ports are occupied, stop this tutorial instance and change both numbers consistently.

For a Linux test with a private loopback and no outside network access, launch the last command inside a private network namespace:

```sh
unshare -rn sh -c 'ip link set lo up && exec "$VOSU_BIN" --tool "$TOOL"'
```

Run the headless browser/test client in that same namespace, not your normal desktop browser: its loopback is separate. This requires unprivileged user namespaces and `ip` from iproute2. Do not use root to work around restrictions. The ordinary spare-port setup above is suitable for manual development; namespace isolation is recommended for automated checks.

## 4. Add the real service

Stop the tutorial instance with Ctrl+C. This tutorial uses the maintained [example service](../../examples/tools/hello-python/main.py), rather than a second copy that can drift. Copy it into your initially empty project:

```sh
cp "$VOSU_SOURCE/examples/tools/hello-python/main.py" "$TOOL/main.py"
cat >> "$TOOL/tool.toml" <<'EOF'

[service]
command = ["python", "main.py"]
startup = "on-demand"

[api]
read = ["/v1/count", "/v1/context", "/v1/ratings"]
write = ["/v1/increment"]

[permissions]
host = ["context"]
EOF
"$VOSU_BIN" --tool "$TOOL"
```

Read `main.py` alongside this step. Its `host()` helper authenticates calls back to vosu using the launch environment, not a hardcoded address:

```python
    def host(path):
        request = Request(host_url + path,
                          headers={'X-Vosu-Tool-Token': token})
        with urlopen(request, timeout=2) as response:
            return json.load(response)
```

Every incoming service request, including `/healthz`, checks the token:

```python
            supplied = self.headers.get('X-Vosu-Tool-Token', '')
            if not hmac.compare_digest(supplied.encode(), token.encode()):
                return self.reply(401, {'error': 'unauthorized'})
```

The lifecycle is small but essential: bind **loopback port 0**, emit one flushed readiness line, then stay alive until stdin EOF. vosu chooses the public URL; never expose this internal port or token to browser code.

```python
    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    server.daemon_threads = True
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    print(f'vosu-tool-ready {server.server_port}', flush=True)
    try:
        while sys.stdin.buffer.read(4096):
            pass
    finally:
        server.shutdown()
        server.server_close()
        worker.join()
```

**Check:** open <http://127.0.0.1:25850/tools/hello-python/api/v1/count>. It starts the on-demand service and returns `{"count":0,"settings":{}}`. The count is intentionally in memory; restarting resets it. Use `VOSU_TOOL_DATA_DIR` for future durable state, not the package directory. See the full [process contract](protocol.md#process-contract).

## 5. Connect a UI and live game data

In a second terminal, re-export `TOOL` and `VOSU_SOURCE` to their absolute paths (`printf '%s\\n' "$TOOL" "$VOSU_SOURCE"` in the first shell), or stop vosu temporarily to use that shell. Replace the starter page with the maintained complete page:

```sh
cp "$VOSU_SOURCE/examples/tools/hello-python/web/index.html" "$TOOL/web/index.html"
```

No frontend build is needed. Open it again from **Tools → Hello Python → Open**. The page uses `/tools/_host/v1/theme.css`, `.v-app`, `.v-bar`, `.v-panel`, `.v-stat`, and `.v-btn`; do not copy dashboard CSS into your project. [Design.md](design.md) explains layout and the component reference at `/tools/_host/v1/design`.

The page imports the bridge and waits before using it:

```js
import { vosu } from '/tools/_host/v1/bridge.js';
```

```js
  await vosu.ready;
  status.textContent = vosu.canWrite ? 'Session' : 'Read-only';
  dot.dataset.state = vosu.canWrite ? 'ok' : 'warn';
  await act('v1/count');
```

`act()` calls `vosu.fetch()`, checks the HTTP status, and displays errors. Fetch paths are relative to **this tool's API**, not the UI directory. Increment uses POST, not GET.

```js
async function request(path, options) {
  const response = await vosu.fetch(path, options);
  const body = await response.json();
  if (!response.ok) throw new Error(body.error || `HTTP ${response.status}`);
  count.textContent = body.count;
  if (body.settings) document.querySelector('#label').textContent = body.settings.label;
}
```

Only after installing its handlers does the page enable writes with
`increment.disabled = !vosu.canWrite`; this avoids an enabled but inert button
while the initial service requests are still loading.

For live telemetry, subscribe with **JSON pointers**, not property names or dotted paths. These two selectors request just the current checksum and score. The bridge reconnects automatically; your callback must reset state on the initial frame, merge updates, and remove absent fields:

```js
  const fields = ['/beatmap/checksum', '/play/score'];
  const values = new Map();
  const stop = vosu.subscribe(fields, frame => {
    if (frame[0] === 2) {
      status.textContent = frame[2];
      return;
    }
    if (frame[0] === 0) values.clear();
    for (const [id, value] of frame[2]) values.set(id, value);
    for (const id of frame[3] || []) values.delete(id);
    document.querySelector('#checksum').textContent = values.get(0) || 'No selected beatmap';
    document.querySelector('#score').textContent = values.get(1) ?? '—';
    if (frame[0] === 0 || frame[2].some(([id]) => id === 0) || (frame[3] || []).includes(0)) {
      context().catch(error => { status.textContent = error.message; });
    }
  });
  window.addEventListener('pagehide', stop, { once: true });
```

Field IDs are indexes in `fields`: initial `[0, revision, pairs]`, update `[1, revision, pairs, removedIds]`, error `[2, code, message]`. An omitted optional field is not a broken connection. With no osu! running, “No selected beatmap” and a zero/absent score are expected. See [bridge behavior](protocol.md#browser-bridge-for-uis) and [renderer fields and sampling](../../README.md#renderer-state-v2).

**Check:** Increment increases the count, Refresh re-reads it, and no game means an honest empty selected-map state. Step 11 shows how to exercise live updates without a game.

## 6. See the read/write boundary

Open <http://127.0.0.1:25850/tools/hello-python/> **directly** in another tab. Reads still work but the button is disabled and the bar says **Read-only**. A direct POST is rejected with 403 even if someone re-enables the button in devtools. The service token protects the internal listener; the browser never receives it.

The dashboard's **Open** grants a tool-bound session via a single-use launch code. `vosu.fetch()` attaches that session. Reloading the tab loses it by design; open from the dashboard again to regain write access. Never store sessions in localStorage, sessionStorage, IndexedDB, or cookies; never frame overlays or open same-origin pages with an opener. Overlay code shares that origin. Read [the security rules](authoring.md#read-versus-write) before adding navigation.

**Check:** one tab can increment; the direct tab can only observe the result after Refresh. GET/HEAD routes in `[api].read` are public to allowed overlay peers. Mutations belong in `[api].write`; unlisted routes are 404. Permission declarations do not sandbox your Python process.

## 7. Add editable settings

Copy the example's `$TOOL/settings.json`:

```sh
cp "$VOSU_SOURCE/examples/tools/hello-python/settings.json" "$TOOL/settings.json"
```

Its whole content is:

```json
[
  {"uniqueID":"label","type":"text","title":"Counter label","description":"Label shown below the count.","value":"Practice clicks"}
]
```

The host watches this file. On the Tools page, choose this tool's **Settings**, change **Counter label**, and save. In the tool, press Refresh: the label changes. The count read handler asks authenticated `GET /tools/_host/v1/settings` for effective values, so it sees saved edits and defaults without parsing host files. The UI can also use `await vosu.settings()` for public values; password settings are excluded. Tools do not use overlay profiles.

**Check:** the label changes without restarting Python. Do not write into vosu's private settings files; see [settings and jobs](authoring.md#settings-jobs-and-live-data).

## 8. Follow the selected beatmap

The service already has `[permissions].host = ["context"]` and a `/v1/context` handler. It fetches `/tools/_host/v1/context`; `beatmap` is null when no game/selection is available. The complete context can contain absolute paths, so our **public** read route deliberately returns only the basename of `beatmap.file`, not the whole host response.

The UI refreshes context whenever checksum selector 0 changes, including reconnects and removal. That follows the **selected instance** instead of scanning Songs or assuming the first osu! process. A context request and a later file fetch are not a transaction; compare checksum identity if you do work across requests. See [host context](protocol.md#host-api-for-services).

**Check:** with no game you see “No selected beatmap”, not an exception or a made-up file. With a game selected, its file name appears; changing selections triggers another context fetch. No `songs:write` permission is needed for this read.

## 9. Make another tool optional

The `/v1/ratings` handler calls the read route `/tools/mania-ratings/api/v1/beatmap/current` using **our own** token. Do **not** add `[[requires]]` for this optional integration: a declared required dependency would block Hello Python when missing.

```python
                except HTTPError as error:
                    if error.code in (404, 503):
                        return self.reply(200, {'available': False, 'status': error.code})
                    return self.reply(502, {'error': f'ratings returned HTTP {error.code}'})
```

404 means the optional tool or current result is absent; 503 means unavailable (for example disabled or untrusted). We show that state instead of inventing ratings. Other HTTP failures remain errors. Only read routes are allowed without a declaration. If your tool truly needs another tool, add `[[requires]]` with its id/version to both manifest and catalog; that also grants dependency write-route authority, so use it deliberately.

**Check:** without Mania ratings installed, the counter still works and the page says **Optional tool unavailable (404)**. If installed and ready with a selected map, it says **Available**. See [proxy access rules](protocol.md#http-surface-on-the-overlay-listener).

## 10. Add a CLI and identity

Stop the tutorial daemon, then append:

```sh
cat >> "$TOOL/tool.toml" <<'EOF'

[cli]
command = ["python", "main.py"]
summary = "Read the Hello Python counter"
EOF
```

The same Python file checks `VOSU_TOOL_INTERFACE=cli` and performs one authenticated read of its **own service** before exiting. It does not emit readiness or wait for stdin in CLI mode. This keeps the service the only state owner.

Add the example artwork and its manifest paths:

```sh
cp "$VOSU_SOURCE/examples/tools/hello-python/web/icon.svg" "$TOOL/web/icon.svg"
cp "$VOSU_SOURCE/examples/tools/hello-python/web/banner.svg" "$TOOL/web/banner.svg"
python - <<'PY'
import os
from pathlib import Path
path = Path(os.environ['TOOL']) / 'tool.toml'
text = path.read_text()
text = text.replace('\n[ui]\n', '\nicon = "web/icon.svg"\nbanner = "web/banner.svg"\n\n[ui]\n')
path.write_text(text)
PY
"$VOSU_BIN" --tool "$TOOL"
```

The icon is square 256×256 SVG; the banner is 1200×400 SVG (3:1). Use your own nonempty artwork for publishing. See [art guidance](authoring.md#manifest-walkthrough).

In a second terminal with the **same tutorial HOME and XDG directories**, run:

```sh
"$VOSU_BIN" x --list
"$VOSU_BIN" x hello-python
"$VOSU_BIN" x hello-python --json
"$VOSU_BIN" x hello-python --help
```

Use the same namespace too if you selected private-network testing. The launcher discovers the tutorial daemon through its isolated broker file, not a port argument. Do not run these commands from your normal desktop shell's environment, where they would target your normal daemon. For convenience, stop the foreground daemon and relaunch it in the background in the same shell (`"$VOSU_BIN" --tool "$TOOL" & TUTORIAL_PID=$!`); stop it afterwards with `kill -INT "$TUTORIAL_PID"; wait "$TUTORIAL_PID"`.

**Check:** the Tools card shows the art and command summary; CLI reports the same count as the UI, JSON includes effective settings, and `--help` exits normally. The [launcher reference](protocol.md#command-line-tools-vosux) covers signals, tokens, and exit codes.

## 11. Test before sharing

Use [the full author checklist](authoring.md#test-a-tool):

1. Validate manifest structure, IDs, confined paths, dependency versions, and unknown fields.
2. Launch against a fake host on a temporary port with fresh data/cache dirs and all launch environment variables; capture stdout and keep stdin piped.
3. Require one readiness line and authenticated `/healthz`; missing/wrong tokens must be 401.
4. Exercise reads, writes, errors, cancellation for jobs you implement, live reconnects, default settings, and edited settings. This counter has no long-running job to cancel.
5. Close stdin and require exit within five seconds, also with connected clients.
6. Through isolated vosu, check direct/read-only access, dashboard writes, optional dependency absence, disable, and restart (the count resets).

In **Tools**, choose **Hello Python → Details → Logs** and leave **Follow output** on. Each increment writes `Counter incremented to N`; readiness, stdout/stderr, and host lifecycle messages appear there. Never log tokens or sessions. For a failed launch, inspect these logs before trying the internal port directly.

For game-free UI testing, Playwright's headless `route_web_socket` can stand in for the **telemetry transport only**, while the tool, bridge, service, and host stay real. Install Playwright in a disposable test environment, launch its browser in the same namespace as vosu, and register this before navigating to the tool:

```python
import json
sockets = []
async def telemetry(ws):
    sockets.append(ws)
    ws.on_message(lambda message: ws.send(json.dumps([0, 1, [[0, 'demo-map'], [1, 100]]])))
await page.route_web_socket('**/websocket/v2/subscribe', telemetry)
await page.goto('http://127.0.0.1:25850/tools/hello-python/')
await page.locator('#score').filter(has_text='100').wait_for()
sockets[0].send(json.dumps([1, 2, [[1, 200]], []]))
await page.locator('#score').filter(has_text='200').wait_for()
```

This is explicitly **simulated telemetry**, not evidence of native game memory reading. Context still honestly reports no selected map. Test context separately against a fake host returning a selected beatmap; do not inject fixtures into your running personal vosu. Capture both schemes with `page.emulate_media(color_scheme='light')` / `'dark'` before opening a direct tab; dashboard launches use the dashboard's chosen scheme instead. Confirm neither browser console errors nor clipped controls at desktop sizes.

**Check:** 100 changes to 200 without Refresh. Disconnect/reconnect should receive a fresh initial frame, not preserve stale IDs. Then run the repository's relevant [tool tests](authoring.md#test-a-tool).

## 12. Package and publish

Stop the development daemon before testing installation: a `--tool` override intentionally cannot be overwritten by a catalog. The maintained [finished example](../../examples/tools/hello-python/) is also a comparison point for everything above. To give your package useful catalog metadata and the complete manifest, use its final manifest (same id/version/routes/CLI), license, and dependency-free packager:

```sh
cp "$VOSU_SOURCE/examples/tools/hello-python/tool.toml" "$TOOL/tool.toml"
cp "$VOSU_SOURCE/examples/tools/hello-python/LICENSE" "$TOOL/LICENSE"
cp "$VOSU_SOURCE/examples/tools/hello-python/package.py" "$TOOL/package.py"
python "$TOOL/package.py" \
  --base-url https://github.com/YOUR-ACCOUNT/YOUR-REPO/releases/download/v0.1.0 \
  --out "$LAB/dist"
python -m zipfile -l "$LAB/dist/vosu-tool-hello-python-0.1.0-any.zip"
```

Replace `YOUR-ACCOUNT/YOUR-REPO` with **your** publishing repository before generating the catalog. Keep the example's license when redistributing its code; add your own copyright/license for your changes. Before sharing, give your actual project a unique id/name/author/description and change the example's hardcoded own-service path in `main.py` to match its new id. The package helper reads the manifest metadata. Describe **Python 3.11+ on PATH** in the release notes and catalog description: target `any` means platform-independent code, not a bundled interpreter.

**Check:** output contains a ZIP and `catalog.json`; ZIP members include root `tool.toml`, `main.py`, `settings.json`, `LICENSE`, and `web/…`, not an enclosing `hello-python/` directory. `catalog.json` records target `any`, exact ZIP byte size, and SHA-256 computed after writing. The helper intentionally packages only runtime files, not itself, caches, or your data. The [normative catalog/package format](protocol.md#catalog-and-package-format) belongs to vosu; [vosu-tools packaging](https://github.com/shiruviaa/vosu-tools/blob/main/docs/catalog.md) describes the larger first-party build pipeline.

Publish only when you intend to distribute:

1. Create a GitHub release tagged `v0.1.0` in that repository and upload **both** generated files as release assets. The ZIP filename and catalog URL must match exactly. Publishing sends your code to others; inspect the ZIP first for private data and verify runtime/license requirements.
2. Users open **Settings → Catalog sources**, add `YOUR-ACCOUNT/YOUR-REPO@v0.1.0` (pinned) or `YOUR-ACCOUNT/YOUR-REPO` (latest release), and save. The host resolves these to the release's `catalog.json`, not a repository source file.
3. Alternatively host both files on public HTTPS, regenerate using that directory as `--base-url`, and add `https://your-domain.example/releases/v0.1.0/catalog.json`. HTTPS art URLs may be added as optional `icon_url`/`banner_url`; the included art already works after installation.
4. On **Tools → Available**, install Hello Python. Your source is **third-party**: installation verifies bytes but does not grant trust. Review the author, runtime, and requested **context** permission, then explicitly approve/trust and enable it in the dashboard. Open it from the dashboard for write access.

Do not ask users to disable HTTPS or package verification. Production downloads reject private/reserved hosts, including localhost. For local automated installation tests only, build `cargo build --locked --features test-harness`, serve `$LAB/dist` with `python -m http.server 25860 --bind 127.0.0.1 --directory "$LAB/dist"`, set `VOSU_TOOL_CATALOG_TEST_SOCKET=127.0.0.1:25860` on the isolated daemon, and use `http://tools.invalid:25860/catalog.json` as the test source. In a **test copy** of the catalog change the package URL to `http://tools.invalid:25860/vosu-tool-hello-python-0.1.0-any.zip`; do not change size/digest. This exact loopback exception exists only in test-harness builds. Launch without `--tool`, install via Available, and test the separate trust gate. Stop the HTTP server and daemon afterwards. Never publish that test catalog or distribute a test-harness binary.

Earlier sources win duplicate ids; adding a later catalog cannot override an earlier entry. Only the protocol's exact official source identities are auto-trusted, not arbitrary GitHub URLs or tags. For updates, bump the manifest/catalog version, regenerate ZIP and digest, publish new assets, and let users update through their configured source. See [source and trust semantics](protocol.md#catalog-sources-and-installation).

**Final check:** a fresh isolated vosu installs from the catalog with no development override; it stays untrusted until approved, then its UI, settings, CLI, and read/write boundary work as before. Stop all tutorial/test processes when finished. Open a fresh normal shell to return to your normal HOME/XDG environment; keep or remove only the temporary `$LAB` directory you created.
