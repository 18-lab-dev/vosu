"""Dependency-free staging, manifest schema, and Python protocol integration tests."""
import importlib.util
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import queue
import re
import subprocess
import sys
import tempfile
import threading
import tomllib
import unittest
from urllib.error import HTTPError
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('stage_tools', Path(__file__).with_name('stage-tools.py'))
stage = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(stage)
SCHEMA = json.loads((ROOT / 'docs/tools/tool.schema.json').read_text())


def validate(value, schema):
    """Evaluate the JSON Schema keywords used by tool.schema.json, without dependencies."""
    if '$ref' in schema:
        target = SCHEMA
        for part in schema['$ref'].removeprefix('#/').split('/'):
            target = target[part]
        validate(value, target)
    for child in schema.get('allOf', []):
        validate(value, child)
    types = {'object': dict, 'array': list, 'string': str, 'integer': int}
    if 'type' in schema:
        assert type(value) is types[schema['type']], f'wrong type: {value!r}'
    if 'const' in schema:
        assert value == schema['const'], value
    if 'enum' in schema:
        assert value in schema['enum'], value
    if isinstance(value, str):
        assert len(value) >= schema.get('minLength', 0), value
        if 'pattern' in schema:
            assert re.search(schema['pattern'], value), value
    if isinstance(value, dict):
        assert set(schema.get('required', [])) <= value.keys(), value
        properties = schema.get('properties', {})
        if schema.get('additionalProperties') is False:
            assert value.keys() <= properties.keys(), value
        for key, item in value.items():
            if key in properties:
                validate(item, properties[key])
    if isinstance(value, list):
        assert len(value) >= schema.get('minItems', 0), value
        prefixes = schema.get('prefixItems', [])
        for index, item in enumerate(value):
            validate(item, prefixes[index] if index < len(prefixes) else schema.get('items', {}))


class ManifestTests(unittest.TestCase):
    def test_all_example_manifests(self):
        protocol = (ROOT / 'docs/tools/protocol.md').read_text()
        blocks = re.findall(r'```toml\n(.*?)```', protocol, re.S)
        self.assertTrue(blocks)
        examples = sorted((ROOT / 'examples').rglob('tool.toml'))
        self.assertTrue(examples)
        for label, text in [(f'protocol block {i}', text) for i, text in enumerate(blocks)] + [(str(p), p.read_text()) for p in examples]:
            with self.subTest(manifest=label):
                validate(tomllib.loads(text), SCHEMA)

    def test_invalid_fields_are_rejected(self):
        valid = {'manifest_version': 1, 'id': 'hello', 'name': 'Hello', 'version': '0.1.0'}
        for change in ({'id': '_host'}, {'version': '1'}, {'typo': True}, {'service': {'command': []}}, {'ui': {'root': '../escape'}}, {'permissions': {'host': ['admin']}}):
            with self.subTest(change=change), self.assertRaises(AssertionError):
                validate(valid | change, SCHEMA)


class StagingTests(unittest.TestCase):
    def test_temporary_workspace_and_replacement(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            crate = root / 'crates/vosu-tool-fixture'
            (crate / 'web').mkdir(parents=True)
            (crate / 'tool.toml').write_text('id="fixture"\n[service]\ncommand=["bin/vosu-fixture"]\n')
            (crate / 'settings.json').write_text('{}')
            (crate / 'web/index.html').write_text('fixture')
            for triple, suffix in [('x86_64-unknown-linux-gnu', ''), ('x86_64-pc-windows-msvc', '.exe')]:
                binary = root / 'target' / triple / 'release' / ('vosu-fixture' + suffix)
                binary.parent.mkdir(parents=True)
                binary.write_bytes(b'explicit non-executable test fixture')
                tools = stage.stage_tools(root, root / 'target', 'release', triple, root / 'out')
                names = sorted(p.relative_to(root / 'out').as_posix() for p in tools.rglob('*') if p.is_file())
                self.assertEqual(names, [f'tools/fixture/bin/vosu-fixture{suffix}', 'tools/fixture/settings.json', 'tools/fixture/tool.toml', 'tools/fixture/web/index.html'])
                print('Staged fixture tree (' + triple + '):\n  ' + '\n  '.join(names))
                (tools / 'stale').write_text('obsolete')
                stage.stage_tools(root, root / 'target', 'release', triple, root / 'out')
                self.assertFalse((tools / 'stale').exists())
                binary.unlink()
                with self.assertRaisesRegex(ValueError, 'missing regular'):
                    stage.stage_tools(root, root / 'target', 'release', triple, root / 'out')
                self.assertTrue((tools / 'fixture/tool.toml').is_file())
            for manifest in ('id="wrong"\n', 'id="fixture"\n[service]\ncommand=["elsewhere"]\n'):
                (crate / 'tool.toml').write_text(manifest)
                with self.assertRaises(ValueError):
                    stage.stage_tools(root, root / 'target', 'debug', None, root / 'out')


class PythonServiceTests(unittest.TestCase):
    def test_handshake_auth_settings_write_and_stdin_eof(self):
        token = 'a' * 64
        calls = []

        class Host(BaseHTTPRequestHandler):
            def do_GET(self):
                calls.append((self.path, self.headers.get('X-Vosu-Tool-Token')))
                body = b'{"greeting":"test"}'
                self.send_response(200)
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *_args):
                pass

        host = ThreadingHTTPServer(('127.0.0.1', 0), Host)
        thread = threading.Thread(target=host.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(host.server_close)
        self.addCleanup(host.shutdown)
        with tempfile.TemporaryDirectory() as temporary:
            environment = dict(os.environ, VOSU_TOOL_PROTOCOL='1', VOSU_TOOL_ID='hello-python', VOSU_TOOL_TOKEN=token,
                               VOSU_URL=f'http://127.0.0.1:{host.server_port}', VOSU_TOOL_DATA_DIR=temporary, VOSU_TOOL_CACHE_DIR=temporary)
            process = subprocess.Popen([sys.executable, str(ROOT / 'examples/tools/hello-python/main.py')],
                                       env=environment, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                lines = queue.Queue()
                threading.Thread(target=lambda: lines.put(process.stdout.readline()), daemon=True).start()
                line = lines.get(timeout=5).decode().strip()
                self.assertRegex(line, r'^vosu-tool-ready [0-9]+$')
                base = 'http://127.0.0.1:' + line.split()[1]
                for supplied in (None, 'wrong'):
                    headers = {} if supplied is None else {'X-Vosu-Tool-Token': supplied}
                    with self.assertRaises(HTTPError) as error:
                        urlopen(Request(base + '/healthz', headers=headers), timeout=3)
                    self.assertEqual(error.exception.code, 401)
                    error.exception.close()
                headers = {'X-Vosu-Tool-Token': token}
                for path, method, expected in [('/healthz', 'GET', {'ok': True}), ('/v1/increment', 'POST', {'count': 1}),
                                               ('/v1/count', 'GET', {'count': 1, 'settings': {'greeting': 'test'}})]:
                    with urlopen(Request(base + path, headers=headers, method=method), timeout=3) as response:
                        self.assertEqual(response.status, 200)
                        self.assertEqual(json.load(response), expected)
                self.assertEqual(calls, [('/tools/_host/v1/settings', token)])
                process.communicate(timeout=5)
                self.assertEqual(process.returncode, 0)
                print(f'Python example: {line}; missing/wrong token=401; authenticated health/read/write=200; host settings called; stdin EOF exit=0')
            finally:
                if process.poll() is None:
                    process.kill()
                    process.communicate()


if __name__ == '__main__':
    unittest.main()
