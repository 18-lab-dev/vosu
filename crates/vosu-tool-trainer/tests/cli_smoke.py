#!/usr/bin/env python3
"""Exercise all first-party CLIs against real services and a stdlib proxy.
No desktop, real daemon, or user Songs access. Run after cargo build.
"""
import base64
import csv
import io
import hashlib
import http.client
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
import wave

ROOT = Path(__file__).resolve().parents[3]
IDS = ('difficulty', 'trainer', 'skill-tracker')
TOKEN = 'cli-smoke-token'


def main():
    with tempfile.TemporaryDirectory(prefix='vosu-cli-smoke-') as directory:
        tmp = Path(directory)
        songs = tmp / 'Songs'
        folder = songs / 'fixture'
        folder.mkdir(parents=True)
        chart = folder / 'fixture.osu'
        source = ROOT / 'crates/vosu-tool-difficulty/tests/fixtures/cyclic.osu'
        text = source.read_text()
        # Supply one small local audio fixture, never copy user media.
        text = text.replace('[General]', '[General]\nAudioFilename: audio.wav', 1)
        chart.write_text(text)
        with wave.open(str(folder / 'audio.wav'), 'wb') as wav:
            wav.setparams((1, 2, 8000, 0, 'NONE', 'not compressed'))
            wav.writeframes(b'\0\0' * 16000)
        context = {'client': 'stable', 'songs_dir': str(songs), 'beatmap': {
            'checksum': hashlib.md5(chart.read_bytes()).hexdigest(), 'mode': 'mania',
            'folder': str(folder), 'file': str(chart), 'audio': str(folder / 'audio.wav'),
            'background': None}}
        ports = {}
        context_read = threading.Event()
        stop = threading.Event()
        class Proxy(http.server.BaseHTTPRequestHandler):
            protocol_version = 'HTTP/1.1'
            def request(self):
                if self.path == '/websocket/v2/subscribe':
                    key = self.headers['Sec-WebSocket-Key']
                    accept = base64.b64encode(hashlib.sha1((key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
                    self.send_response(101)
                    self.send_header('Upgrade', 'websocket')
                    self.send_header('Connection', 'Upgrade')
                    self.send_header('Sec-WebSocket-Accept', accept)
                    self.end_headers()
                    def frame(value):
                        data = json.dumps(value).encode()
                        length = bytes([len(data)]) if len(data) < 126 else b'\x7e' + len(data).to_bytes(2, 'big')
                        self.wfile.write(b'\x81' + length + data)
                        self.wfile.flush()
                    context_read.clear()
                    values = [{'number': 2, 'name': 'play'}, 'stable',
                              {'checksum': context['beatmap']['checksum'], 'title': 'CLI, "fixture"', 'version': '4K', 'mode': {'name': 'mania'}, 'time': {'live': 1000}, 'stats': {'cs': {'converted': 4}}},
                              {'mode': {'name': 'mania'}, 'score': 1000000, 'accuracy': 100.0, 'failed': False, 'hits': {'geki': 10}, 'hitOffsets': [0] * 10, 'mods': {'number': 0, 'rate': 1.0}, 'combo': {'max': 10}},
                              {}, False, 1, {}]
                    frame([0, 1, list(enumerate(values))])
                    assert context_read.wait(10), 'play start context never read'
                    frame([1, 2, [[0, {'number': 7, 'name': 'resultScreen'}], [4, {'hits': {'geki': 10}, 'score': 1000000, 'accuracy': 100.0, 'maxCombo': 10}]], []])
                    stop.wait(60)
                    return
                if self.headers.get('X-Vosu-Tool-Token') != TOKEN:
                    self.send_error(401)
                    return
                if self.path == '/tools/_host/v1/context':
                    body = json.dumps(context).encode()
                    context_read.set()
                    self.send_response(200)
                elif self.path.startswith('/tools/') and '/api/' in self.path:
                    _, _, tool, _, path = self.path.split('/', 4)
                    connection = http.client.HTTPConnection('127.0.0.1', ports[tool], timeout=60)
                    data = self.rfile.read(int(self.headers.get('Content-Length', 0)))
                    headers = {'X-Vosu-Tool-Token': TOKEN, 'X-Vosu-Caller': 'cli:' + tool}
                    if self.headers.get('Content-Type'):
                        headers['Content-Type'] = self.headers['Content-Type']
                    connection.request(self.command, '/' + path, body=data, headers=headers)
                    response = connection.getresponse()
                    body = response.read()
                    self.send_response(response.status)
                    connection.close()
                else:
                    self.send_error(404)
                    return
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            do_GET = do_POST = do_DELETE = request
            def log_message(self, *_):
                pass
        host = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Proxy)
        threading.Thread(target=host.serve_forever, daemon=True).start()
        processes = []
        environments = {}
        try:
            for tool in IDS:
                env = os.environ.copy()
                for key in ('HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME', 'XDG_STATE_HOME', 'XDG_RUNTIME_DIR'):
                    env[key] = str(tmp)
                env.update(VOSU_TOOL_PROTOCOL='1', VOSU_TOOL_ID=tool,
                           VOSU_TOOL_TOKEN=TOKEN, VOSU_URL=f'http://127.0.0.1:{host.server_port}',
                           VOSU_TOOL_DATA_DIR=str(tmp / tool / 'data'),
                           VOSU_TOOL_CACHE_DIR=str(tmp / tool / 'cache'),
                           VOSU_TOOL_INTERFACE='service', BROWSER='/bin/false')
                process = subprocess.Popen([str(ROOT / 'target/debug' / ('vosu-' + tool))],
                                           stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                           stderr=subprocess.PIPE, env=env, text=True)
                processes.append(process)
                handshake = process.stdout.readline().strip()
                assert handshake.startswith('vosu-tool-ready '), handshake
                ports[tool] = int(handshake.split()[1])
                environments[tool] = dict(env, VOSU_TOOL_INTERFACE='cli')
            def cli(tool, *args, stdin=None):
                result = subprocess.run([str(ROOT / 'target/debug' / ('vosu-' + tool)), 'cli', *args],
                                        input=stdin, capture_output=True, text=True,
                                        env=environments[tool], timeout=60)
                assert result.returncode == 0, (tool, args, result.returncode, result.stderr)
                print(tool, ' '.join(args), '=>', result.stdout[:180].strip())
                return result.stdout
            assert 'msd.overall:' in cli('difficulty', str(chart))
            assert json.loads(cli('difficulty', '-', '--json', stdin=text))['msd']
            assert len(json.loads(cli('difficulty', '--rates', '--json'))['rates']) == 14
            assert json.loads(cli('difficulty', str(chart), '--rate', '0.85', '--score', '0.93', '--json'))['ssr']
            path = Path(cli('trainer', '--rate', '0.85').strip())
            assert path.is_relative_to(songs) and path.exists() and path.suffix == '.osu'
            entries = json.loads(cli('trainer', 'list', '--json'))['maps']
            assert entries
            cli('trainer', 'delete', entries[0]['id'], '--json')
            assert not path.exists()
            assert not json.loads(cli('trainer', 'list', '--json'))['maps']
            assert 'rating_model:' in cli('skill-tracker', 'profile')
            assert 'ratings' in json.loads(cli('skill-tracker', 'profile', '--json'))
            deadline = time.monotonic() + 10
            while True:
                history = json.loads(cli('skill-tracker', 'history', '--last', '5', '--json'))['plays']
                if history:
                    break
                assert time.monotonic() < deadline, 'live fixture never persisted'
                time.sleep(0.05)
            latest = json.loads(cli('skill-tracker', 'last', '--json'))
            assert latest['difficulty']['msd'] and latest['difficulty']['ssr']
            assert latest['wife']['model'] == 'wife3-j4-offsets'
            assert json.loads(cli('skill-tracker', 'export', '--format', 'json')) == history
            rows = list(csv.DictReader(io.StringIO(cli('skill-tracker', 'export', '--format', 'csv'))))
            assert rows[0]['title'] == latest['title']
            assert json.loads(rows[0]['difficulty']) == latest['difficulty']
            assert chart.read_text() == text
            print('PASS: real service proxy CLI smoke; originals unchanged; isolated Songs only')
        finally:
            stop.set()
            for process in processes:
                process.stdin.close()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            host.shutdown()


if __name__ == '__main__':
    main()
