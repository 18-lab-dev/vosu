#!/usr/bin/env python3
"""Check the dashboard in a real browser against an isolated test daemon.

Runs tests/dashboard_browser.mjs with an installed playwright index.mjs:
text baselines in the header and status strip (within 0.5 px), keyboard
reachability of every card control on the Overlays, Available and Tools
grids, the 2.0 Tools page (Ready, Blocked and legacy Disabled cards, launching
`fixture-v2` through the single-use exchange, and no call to a deleted
service-era tool route), that the dashboard's own previews are not counted as
connected browser sources, the status strip's session join, the update notice
(minor, major with consent, unsafe release URLs) and Settings filled from
SettingsView or replaced by an alert when that read fails. Then, with
tests/dashboard_notices_browser.mjs, the D20 removed-route notice (M7-N01,
M7-G01–G03): a real 1.x overlay page requests the removed WebSocket and
`/api/ingame`, and the dashboard shows the notice through its own status
refresh, with one daemon WARN for `/api/ingame`. Only test ports
25150/25151 are used.
"""
import argparse
import functools
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import threading
from tool_host_smoke import ROOT, channel_root, curl, wait_for
from catalog_v2_fixture import Catalog, api, dashboard_headers, facts, set_sources

# A classic inline script in <body> that opens a socket immediately, like
# most overlays: the preview marker must already be in place.
OVERLAY = '''<!doctype html>
<html><head><meta charset="utf-8"><title>{name}</title></head>
<body><p>{name}</p>
<script>new WebSocket(`ws://${{location.host}}/websocket/commands?l=${{encodeURIComponent(location.pathname.split('/')[1])}}`);</script>
</body></html>
'''

# A 1.x overlay that still uses two removed routes. Its name is hostile
# markup, which the dashboard must show as text.
HOSTILE_NAME = '<img src=x onerror="window.__d20=1">Hostile & "quoted"'
HOSTILE = '''<!doctype html>
<html><head><meta charset="utf-8"><title>1.x overlay</title></head>
<body><p>1.x overlay</p>
<script>
const socket = new WebSocket(`ws://${location.host}/websocket/v2/subscribe?l=hostile-overlay`);
socket.onopen = () => { document.body.dataset.opened = 'true'; };
socket.onclose = () => { document.body.dataset.socket = 'closed'; };
fetch('/api/ingame').then((response) => { document.body.dataset.ingame = String(response.status); });
</script></body></html>
'''
INGAME_WARNING = '/api/ingame was removed in vosu 2.0'


def removed_routes(static, args, environment):
    """D20: real removed requests, then the notice in the browser."""
    folder = static / 'hostile-overlay'
    folder.mkdir()
    (folder / 'index.html').write_text(HOSTILE)
    (folder / 'metadata.txt').write_text(f'Name: {HOSTILE_NAME}\nAuthor: 1.x\nVersion: 1.0.0\nResolution: 400x100\n')
    # The upstream in-game client carries no identity: the unknown bucket.
    # Every exact request is the ordinary 404; a near miss is not recorded.
    for method in ['GET', 'POST', 'PUT']:
        expected = curl('/api/definitely-unknown', method=method)
        assert curl('/api/ingame?sentinel=never-retain', method=method) == expected, method
        assert expected[0] == 404, expected
    assert curl('/api/ingame/')[0] == 404
    assert curl('/files/skin/?l=overlay-1')[0] == 404
    subprocess.run(['node', str(ROOT / 'tests/dashboard_notices_browser.mjs'), str(args.playwright_module.resolve()),
                    str(args.evidence.resolve()), str(static)], check=True, env=environment)
    status, body = curl('/api/dashboard/status', dashboard=True)
    assert status == 200 and 'ingame' not in json.loads(body), body
    assert 'never-retain' not in body, body


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/vosu')
    parser.add_argument('--evidence', type=Path, default=Path('/tmp/vosu-dashboard-evidence'))
    parser.add_argument('--playwright-module', type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    args.evidence.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='vosu-dashboard-smoke-') as temporary:
        root = Path(temporary)
        environment = dict(os.environ)
        for variable in ['DISPLAY', 'WAYLAND_DISPLAY', 'DBUS_SESSION_BUS_ADDRESS']:
            environment.pop(variable, None)
        # Browsers installed for the real user, found before HOME is isolated.
        environment['PLAYWRIGHT_BROWSERS_PATH'] = os.environ.get(
            'PLAYWRIGHT_BROWSERS_PATH', str(Path.home() / '.cache/ms-playwright'))
        for variable, directory in [('HOME', 'home'), ('XDG_CONFIG_HOME', 'config'),
                                    ('XDG_DATA_HOME', 'data'), ('XDG_CACHE_HOME', 'cache'),
                                    ('XDG_STATE_HOME', 'state'), ('XDG_RUNTIME_DIR', 'runtime')]:
            path = root / directory
            path.mkdir(mode=0o700)
            environment[variable] = str(path)
        static = root / 'static'
        for index in range(6):
            folder = static / f'overlay-{index}'
            folder.mkdir(parents=True)
            (folder / 'index.html').write_text(OVERLAY.format(name=f'overlay-{index}'))
            (folder / 'metadata.txt').write_text(f'Name: Overlay {index}\nAuthor: 18-LAB\nVersion: 1.0.0\nResolution: 400x100\n')
        config = channel_root(binary, root / 'config/vosu')
        config.mkdir(parents=True)
        (config / 'vosu.toml').write_text(f'''ENABLE_AUTOUPDATE = false
OPEN_DASHBOARD_ON_STARTUP = false
DEBUG_LOG = false
SERVER_IP = "127.0.0.1"
SERVER_PORT = 25150
DASHBOARD_IP = "127.0.0.1"
DASHBOARD_PORT = 25151
ALLOWED_IPS = "127.0.0.1,localhost,absolute"
STATIC_FOLDER_PATH = {json.dumps(str(static))}
''')
        # 2.0 web tools from the fixture-v2 package. Without its test plugin
        # the declared `requires` cannot resolve: the two Ready copies drop
        # them, `fixture-v2-blocked` keeps them. `fixture` is a 1.x
        # `tool.toml` package, listed Disabled with needs-2.0-update.
        tools = []
        v2_manifest = (ROOT / 'tests/fixtures/tool-host-v2/manifest.toml').read_text()
        for tool_id in ['fixture-v2', 'fixture-v2-two', 'fixture-v2-blocked']:
            tool = root / 'tools' / tool_id
            shutil.copytree(ROOT / 'tests/fixtures/tool-host-v2', tool)
            text = v2_manifest.replace("id = 'fixture-v2'", f"id = '{tool_id}'")
            if tool_id != 'fixture-v2':
                text = text.replace("name = 'M3 browser fixture'", f"name = '{tool_id}'")
            if tool_id != 'fixture-v2-blocked':
                text = text.split('[[requires]]')[0]
            (tool / 'manifest.toml').write_text(text)
            tools += ['--tool', str(tool)]
        legacy = root / 'tools' / 'fixture'
        shutil.copytree(ROOT / 'tests/fixtures/tool-host', legacy)
        tools += ['--tool', str(legacy)]
        # A catalog-v2 source with two overlays and two tools, so the
        # Available and Tools pages have catalog cards beside the
        # (unreachable) community catalog. Needs a `--features test-harness`
        # build; packages are listed but never downloaded here.
        packages = root / 'packages'
        packages.mkdir()
        server = ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(SimpleHTTPRequestHandler, directory=str(packages)))
        port = server.server_address[1]
        threading.Thread(target=server.serve_forever, daemon=True).start()
        host_range, target = facts(binary)
        catalog = Catalog(packages, port, host_range, target)
        catalog.publish('keyboard',
                        tools=[catalog.tool(f'catalog-tool-{index}', f'Catalog tool {index}',
                                            description='A catalog tool for keyboard checks.') for index in range(2)],
                        overlays=[catalog.overlay(f'source-overlay-{index}', f'Source overlay {index}',
                                                  description='A catalog-source overlay for keyboard checks.')
                                  for index in range(2)])
        environment['VOSU_CATALOG_TEST_SOCKET'] = f'127.0.0.1:{port}'
        with (args.evidence / 'daemon.log').open('w') as log:
            daemon = subprocess.Popen([str(binary), '--update=false', *tools], cwd=root, env=environment,
                                      stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
            try:
                wait_for(lambda: curl('/api/tools', dashboard=True)[0] == 200)
                headers = dashboard_headers()
                set_sources(headers, [catalog.source('keyboard')])
                assert api('refresh', headers, 'POST')['sources'][0]['status'] == 'ok'
                # Unknown routes: the styled page for a person, the plain
                # body for scripts, on both listeners.
                for dashboard in [False, True]:
                    status, body = curl('/definitely-missing/', dashboard=dashboard, headers={'Accept': 'text/html'})
                    assert status == 404 and 'Page not found' in body and '/assets/system.css' in body, body
                    assert curl('/definitely-missing/', dashboard=dashboard) == (404, 'Not Found')
                subprocess.run(['node', str(ROOT / 'tests/dashboard_browser.mjs'),
                                str(args.playwright_module.resolve()), str(args.evidence.resolve())],
                               check=True, env=environment)
                removed_routes(static, args, environment)
                daemon.send_signal(signal.SIGTERM)
                assert daemon.wait(timeout=15) == 0
                # One WARN per daemon process for /api/ingame, in each log
                # sink that records it, and never the request's query.
                logs = [args.evidence / 'daemon.log', *root.rglob('*.log')]
                counts = [path.read_text(errors='replace').count(INGAME_WARNING) for path in logs]
                assert max(counts) == 1 and all(count in (0, 1) for count in counts), list(zip(logs, counts))
                assert not any('never-retain' in path.read_text(errors='replace') for path in logs)
            finally:
                if daemon.poll() is None:
                    daemon.terminate()
                    try:
                        daemon.wait(timeout=15)
                    except subprocess.TimeoutExpired:
                        daemon.kill()
                        daemon.wait(timeout=15)
                server.shutdown()
    print((args.evidence / 'dashboard.json').read_text())


if __name__ == '__main__':
    main()
