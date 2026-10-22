#!/usr/bin/env python3
"""Exercise two mixed catalog sources against an isolated test-harness daemon.

Optional --playwright-module points to an installed playwright index.mjs for
headless dark/light screenshots. Only test ports 25150/25151 are used.
"""
import argparse
import functools
import hashlib
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import tempfile
import threading
import zipfile
from tool_host_smoke import ROOT, DASHBOARD, curl, wait_for


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/vosu')
    parser.add_argument('--evidence', type=Path, default=Path('/tmp/vosu-tools-evidence/catalog'))
    parser.add_argument('--playwright-module', type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    args.evidence.mkdir(parents=True, exist_ok=True)
    evidence = []
    with tempfile.TemporaryDirectory(prefix='vosu-catalog-smoke-') as temporary:
        root = Path(temporary)
        environment = dict(os.environ)
        for variable in ['DISPLAY', 'WAYLAND_DISPLAY', 'DBUS_SESSION_BUS_ADDRESS']:
            environment.pop(variable, None)
        environment['PLAYWRIGHT_BROWSERS_PATH'] = os.environ.get('PLAYWRIGHT_BROWSERS_PATH', str(args.evidence.resolve() / 'browser-cache'))
        for variable, directory in [('HOME', 'home'), ('XDG_CONFIG_HOME', 'config'),
                                    ('XDG_DATA_HOME', 'data'), ('XDG_CACHE_HOME', 'cache'),
                                    ('XDG_STATE_HOME', 'state'), ('XDG_RUNTIME_DIR', 'runtime')]:
            path = root / directory
            path.mkdir(mode=0o700)
            environment[variable] = str(path)
        config = root / 'config/vosu'
        config.mkdir()
        (config / 'vosu.toml').write_text('''ENABLE_AUTOUPDATE = false
OPEN_DASHBOARD_ON_STARTUP = false
DEBUG_LOG = false
SERVER_IP = "127.0.0.1"
SERVER_PORT = 25150
DASHBOARD_IP = "127.0.0.1"
DASHBOARD_PORT = 25151
ALLOWED_IPS = "127.0.0.1,localhost,absolute"
''')
        packages = root / 'packages'
        packages.mkdir()
        handler = functools.partial(SimpleHTTPRequestHandler, directory=str(packages))
        server = ThreadingHTTPServer(('127.0.0.1', 0), handler)
        port = server.server_address[1]
        threading.Thread(target=server.serve_forever, daemon=True).start()
        base = f'http://tools.invalid:{port}'
        sources = [{'source': f'{base}/{name}/catalog.json', 'enabled': True} for name in ['first', 'second']]
        (config / 'tools').mkdir()
        (config / 'tools/catalog-sources.json').write_text(json.dumps(sources))
        environment['VOSU_TOOL_CATALOG_TEST_SOCKET'] = f'127.0.0.1:{port}'
        for name in ['first', 'second']:
            (packages / name).mkdir()

        def package(archive):
            return {'url': f'{base}/{archive.name}', 'sha256': hashlib.sha256(archive.read_bytes()).hexdigest(), 'size': archive.stat().st_size}

        def tool(tool_id, version):
            archive = packages / f'{tool_id}-{version}.zip'
            fixture = ROOT / 'tests/fixtures/tool-host'
            with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED) as target:
                for path in fixture.rglob('*'):
                    if not path.is_file():
                        continue
                    data = path.read_bytes()
                    if path.name == 'tool.toml':
                        data = data.replace(b'version = "1.0.0"', f'version = "{version}"'.encode()).replace(b'id = "fixture"', f'id = "{tool_id}"'.encode())
                    target.writestr(path.relative_to(fixture).as_posix(), data)
            return {'id': tool_id, 'name': 'Catalog service fixture', 'version': version,
                    'description': 'A packaged fixture for isolated catalog acceptance checks.',
                    'author': '18-LAB', 'homepage': None, 'protocol': 1,
                    'interfaces': ['ui', 'service'], 'requires': [], 'packages': {'any': package(archive)}}

        def publish(version):
            fixture = tool('fixture', version)
            second = tool('fixture-two', '1.0.0')
            archive = packages / f'fixture-overlay-{version}.zip'
            with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED) as target:
                target.writestr('index.html', f'<!doctype html><h1>Overlay {version}</h1>')
                target.writestr('settings.json', '[]')
                target.writestr('metadata.txt', f'Name: Catalog overlay fixture\nAuthor: 18-LAB\nVersion: {version}\n')
            overlay = {'id': 'fixture-overlay', 'name': 'Catalog overlay fixture', 'version': version,
                       'description': 'An atomic overlay update fixture.', 'author': '18-LAB',
                       'homepage': None, 'resolution': [640, 120], 'preview': None,
                       'requires_tools': [{'id': 'fixture', 'version': '>=1.0.0'}], 'package': package(archive)}
            base_catalog = {'catalog_version': 1, 'published': '2026-10-21T00:00:00Z'}
            (packages / 'first/catalog.json').write_text(json.dumps({**base_catalog, 'tools': [fixture], 'overlays': [overlay]}))
            (packages / 'second/catalog.json').write_text(json.dumps({**base_catalog, 'tools': [fixture, second]}))

        publish('1.0.0')
        with (args.evidence / 'daemon.log').open('w') as log:
            daemon = subprocess.Popen([str(binary), '--update=false'], cwd=root, env=environment,
                                      stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
            try:
                wait_for(lambda: curl('/api/tools', dashboard=True)[0] == 200)
                status, page = curl('/tools', dashboard=True)
                assert status == 200
                context = json.loads(re.search(r'<script[^>]*id="vosu-context"[^>]*>(.*?)</script>', page, re.S).group(1))
                headers = {'Origin': DASHBOARD, 'X-Vosu-CSRF': context['csrf']}

                def action(path, body=None):
                    status, response = curl(path, dashboard=True, method='POST', headers=headers, data=body or {})
                    assert status == 200, (status, response)
                    job = json.loads(response)['job_id']
                    def completed():
                        status, response = curl(f'/api/tools/catalog/jobs/{job}', dashboard=True)
                        assert status == 200
                        result = json.loads(response)
                        assert result['status'] != 'failed', result
                        return result if result['status'] == 'completed' else None
                    return wait_for(completed, timeout=45)

                status, response = curl('/api/tools/catalog/sources', dashboard=True, method='POST', headers=headers, data={'sources': sources})
                assert status == 200, response
                status, response = curl('/api/tools/catalog', dashboard=True)
                catalog = json.loads(response)
                assert status == 200 and len(catalog['tools']) == 2 and len(catalog['overlays']) == 1
                assert len(catalog['shadowed']) == 1 and catalog['shadowed'][0]['source'] == sources[1]['source']
                assert catalog['tools'][0]['source'] == sources[0]['source']
                assert all(source['status'] == 'ok' for source in catalog['sources'])
                assert curl('/api/tools/fixture/install', dashboard=True, method='POST', data={})[0] == 403
                evidence.append('Two mixed sources merged in order; later duplicate shadowed; CSRF enforced; sources atomically saved')
                for changed in [list(reversed(sources)), [{**sources[0], 'enabled': False}, sources[1]], []]:
                    assert curl('/api/tools/catalog/sources', dashboard=True, method='POST', headers=headers, data={'sources': changed})[0] == 200
                    _, response = curl('/api/tools/catalog', dashboard=True)
                    merged = json.loads(response)
                    if changed:
                        assert merged['tools'][0]['source'] == sources[1]['source']
                    else:
                        assert not merged['tools'] and not merged['overlays']
                assert curl('/api/tools/catalog/sources', dashboard=True, method='POST', headers=headers, data={'sources': sources})[0] == 200
                assert json.loads((config / 'tools/catalog-sources.json').read_text()) == sources
                evidence.append('Source reorder, disable, remove-all, and restore persist and change winning entries')
                action('/api/tools/fixture/install')
                status, response = curl('/api/tools/fixture', dashboard=True)
                view = json.loads(response)
                assert status == 200 and view['source'] == 'user' and not view['trusted'] and not view['update_available']
                evidence.append('Non-official source tool installs as user/untrusted')
                assert curl('/api/tools/fixture/trust', dashboard=True, method='POST', headers=headers, data={})[0] == 200
                status, response = curl('/api/tools/fixture/start', dashboard=True, method='POST', headers=headers, data={})
                assert status == 200, response
                _, response = curl('/api/tools/fixture', dashboard=True)
                old_pid = json.loads(response)['process']['pid']
                data_dir = Path(view['data']['data_dir'])
                data_dir.mkdir(parents=True, exist_ok=True)
                (data_dir / 'retained').write_text('keep me')
                action('/api/tools/fixture-two/install')
                _, response = curl('/api/tools/fixture-two', dashboard=True)
                assert json.loads(response)['source'] == 'user' and not json.loads(response)['trusted']
                evidence.append('Unique tool from second source installed independently and remains untrusted')
                action('/api/tools/catalog/overlays/fixture-overlay/install', {'install_tools': True})
                settings = config / 'settings/fixture-overlay.settings.values.toml'
                settings.parent.mkdir(parents=True, exist_ok=True)
                settings.write_text('text = "preserved"\n')
                publish('2.0.0')
                status, response = curl('/api/tools/catalog', dashboard=True)
                catalog = json.loads(response)
                assert status == 200 and catalog['tools'][0]['update_available'] and catalog['overlays'][0]['update_available']
                if args.playwright_module:
                    subprocess.run(['node', str(ROOT / 'tests/tool_catalog_browser.mjs'), str(args.playwright_module.resolve()), str(args.evidence.resolve())], check=True, env=environment)
                action('/api/tools/fixture/update')
                _, response = curl('/api/tools/fixture', dashboard=True)
                updated = json.loads(response)
                assert updated['version'] == '2.0.0' and updated['status'] == 'running' and updated['process']['pid'] != old_pid
                evidence.append('Tool update 2.0.0: badge shown; approved running service restarted with new PID')
                action('/api/tools/catalog/overlays/fixture-overlay/update', {'install_tools': True})
                assert settings.read_text() == 'text = "preserved"\n'
                assert curl('/fixture-overlay/')[0] == 200 and '2.0.0' in curl('/fixture-overlay/')[1]
                evidence.append('Overlay install/update verified; index replaced atomically and canonical settings values preserved')
                action('/api/tools/fixture/uninstall', {'remove_data': False})
                assert (data_dir / 'retained').read_text() == 'keep me'
                evidence.append('Dashboard uninstall keep-data retained saved data')
                for arguments in [['--sources'], ['--install', 'fixture']]:
                    cli = subprocess.run([str(binary), 'x', *arguments], cwd=root, env=environment, text=True, capture_output=True, timeout=45)
                    assert cli.returncode == 0, cli.stdout + cli.stderr
                    evidence.append('vosux via vosu x: ' + cli.stdout.strip())
                publish('3.0.0')
                for arguments in [['--update', 'fixture'], ['--uninstall', 'fixture', '--remove-data']]:
                    cli = subprocess.run([str(binary), 'x', *arguments], cwd=root, env=environment, text=True, capture_output=True, timeout=45)
                    assert cli.returncode == 0, cli.stdout + cli.stderr
                    evidence.append('vosux via vosu x: ' + cli.stdout.strip())
                assert not data_dir.exists()
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
    (args.evidence / 'smoke.json').write_text(json.dumps(evidence, indent=2) + '\n')
    print('\n'.join(evidence))


if __name__ == '__main__':
    main()
