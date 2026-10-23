#!/usr/bin/env python3
"""Check the dashboard in a real browser against an isolated test daemon.

Runs tests/dashboard_browser.mjs with an installed playwright index.mjs:
text baselines in the header and status strip (within 0.5 px), keyboard
reachability of every card control on the Overlays, Available and Tools
grids, and that the dashboard's own previews are not counted as connected
browser sources. Only test ports 25150/25151 are used.
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
from tool_host_smoke import ROOT, curl, wait_for

# A classic inline script in <body> that opens a socket immediately, like
# most overlays: the preview marker must already be in place.
OVERLAY = '''<!doctype html>
<html><head><meta charset="utf-8"><title>{name}</title></head>
<body><p>{name}</p>
<script>new WebSocket(`ws://${{location.host}}/websocket/commands?l=${{encodeURIComponent(location.pathname.split('/')[1])}}`);</script>
</body></html>
'''


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
        config = root / 'config/vosu'
        config.mkdir()
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
        tools = []
        for tool_id in ['fixture', 'fixture-two']:
            tool = root / 'tools' / tool_id
            shutil.copytree(ROOT / 'tests/fixtures/tool-host', tool)
            manifest = tool / 'tool.toml'
            manifest.write_text(manifest.read_text().replace('id = "fixture"', f'id = "{tool_id}"'))
            tools += ['--tool', str(tool)]
        # A catalog source with two overlays, so the Available page has
        # source cards beside the (unreachable) community catalog. This needs
        # a `--features test-harness` build.
        packages = root / 'packages'
        packages.mkdir()
        server = ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(SimpleHTTPRequestHandler, directory=str(packages)))
        port = server.server_address[1]
        threading.Thread(target=server.serve_forever, daemon=True).start()
        base = f'http://tools.invalid:{port}'
        overlays = [{'id': f'source-overlay-{index}', 'name': f'Source overlay {index}', 'version': '1.0.0',
                     'description': 'A catalog-source overlay for keyboard checks.', 'author': '18-LAB',
                     'homepage': None, 'resolution': [640, 120], 'preview': None, 'requires_tools': [],
                     'package': {'url': f'{base}/overlay-{index}.zip', 'sha256': '0' * 64, 'size': 1}}
                    for index in range(2)]
        catalog_tools = [{'id': f'catalog-tool-{index}', 'name': f'Catalog tool {index}', 'version': '1.0.0',
                          'description': 'A catalog tool for keyboard checks.', 'author': '18-LAB',
                          'homepage': None, 'protocol': 1, 'interfaces': ['ui'], 'requires': [],
                          'packages': {'any': {'url': f'{base}/tool-{index}.zip', 'sha256': '0' * 64, 'size': 1}}}
                         for index in range(2)]
        (packages / 'catalog.json').write_text(json.dumps({'catalog_version': 1, 'published': '2026-10-23T00:00:00Z',
                                                           'tools': catalog_tools, 'overlays': overlays}))
        (config / 'tools').mkdir()
        (config / 'tools/catalog-sources.json').write_text(json.dumps([{'source': f'{base}/catalog.json', 'enabled': True}]))
        environment['VOSU_TOOL_CATALOG_TEST_SOCKET'] = f'127.0.0.1:{port}'
        with (args.evidence / 'daemon.log').open('w') as log:
            daemon = subprocess.Popen([str(binary), '--update=false', *tools], cwd=root, env=environment,
                                      stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
            try:
                wait_for(lambda: curl('/api/tools', dashboard=True)[0] == 200)
                # Unknown routes: the styled page for a person, the plain
                # body for scripts, on both listeners.
                for dashboard in [False, True]:
                    status, body = curl('/definitely-missing/', dashboard=dashboard, headers={'Accept': 'text/html'})
                    assert status == 404 and 'Page not found' in body and '/assets/system.css' in body, body
                    assert curl('/definitely-missing/', dashboard=dashboard) == (404, 'Not Found')
                subprocess.run(['node', str(ROOT / 'tests/dashboard_browser.mjs'),
                                str(args.playwright_module.resolve()), str(args.evidence.resolve())],
                               check=True, env=environment)
                daemon.send_signal(signal.SIGTERM)
                assert daemon.wait(timeout=15) == 0
            finally:
                if daemon.poll() is None:
                    daemon.terminate()
                    try:
                        daemon.wait(timeout=15)
                    except subprocess.TimeoutExpired:
                        daemon.kill()
                        daemon.wait()
                server.shutdown()
    print((args.evidence / 'dashboard.json').read_text())


if __name__ == '__main__':
    main()
