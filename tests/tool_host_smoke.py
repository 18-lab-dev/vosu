#!/usr/bin/env python3
"""Exercise the real daemon/tool HTTP boundary with isolated XDG roots and curl."""
import argparse
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/vosu')
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix='vosu-tool-smoke-') as temporary:
        root = Path(temporary)
        environment = dict(os.environ)
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
        with (root / 'daemon.log').open('w+') as log:
            daemon = subprocess.Popen([str(binary), '--tool', str(tool), '--update=false'],
                                      cwd=root, env=environment, stdin=subprocess.DEVNULL,
                                      stdout=log, stderr=subprocess.STDOUT)
            child_pid = None
            try:
                wait_for(lambda: curl('/api/tools', dashboard=True)[0] == 200)
                status, body = curl('/tools')
                assert status == 302
                print('curl GET /tools -> 302 (dashboard redirect)')
                status, body = curl('/tools/fixture/')
                assert status == 200 and '<h1>Tool fixture</h1>' in body
                print('curl GET /tools/fixture/ -> 200 (fixture HTML)')
                status, body = curl('/tools/fixture/api/v1/read?smoke=yes')
                first = json.loads(body)
                assert status == 200 and first['caller'] == 'overlay' and first['path'].endswith('?smoke=yes')
                print(f'curl GET /tools/fixture/api/v1/read?smoke=yes -> {status} {body}')
                status, body = curl('/tools/fixture/api/v1/write', method='POST', data={})
                assert status == 403
                print(f'curl POST /tools/fixture/api/v1/write (no session) -> {status}')
                status, page = curl('/tools', dashboard=True)
                assert status == 200 and 'tools-root' in page
                context = json.loads(re.search(r'<script[^>]*id="vosu-context"[^>]*>(.*?)</script>', page, re.S).group(1))
                status, launch = curl('/api/tools/fixture/launch', dashboard=True, method='POST',
                                      headers={'Origin': DASHBOARD, 'X-Vosu-CSRF': context['csrf']}, data={'theme': 'dark'})
                assert status == 200
                code = re.search(r'vosu-launch=([^&]+)', json.loads(launch)['url']).group(1)
                status, session = curl('/tools/_host/v1/session', method='POST', data={'tool': 'fixture', 'code': code})
                assert status == 200
                session = json.loads(session)['session']
                headers = {'X-Vosu-Tool-Session': session}
                status, body = curl('/tools/fixture/api/v1/write', method='POST', headers=headers, data={})
                assert status == 200 and json.loads(body)['caller'] == 'ui'
                print(f'curl POST launch + session exchange -> 200; authenticated write -> {status} {body}')
                status, body = curl('/api/tools', dashboard=True)
                view = json.loads(body)['tools'][0]
                assert status == 200 and view['status'] == 'running'
                child_pid = view['process']['pid']
                print(f'curl GET dashboard /api/tools -> {status} {body}')
                before = time.monotonic()
                status, _ = curl('/tools/fixture/api/v1/crash', method='POST', headers=headers, data={})
                assert status == 502
                def restarted():
                    _, body = curl('/api/tools/fixture', dashboard=True)
                    view = json.loads(body)
                    return view if view['status'] == 'running' and view['process']['pid'] != child_pid else None
                view = wait_for(restarted)
                child_pid = view['process']['pid']
                assert time.monotonic() - before >= 1
                assert view['process']['restarts'] == 1
                print(f'crash -> backoff >=1s -> running pid={child_pid}, restarts=1')
                status, body = curl('/api/tools/fixture/logs?after=0', dashboard=True)
                assert status == 200 and any('crashed' in line['text'] or 'exited unexpectedly' in line['text'] for line in json.loads(body)['lines'])
                print(f'curl GET dashboard /api/tools/fixture/logs -> {status} {body}')
                daemon.send_signal(signal.SIGTERM)
                assert daemon.wait(timeout=15) == 0
                assert child_pid is not None and not Path(f'/proc/{child_pid}').exists()
                marker = root / 'data/vosu/tools/data/fixture/stdin-closed'
                assert marker.read_text() == 'yes'
                print('vosu SIGTERM -> exit 0; child stdin EOF marker=yes; no orphaned child')
            except BaseException:
                log.flush()
                print((root / 'daemon.log').read_text())
                raise
            finally:
                if daemon.poll() is None:
                    daemon.terminate()
                    try:
                        daemon.wait(timeout=15)
                    except subprocess.TimeoutExpired:
                        daemon.kill()
                        daemon.wait()


if __name__ == '__main__':
    main()
