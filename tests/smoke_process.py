"""Native fixtures and isolated daemon lifecycle for the four M7 smokes."""
import hashlib
import http.client
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import time
from contextlib import contextmanager
import functools
from http.server import ThreadingHTTPServer
from threading import Thread

ROOT = Path(__file__).resolve().parents[1]
OVERLAY_PORT, DASHBOARD_PORT = 25150, 25151
OVERLAY = f'http://127.0.0.1:{OVERLAY_PORT}'
DASHBOARD = f'http://127.0.0.1:{DASHBOARD_PORT}'
BINARY = ROOT / 'target/debug' / ('vosu.exe' if os.name == 'nt' else 'vosu')
NODE = os.environ.get('VOSU_TEST_NODE', 'node')


def channel_root(binary, normal_root):
    version = subprocess.check_output([str(binary), '--version'], text=True, timeout=30).strip().split()[-1]
    return normal_root / 'prerelease' if '-' in version.split('+', 1)[0] else normal_root


def curl(path, *, dashboard=False, method='GET', headers=None, data=None):
    """Direct loopback HTTP with no environment proxy or redirect following."""
    connection = http.client.HTTPConnection('127.0.0.1', DASHBOARD_PORT if dashboard else OVERLAY_PORT, timeout=10)
    try:
        headers = dict(headers or {})
        body = None
        if data is not None:
            headers['Content-Type'] = 'application/json'
            body = json.dumps(data).encode()
        connection.request(method, path, body=body, headers=headers)
        response = connection.getresponse()
        return response.status, response.read().decode('utf-8')
    finally:
        connection.close()


def wait_for(callback, timeout=30):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            value = callback()
            if value:
                return value
        except (OSError, http.client.HTTPException) as error:
            last = error
        time.sleep(0.05)
    raise AssertionError(f'timed out waiting for daemon state: {last}')


def check_ports():
    for port in (OVERLAY_PORT, DASHBOARD_PORT):
        with socket.socket() as sock:
            if sock.connect_ex(('127.0.0.1', port)) == 0:
                raise RuntimeError(f'127.0.0.1:{port} is already in use; run the smokes serially')


def artifact(variable):
    path = Path(os.environ.get(variable, ''))
    if not path.is_absolute() or not path.is_file():
        raise RuntimeError(f'{variable} missing: run scripts/plugin-host-fixtures.py before the smoke')
    with path.open('rb') as stream:
        digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    print(f'ARTIFACT {variable} sha256={digest}', flush=True)
    return path


def link_or_copy(src, dest):
    try:
        os.link(src, dest)
    except OSError:
        shutil.copy2(src, dest)


@contextmanager
def target(root, name='api_v1_target'):
    executable = root / (name + ('.exe' if os.name == 'nt' else ''))
    link_or_copy(artifact('VOSU_M7_TARGET'), executable)
    with subprocess.Popen([str(executable)], cwd=root, stdin=subprocess.DEVNULL) as process:
        try:
            yield process.pid, str(executable)
        finally:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=10)


def api_plugin(root):
    plugin = root / 'api-plugin'
    shutil.copytree(ROOT / 'tests/fixtures/api-v1-plugin', plugin)
    library = artifact('VOSU_M7_API_LIBRARY')
    (plugin / 'lib').mkdir()
    shutil.copy2(library, plugin / 'lib' / library.name)
    return plugin


@contextmanager
def package_server(handler, packages):
    with ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(handler, directory=str(packages))) as server:
        thread = Thread(target=server.serve_forever)
        thread.start()
        try:
            yield server.server_address[1]
        finally:
            server.shutdown()
            thread.join(timeout=10)
            assert not thread.is_alive()


class Daemon:
    def __init__(self, binary, root, *, allowed='127.0.0.1,localhost', static=None):
        self.root = root
        self.environment = dict(os.environ)
        for key in ('DISPLAY', 'WAYLAND_DISPLAY', 'DBUS_SESSION_BUS_ADDRESS', 'VOSU_DASHBOARD',
                    'VOSU_TEST_OFFICIAL_PLUGIN', 'VOSU_CATALOG_TEST_SOCKET', 'VOSU_TEST_ADAPTER_CAPTURE'):
            self.environment.pop(key, None)
        browser_cache = Path(os.environ['LOCALAPPDATA']) / 'ms-playwright' if os.name == 'nt' else Path.home() / '.cache/ms-playwright'
        self.environment.setdefault('PLAYWRIGHT_BROWSERS_PATH', str(browser_cache))
        for variable, directory in [('HOME', 'home'), ('XDG_CONFIG_HOME', 'config'),
                                    ('XDG_DATA_HOME', 'data'), ('XDG_CACHE_HOME', 'cache'),
                                    ('XDG_STATE_HOME', 'state'), ('XDG_RUNTIME_DIR', 'runtime'),
                                    ('LOCALAPPDATA', 'local'), ('TEMP', 'temp'), ('TMP', 'temp'), ('OneDrive', 'onedrive')]:
            path = root / directory
            path.mkdir(mode=0o700, exist_ok=True)
            self.environment[variable] = str(path)
        program = root / 'bin'
        program.mkdir()
        self.binary = program / binary.name
        link_or_copy(binary, self.binary)
        self.config = channel_root(self.binary, program if os.name == 'nt' else root / 'config/vosu')
        self.data = self.config if os.name == 'nt' else channel_root(self.binary, root / 'data/vosu')
        self.config.mkdir(parents=True, exist_ok=True)
        (self.config / 'vosu.toml').write_text(f'''ENABLE_AUTOUPDATE = false
OPEN_DASHBOARD_ON_STARTUP = false
START_AT_LOGIN = false
DEBUG_LOG = false
SERVER_IP = "127.0.0.1"
SERVER_PORT = {OVERLAY_PORT}
DASHBOARD_IP = "127.0.0.1"
DASHBOARD_PORT = {DASHBOARD_PORT}
ALLOWED_IPS = {json.dumps(allowed)}
''', encoding='utf-8')
        plugins_dir = self.config / 'plugins'
        plugins_dir.mkdir(parents=True, exist_ok=True)
        (plugins_dir / 'settings.json').write_text(json.dumps({
            'version': 1,
            'plugins': {
                'vosu.overlayhost': {
                    'static_folder_path': str(static or root / 'static'),
                },
            },
        }), encoding='utf-8')
        self.stop = root / 'stop'
        self.environment['VOSU_TEST_STOP_FILE'] = str(self.stop)
        self.environment['VOSU_TEST_ISOLATED_DISCOVERY'] = '1'

    def discover(self, *targets):
        self.environment['VOSU_TEST_ISOLATED_DISCOVERY'] = json.dumps(targets)

    @contextmanager
    def run(self, log, *args):
        check_ports()
        with log.open('wb') as output, subprocess.Popen(
                [str(self.binary), '--update=false', *args], cwd=self.root, env=self.environment,
                stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.STDOUT) as process:
            try:
                def ready():
                    if process.poll() is not None:
                        raise AssertionError(f'daemon exited during startup: {process.returncode}')
                    return curl('/api/tools', dashboard=True)[0] == 200
                wait_for(ready)
                yield
                self.stop.touch()
                assert process.wait(timeout=30) == 0, 'daemon did not shut down cleanly'
            except BaseException:
                print(log.read_text(errors='replace'), flush=True)
                raise
            finally:
                if process.poll() is None:
                    self.stop.touch()
                    try:
                        process.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=10)

    def browser(self, script, module, *args, timeout=120):
        result = subprocess.run(
            [NODE, str(ROOT / 'tests' / script), str(module.resolve(strict=True)), *map(str, args)],
            cwd=ROOT, env=self.environment, stdout=subprocess.PIPE,
            encoding='utf-8', errors='replace', timeout=timeout, check=False,
        )
        print(result.stdout, end='', flush=True)
        result.check_returncode()
        scenarios = [line.split(' ', 3) for line in result.stdout.splitlines() if line.startswith('M7_BROWSER ')]
        assert (len(scenarios) == 1 and scenarios[0][1] == script
                and int(scenarios[0][2]) > 0), f'{script}: no named browser receipt'
