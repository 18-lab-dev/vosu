"""vosu protocol v1 reference service; requires only Python 3.11+."""
import hmac
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import sys
import threading
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen


def main():
    if os.environ['VOSU_TOOL_PROTOCOL'] != '1':
        raise SystemExit('unsupported tool protocol')
    token = os.environ['VOSU_TOOL_TOKEN']
    host_url = os.environ['VOSU_URL'].rstrip('/')
    count = 0
    lock = threading.Lock()

    def host(path):
        request = Request(host_url + path,
                          headers={'X-Vosu-Tool-Token': token})
        with urlopen(request, timeout=2) as response:
            return json.load(response)

    if os.environ.get('VOSU_TOOL_INTERFACE') == 'cli':
        if sys.argv[1:] not in ([], ['--json'], ['--help']):
            raise SystemExit('usage: vosu x hello-python [--json|--help]')
        if sys.argv[1:] == ['--help']:
            print('usage: vosu x hello-python [--json|--help]')
            return
        value = host('/tools/hello-python/api/v1/count')
        print(json.dumps(value) if sys.argv[1:] else f"Count: {value['count']}")
        return

    class Handler(BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.0'

        def log_message(self, *_args):
            # Never log headers, launch tokens, or browser sessions.
            pass

        def reply(self, status, value):
            body = json.dumps(value).encode()
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            if self.command != 'HEAD':
                self.wfile.write(body)

        def dispatch(self):
            nonlocal count
            supplied = self.headers.get('X-Vosu-Tool-Token', '')
            if not hmac.compare_digest(supplied.encode(), token.encode()):
                return self.reply(401, {'error': 'unauthorized'})
            path = self.path.split('?', 1)[0]
            if path == '/healthz' and self.command in ('GET', 'HEAD'):
                return self.reply(200, {'ok': True})
            if path == '/v1/count' and self.command in ('GET', 'HEAD'):
                try:
                    current_settings = host('/tools/_host/v1/settings')
                except (HTTPError, URLError, TimeoutError, ValueError):
                    return self.reply(502, {'error': 'host settings unavailable'})
                with lock:
                    value = count
                return self.reply(200, {'count': value, 'settings': current_settings})
            if path == '/v1/context' and self.command in ('GET', 'HEAD'):
                try:
                    context = host('/tools/_host/v1/context')
                except (HTTPError, URLError, TimeoutError, ValueError):
                    return self.reply(502, {'error': 'host context unavailable'})
                # Host context includes absolute paths; expose only the file name.
                beatmap = context.get('beatmap')
                return self.reply(200, {'file': Path(beatmap['file']).name if beatmap else None})
            if path == '/v1/ratings' and self.command in ('GET', 'HEAD'):
                try:
                    value = host('/tools/mania-ratings/api/v1/beatmap/current')
                except HTTPError as error:
                    if error.code in (404, 503):
                        return self.reply(200, {'available': False, 'status': error.code})
                    return self.reply(502, {'error': f'ratings returned HTTP {error.code}'})
                except (URLError, TimeoutError, ValueError):
                    return self.reply(502, {'error': 'ratings unavailable'})
                return self.reply(200, {'available': True, 'rating': value})
            if path == '/v1/increment' and self.command == 'POST':
                # No caller-supplied body is needed; each request closes its connection.
                with lock:
                    count += 1
                    value = count
                print(f'Counter incremented to {value}', flush=True)
                return self.reply(200, {'count': value})
            return self.reply(404, {'error': 'not found'})

        do_GET = dispatch
        do_HEAD = dispatch
        do_POST = dispatch

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    server.daemon_threads = True
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    print(f'vosu-tool-ready {server.server_port}', flush=True)
    try:
        while sys.stdin.buffer.read(4096):
            pass
    finally:
        server.shutdown()
        server.server_close()
        worker.join()


if __name__ == '__main__':
    main()
