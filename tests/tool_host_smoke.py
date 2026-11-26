#!/usr/bin/env python3
"""Exercise the real daemon's web-only tool host with isolated XDG roots and curl.

A 2.0 tool is a static web package served by the bundled `vosu.toolhost`
(A7.2). The legacy `tests/fixtures/tool-host` `tool.toml` package is listed
`disabled` with `needs-2.0-update`, never parsed, run, proxied or served, and
its files are byte-identical afterwards. The required browser phase drives the
`fixture-v2` web tool through the dashboard launch, the single-use session
exchange and the API v1 bridge.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
OVERLAY = 'http://127.0.0.1:25150'
DASHBOARD = 'http://127.0.0.1:25151'
NEEDS_UPDATE = 'needs-2.0-update'

def channel_root(binary, normal_root):
    """Select fixtures from the actual executable version, not build mode."""
    version = subprocess.check_output([str(binary), '--version'], text=True, timeout=10).strip().split()[-1]
    return normal_root / 'prerelease' if '-' in version.split('+', 1)[0] else normal_root


def curl(path, *, dashboard=False, method='GET', headers=None, data=None):
    url = (DASHBOARD if dashboard else OVERLAY) + path
    command = ['curl', '--silent', '--show-error', '--max-time', '10',
               '--write-out', '\n%{http_code}', '--request', method]
    for key, value in (headers or {}).items():
        command += ['--header', f'{key}: {value}']
    if data is not None:
        command += ['--header', 'Content-Type: application/json', '--data-binary', json.dumps(data)]
    result = subprocess.run(command + [url], text=True, capture_output=True, check=True)
    body, status = result.stdout.rsplit('\n', 1)
    return int(status), body


def wait_for(callback, timeout=20):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            value = callback()
            if value:
                return value
        except (OSError, subprocess.CalledProcessError) as error:
            last = error
        time.sleep(0.05)
    raise AssertionError(f'timed out waiting for daemon state: {last}')


def receipts(directory):
    """SHA-256 of every file below `directory`, keyed by relative path."""
    return {path.relative_to(directory).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in sorted(directory.rglob('*')) if path.is_file()}


def tool_view(tool_id):
    status, body = curl(f'/api/tools/{tool_id}', dashboard=True)
    assert status == 200, (tool_id, status, body)
    return json.loads(body)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/vosu')
    parser.add_argument('--playwright-module', type=Path,
                        default=os.environ.get('VOSU_PLAYWRIGHT_MODULE'))
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    if args.playwright_module is None or not args.playwright_module.is_file():
        parser.error('required browser phase needs --playwright-module or VOSU_PLAYWRIGHT_MODULE')
    playwright = args.playwright_module.resolve(strict=True)
    subprocess.run(['taskset', '-c', '0-7,16-23', 'nice', '-n', '10', 'cargo', 'build',
                    '--locked', '-p', 'vosu-plugin-testkit', '--features', 'test-harness',
                    '--example', 'api_v1_plugin', '--target-dir', str(binary.parent.parent)],
                   cwd=ROOT, check=True, timeout=600)
    plugin_library = binary.parent / 'examples/libapi_v1_plugin.so'
    if not plugin_library.is_file():
        raise RuntimeError(f'debug fixture missing: {plugin_library}')
    with tempfile.TemporaryDirectory(prefix='vosu-tool-smoke-') as temporary:
        root = Path(temporary)
        environment = dict(os.environ)
        for variable in ['DISPLAY', 'WAYLAND_DISPLAY', 'DBUS_SESSION_BUS_ADDRESS']:
            environment.pop(variable, None)
        environment['PLAYWRIGHT_BROWSERS_PATH'] = os.environ.get(
            'PLAYWRIGHT_BROWSERS_PATH', str(Path.home() / '.cache/ms-playwright'))
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
POLL_RATE = 150
PRECISE_DATA_POLL_RATE = 10
SERVER_IP = "127.0.0.1"
SERVER_PORT = 25150
DASHBOARD_IP = "127.0.0.1"
DASHBOARD_PORT = 25151
ALLOWED_IPS = "127.0.0.1,localhost,absolute"
''')
        tool = root / 'fixture'
        shutil.copytree(ROOT / 'tests/fixtures/tool-host', tool)
        legacy_receipts = receipts(tool)
        v2_tool = root / 'fixture-v2'
        shutil.copytree(ROOT / 'tests/fixtures/tool-host-v2', v2_tool)
        plugin = root / 'plugin'
        shutil.copytree(ROOT / 'tests/fixtures/api-v1-plugin', plugin)
        (plugin / 'lib').mkdir()
        shutil.copy2(plugin_library, plugin / 'lib/libapi_v1_plugin.so')
        target_path = root / 'api_v1_target'
        shutil.copy2('/bin/sh', target_path)
        target = subprocess.Popen([str(target_path), '-c', 'read line'], stdin=subprocess.PIPE)
        with (root / 'daemon.log').open('w+') as log:
            daemon = subprocess.Popen([str(binary), '--tool', str(tool), '--tool', str(v2_tool),
                                       '--plugin', str(plugin), '--update=false'],
                                      cwd=root, env=environment, stdin=subprocess.DEVNULL,
                                      stdout=log, stderr=subprocess.STDOUT)
            try:
                wait_for(lambda: curl('/api/tools', dashboard=True)[0] == 200)
                status, body = curl('/tools')
                assert status == 302
                print('curl GET /tools -> 302 (dashboard redirect)')
                status, page = curl('/tools', dashboard=True)
                assert status == 200 and 'tools-root' in page
                context = json.loads(re.search(r'<script[^>]*id="vosu-context"[^>]*>(.*?)</script>', page, re.S).group(1))
                management = {'Origin': DASHBOARD, 'X-Vosu-CSRF': context['csrf']}

                # Legacy `tool.toml`: listed, diagnosed, never presented or run.
                view = tool_view('fixture')
                assert view['status'] == 'disabled' and view['status_detail'] == NEEDS_UPDATE, view
                assert view['enabled'] is False and view['ui_url'] is None, view
                print(f'curl GET dashboard /api/tools/fixture -> disabled {NEEDS_UPDATE}')
                for action in ['enable', 'trust', 'launch']:
                    status, body = curl(f'/api/tools/fixture/{action}', dashboard=True, method='POST',
                                        headers=management, data={})
                    assert status == 400 and json.loads(body)['error'] == NEEDS_UPDATE, (action, status, body)
                print(f'curl POST dashboard /api/tools/fixture/{{enable,trust,launch}} -> 400 {NEEDS_UPDATE}')
                for method, path in [('GET', '/tools/fixture/'), ('GET', '/tools/fixture/api/v1/read'),
                                     ('POST', '/tools/fixture/api/v1/write'),
                                     ('GET', '/tools/fixture/_settings')]:
                    status, _ = curl(path, method=method, data={} if method == 'POST' else None)
                    assert status == 404, (method, path, status)
                print('curl legacy UI, settings and /api/* proxy paths -> 404 (no service, no proxy)')
                for path in ['/tools/_host/v1/self', '/tools/_host/v1/settings',
                             '/tools/_host/v1/context', '/tools/_host/v1/overlays',
                             '/tools/_host/v1/bridge.js', '/tools/_host/v1/cli']:
                    status, _ = curl(path)
                    assert status == 404, (path, status)
                print('curl deleted /tools/_host/v1/{self,settings,context,overlays,bridge.js,cli} -> 404')

                # The v2 web tool: static UI, explicit api/* 404, 503 when not servable.
                wait_for(lambda: tool_view('fixture-v2')['status'] == 'running')
                status, body = curl('/tools/fixture-v2/')
                assert status == 200 and '<title>M3 browser fixture</title>' in body
                status, _ = curl('/tools/fixture-v2/api/x')
                assert status == 404
                status, bridge = curl('/tools/_host/v2/bridge.js')
                assert status == 200 and 'export' in bridge
                print('curl GET /tools/fixture-v2/ -> 200; /tools/fixture-v2/api/x -> 404; v2 bridge -> 200')
                status, _ = curl('/api/tools/fixture-v2/disable', dashboard=True, method='POST',
                                 headers=management, data={})
                assert status == 200
                wait_for(lambda: curl('/tools/fixture-v2/')[0] == 503)
                status, _ = curl('/api/tools/fixture-v2/enable', dashboard=True, method='POST',
                                 headers=management, data={})
                assert status == 200
                wait_for(lambda: curl('/tools/fixture-v2/')[0] == 200)
                print('disable fixture-v2 -> /tools/fixture-v2/ 503; enable -> 200')

                wait_for(lambda: any(s['plugin']['id'] == 'test.api.fixture'
                                     for s in json.loads(curl('/api/v1/sessions')[1])['sessions']))
                subprocess.run(['node', str(ROOT / 'tests/tool_host_v2_browser.mjs'), str(playwright)],
                               cwd=ROOT, env=environment, check=True, timeout=90)
                daemon.send_signal(signal.SIGTERM)
                assert daemon.wait(timeout=15) == 0
                tool_data = channel_root(binary, root / 'data/vosu') / 'tool-data/fixture'
                assert not tool_data.exists(), f'legacy service ran: {sorted(tool_data.rglob("*"))}'
                assert receipts(tool) == legacy_receipts, 'legacy package files changed'
                print('vosu SIGTERM -> exit 0; legacy service never ran; legacy files unchanged (SHA-256)')
            except BaseException:
                log.flush()
                print((root / 'daemon.log').read_text())
                raise
            finally:
                target.terminate()
                target.wait(timeout=10)
                if daemon.poll() is None:
                    daemon.terminate()
                    try:
                        daemon.wait(timeout=15)
                    except subprocess.TimeoutExpired:
                        daemon.kill()
                        daemon.wait(timeout=15)


if __name__ == '__main__':
    main()
