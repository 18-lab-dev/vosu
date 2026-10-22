import base64
import hashlib
import json
import os
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

TOKEN = os.environ['VOSU_TOOL_TOKEN']
DATA = Path(os.environ['VOSU_TOOL_DATA_DIR'])
DATA.mkdir(parents=True, exist_ok=True)
(DATA / 'pid').write_text(str(os.getpid()))
if '--crash-before-ready' in sys.argv:
    sys.exit(7)
if '--exit-before-ready' in sys.argv:
    sys.exit(0)

class Handler(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *_args):
        pass

    def do_GET(self):
        if self.headers.get('X-Vosu-Tool-Token') != TOKEN:
            self.send_error(401)
            return
        if self.path == '/v1/stream':
            key = self.headers['Sec-WebSocket-Key']
            accept = base64.b64encode(hashlib.sha1((key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
            self.send_response(101)
            self.send_header('Upgrade', 'websocket')
            self.send_header('Connection', 'Upgrade')
            self.send_header('Sec-WebSocket-Accept', accept)
            if self.headers.get('Sec-WebSocket-Protocol'):
                self.send_header('Sec-WebSocket-Protocol', self.headers['Sec-WebSocket-Protocol'].split(',')[0].strip())
            self.end_headers()
            frame = json.dumps({'caller': self.headers.get('X-Vosu-Caller')}).encode()
            self.wfile.write(bytes([0x81, len(frame)]) + frame)
            self.wfile.flush()
            self.wfile.write(b'\x88\x02\x03\xe8')
            self.wfile.flush()
            self.close_connection = True
            return
        body = json.dumps({'path': self.path, 'caller': self.headers.get('X-Vosu-Caller'), 'pid': os.getpid()}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        self.rfile.read(int(self.headers.get('Content-Length', '0')))
        if self.headers.get('X-Vosu-Tool-Token') != TOKEN:
            self.send_error(401)
            return
        if self.path == '/v1/crash':
            print('crashing on request', flush=True)
            os._exit(7)
        if self.path == '/v1/exit':
            os._exit(0)
        self.do_GET()

server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
server.daemon_threads = True
print(f'vosu-tool-ready {server.server_port}', flush=True)
print('fixture started', flush=True)

def stdin_closed():
    sys.stdin.buffer.read()
    (DATA / 'stdin-closed').write_text('yes')
    server.shutdown()

threading.Thread(target=stdin_closed, daemon=True).start()
server.serve_forever()
print('fixture exiting', flush=True)
server.server_close()
