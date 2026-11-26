#!/usr/bin/env python3
"""Catalog v2 management against an isolated test-harness daemon.

Two static catalog-v2 sources served from one exact loopback listener carry
a tool, an overlay, a user process plugin naming the D8 internal-path
carve-out, a real in-process plugin (the testkit cdylib beside the binary)
and a catalog game plugin whose matcher names a process this smoke starts.
The HTTP phase proves the route contracts; the required browser phase
(tests/tool_catalog_browser.mjs) drives the dashboard in Chromium: the
exact trust warning before every grant, write-only credentials, the private
network toggle, plan review with approval and D8 disclosure, one
confirmation, job outcome and load reports, uninstall preview, the game
offer and the pending-restart banner. Only test ports 25150/25151 are used.
"""
import argparse
import functools
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import threading
import time
from tool_host_smoke import ROOT, channel_root, curl, wait_for
from catalog_v2_fixture import Catalog, api, dashboard_headers, facts, set_sources

GAME_PROCESS = 'vosufixgame'
USERNAME = 'sentinel-user-4f81'
PASSWORD = 'sentinel-pass-c2d9'
# D1: a plugin `password` setting, written through the dashboard only.
PLUGIN_SECRET = 'sentinel-plugin-secret-6b0e'
ECHO_SETTINGS = [
    {'uniqueID': 'LABEL', 'type': 'text', 'title': 'Label', 'value': 'hello'},
    {'uniqueID': 'TOKEN', 'type': 'password', 'title': 'API token', 'value': ''},
]
# Package paths the fixture server answered, to prove an offer review
# downloads nothing before its plan is confirmed.
REQUESTED = []


class Quiet(SimpleHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_GET(self):
        REQUESTED.append(self.path)
        super().do_GET()


def wait_job(job, timeout=45):
    def done():
        status, body = curl(f'/api/catalog/jobs/{job}', dashboard=True)
        assert status == 200, body
        state = json.loads(body)
        return state if state['status'] != 'running' else None
    return wait_for(done, timeout=timeout)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/vosu')
    parser.add_argument('--evidence', type=Path, default=Path('/tmp/vosu-tools-evidence/catalog'))
    parser.add_argument('--playwright-module', type=Path,
                        default=Path(os.environ['VOSU_PLAYWRIGHT_MODULE']) if os.environ.get('VOSU_PLAYWRIGHT_MODULE') else None)
    args = parser.parse_args()
    if not args.playwright_module:
        raise SystemExit('--playwright-module (or VOSU_PLAYWRIGHT_MODULE) is required: the browser phase is not optional')
    binary = args.binary.resolve(strict=True)
    library = binary.parent / ('vosu_plugin_testkit.dll' if os.name == 'nt' else 'libvosu_plugin_testkit.so')
    if not library.is_file():
        raise SystemExit(f'{library} is missing: run `cargo build -p vosu-plugin-testkit --all-features --locked` first')
    args.evidence.mkdir(parents=True, exist_ok=True)
    evidence = []
    with tempfile.TemporaryDirectory(prefix='vosu-catalog-smoke-') as temporary:
        root = Path(temporary)
        environment = dict(os.environ)
        for variable in ['DISPLAY', 'WAYLAND_DISPLAY', 'DBUS_SESSION_BUS_ADDRESS']:
            environment.pop(variable, None)
        environment['PLAYWRIGHT_BROWSERS_PATH'] = os.environ.get('PLAYWRIGHT_BROWSERS_PATH', str(Path.home() / '.cache/ms-playwright'))
        for variable, directory in [('HOME', 'home'), ('XDG_CONFIG_HOME', 'config'),
                                    ('XDG_DATA_HOME', 'data'), ('XDG_CACHE_HOME', 'cache'),
                                    ('XDG_STATE_HOME', 'state'), ('XDG_RUNTIME_DIR', 'runtime')]:
            path = root / directory
            path.mkdir(mode=0o700)
            environment[variable] = str(path)
        config = channel_root(binary, root / 'config/vosu')
        config.mkdir(parents=True)
        (config / 'vosu.toml').write_text('''ENABLE_AUTOUPDATE = false
OPEN_DASHBOARD_ON_STARTUP = false
DEBUG_LOG = false
SERVER_IP = "127.0.0.1"
SERVER_PORT = 25150
DASHBOARD_IP = "127.0.0.1"
DASHBOARD_PORT = 25151
ALLOWED_IPS = "127.0.0.1,localhost"
''')
        packages = root / 'packages'
        packages.mkdir()
        server = ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Quiet, directory=str(packages)))
        port = server.server_address[1]
        threading.Thread(target=server.serve_forever, daemon=True).start()
        environment['VOSU_CATALOG_TEST_SOCKET'] = f'127.0.0.1:{port}'
        host_range, target = facts(binary)
        catalog = Catalog(packages, port, host_range, target)
        first, second = catalog.source('first'), catalog.source('second')
        catalog.publish('first',
                        tools=[catalog.tool('catalog-tool', 'Catalog tool fixture')],
                        overlays=[catalog.overlay('catalog-overlay', 'Catalog overlay fixture')],
                        plugins=[
                            catalog.plugin('acme.paths', 'process', b'process fixture',
                                           requires=[{'id': 'field:vosu.osu.internal.paths', 'version': '^1'}]),
                            catalog.plugin('acme.echo', 'in-process', library.read_bytes(), settings=ECHO_SETTINGS),
                            catalog.plugin('acme.game', 'process', b'game fixture',
                                           provides=[{'id': f'game:{GAME_PROCESS}', 'version': '*'}],
                                           matcher={'process_names': [GAME_PROCESS]}),
                        ])
        catalog.publish('second', tools=[catalog.tool('second-tool', 'Second source tool')])
        game = root / GAME_PROCESS
        shutil.copy(shutil.which('sleep'), game)
        processes = []
        with (args.evidence / 'daemon.log').open('w') as log:
            # The browser phase uses fixed ports: another smoke's daemon on
            # them would answer with its own fixture listener ("source
            # unavailable"), so refuse to start instead.
            for port in (25150, 25151):
                with socket.socket() as probe:
                    if probe.connect_ex(('127.0.0.1', port)) == 0:
                        raise SystemExit(f'127.0.0.1:{port} is already in use; another catalog smoke is running')
            daemon = subprocess.Popen([str(binary), '--update=false'], cwd=root, env=environment,
                                      stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
            try:
                wait_for(lambda: curl('/api/tools', dashboard=True)[0] == 200)
                headers = dashboard_headers()
                view = set_sources(headers, [first, second])
                assert [s['source'] for s in view['sources']] == [first, second], view
                assert not any(s['trusted'] or s['private_network'] or s['has_credential'] for s in view['sources'])
                # A6b.4: list replacement cannot grant trust or private access.
                status, body = curl('/api/catalog/sources', dashboard=True, method='POST', headers=headers,
                                    data={'sources': [{'source': first, 'enabled': True, 'trusted': True}]})
                assert status == 400, body
                # Mutations need dashboard provenance and CSRF; overlay listener has no catalog routes.
                assert curl('/api/catalog/refresh', dashboard=True, method='POST', data={})[0] == 403
                assert curl('/api/catalog/sources')[0] == 404
                # The removed 1.x immediate install route is a plain 404 (A7 inventory) and installs nothing.
                assert curl('/api/tools/catalog-tool/install', dashboard=True, method='POST', headers=headers, data={})[0] == 404
                view = api('refresh', headers, 'POST')
                assert [s['status'] for s in view['sources']] == ['ok', 'ok'], view
                entries = api('entries', headers)['entries']
                assert {e['id'] for e in entries} == {'catalog-tool', 'catalog-overlay', 'acme.paths', 'acme.echo', 'acme.game', 'second-tool'}
                evidence.append('Two static v2 sources fetched through S0 validation; list replacement cannot smuggle trust; '
                                'CSRF, listener and removed 1.x install route refused')
                # The D8 disclosure on the real daemon, where vosu.osu is bundled.
                plan = api('install', headers, 'POST', {'kind': 'plugin', 'id': 'acme.paths', 'source': first})
                assert plan['errors'] == [] and plan['adds'][0]['requires_internal_paths'] is True, plan
                assert plan['adds'][0]['needs_approval'] and plan['adds'][0]['runtime'] == 'process'
                evidence.append('acme.paths plan: native process plugin, user trust, approval required, D8 requires_internal_paths true')
                # Write-only credential: 204, presence only.
                path = f'sources/{first.replace(":", "%3A").replace("/", "%2F")}/credential'
                status, body = curl(f'/api/catalog/{path}', dashboard=True, method='POST', headers=headers,
                                    data={'kind': 'basic', 'username': USERNAME, 'password': PASSWORD})
                assert status == 204 and body == '', (status, body)
                assert api(path, headers) == {'has_credential': True, 'available': True}
                assert curl(f'/api/catalog/{path}', dashboard=True, method='DELETE', headers=headers)[0] == 204
                evidence.append('Credential POST/DELETE 204 with presence-only GET')
                processes.append(subprocess.Popen([str(game), '600'], cwd=root))
                subprocess.run(['node', str(ROOT / 'tests/tool_catalog_browser.mjs'), str(args.playwright_module.resolve()),
                                str(args.evidence.resolve()), first, second, USERNAME, PASSWORD, PLUGIN_SECRET],
                               check=True, env=environment)
                evidence.append('Browser: trust warning before every grant (cancel keeps untrusted; no remembered bypass), '
                                'write-only credential, private toggle, plan review with approval + D8, one confirmation, '
                                'load reports, uninstall preview, game offer, pending-restart banner; plugins and web tools '
                                'in separate sections; plugin settings write-only (blank after a failed save); untrust names '
                                'the still-running in-process plugin and never says it stopped')
                installed = api('installed', headers)['packages']
                assert {(p['id'], p['installed_for']) for p in installed} == {('acme.paths', 'requested'), ('acme.echo', 'requested'), ('catalog-overlay', 'requested')}, installed
                assert (root / 'data').exists()
                status = api('status', headers)
                assert 'acme.echo' in status['pending_restart'], status
                # D1: reviewing the game offer downloaded nothing (its review was cancelled).
                assert not any('acme.game' in path for path in REQUESTED), REQUESTED
                status, body = curl('/api/plugins/acme.echo/settings', dashboard=True)
                assert status == 200 and PLUGIN_SECRET not in body, (status, body)
                settings = json.loads(body)
                assert settings['secrets'] == {'TOKEN': True} and settings['values']['TOKEN'] == '', settings
                assert settings['values']['LABEL'] == 'from the dashboard', settings
                log_text = (args.evidence / 'daemon.log').read_text(errors='replace')
                for secret in [USERNAME, PASSWORD, PLUGIN_SECRET]:
                    assert secret not in log_text
                evidence.append('Installed graph after the browser flow: process plugin, in-process plugin and overlay; '
                                'the untrusted running in-process plugin is pending restart; the offer review fetched no '
                                'package; the plugin password is stored but never read back; sentinels absent from the log')
                daemon.send_signal(signal.SIGTERM)
                assert daemon.wait(timeout=20) == 0
            finally:
                for process in processes:
                    process.kill()
                if daemon.poll() is None:
                    daemon.terminate()
                    try:
                        daemon.wait(timeout=15)
                    except subprocess.TimeoutExpired:
                        daemon.kill()
                        daemon.wait(timeout=15)
                server.shutdown()
    (args.evidence / 'smoke.json').write_text(json.dumps(evidence, indent=2) + '\n')
    print('\n'.join(evidence))


if __name__ == '__main__':
    main()
