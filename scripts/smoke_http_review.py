"""Executable regressions for the five focused M3 final-review corrections."""

import concurrent.futures
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import sys
import threading
import time
from urllib.parse import urlsplit

from smoke_fastapi import available_port, cleanup, request, require, stop_proxy, wait_ready

ROOT = Path(__file__).resolve().parents[1]
RECORD = re.compile(r'^\[http-review\] (\S+) (\S+) -> (\d+) \([\d.]+ms\) \[(\w+)\] timestamp=\d+\.\d{9}$')


class Backend(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def log_message(self, *_):
        pass

    def do_HEAD(self):
        self.send_response(200)
        self.send_header('Content-Length', '42')
        self.end_headers()

    def do_GET(self):
        path = urlsplit(self.path).path
        if path == '/101':
            self.send_response(101)
            self.send_header('Connection', 'upgrade')
            self.send_header('Upgrade', 'websocket')
            self.end_headers()
            self.close_connection = True
            return
        if path.startswith('/bad-upstream/'):
            invalid = ['x-private,\xff', '"x-private"', 'x private'][int(path.rsplit('/', 1)[1])]
            self.send_response(200)
            self.send_header('Connection', invalid)
            self.send_header('X-Private', 'must-not-forward')
            self.send_header('Content-Length', '2')
            self.end_headers()
            self.wfile.write(b'ok')
            self.close_connection = True
            return
        if path in ('/204', '/205', '/304'):
            status = int(path[1:])
            self.send_response(status)
            if status == 205:
                self.send_header('Content-Length', '0')
            if status == 304:
                self.send_header('Content-Length', '42')
                self.send_header('ETag', '"revision"')
            self.end_headers()
            return
        body = json.dumps({
            'host': self.headers.get('Host'),
            'via': self.headers.get_all('Via') or [],
            'private': self.headers.get('X-Private'),
            'other': self.headers.get('X-Other'),
            'public': self.headers.get('X-Public'),
            'backend_peer_port': self.client_address[1],
        }).encode()
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Via', '1.0 origin-gateway')
        self.send_header('Connection', 'x-response-hop')
        self.send_header('X-Response-Hop', 'must-not-forward')
        self.send_header('Set-Cookie', 'one=1')
        self.send_header('Set-Cookie', 'two=2')
        self.end_headers()
        self.wfile.write(body)


class QuietServer(ThreadingHTTPServer):
    def handle_error(self, *_):
        # A raw malformed-input/closed-client test may reset its own connection.
        pass


def windows_options():
    if os.name != 'nt':
        return {}
    startup = subprocess.STARTUPINFO()
    startup.dwFlags |= subprocess.STARTF_USESHOWWINDOW
    startup.wShowWindow = 0
    return {'creationflags': subprocess.CREATE_NEW_CONSOLE, 'startupinfo': startup}


def start_proxy(binary, backend_port, stdout, stderr, name='http-review'):
    port = available_port()
    process = subprocess.Popen(
        [str(binary), 'attach', '--listen', f'127.0.0.1:{port}', '--target', f'127.0.0.1:{backend_port}', '--name', name],
        cwd=ROOT, stdout=stdout, stderr=stderr, **windows_options(),
    )
    return process, port


def protocol_checks(binary, backend_port, output):
    process = None
    expected = {}
    try:
        with (output / 'protocol-observations.log').open('wb') as observations, (output / 'protocol-stderr.log').open('wb') as diagnostics:
            process, port = start_proxy(binary, backend_port, observations, diagnostics)
            wait_ready(process, port)

            def check(path, status, outcome='Success', method='GET'):
                actual, headers, body = request(port, method, path)
                require(actual == status, f'{path}: expected HTTP {status}, got {actual}')
                expected[path] = (method, status, outcome)
                return headers, body

            for i in range(3):
                check(f'/bad-upstream/{i}', 502, 'BackendFailure')
            check('/101', 502, 'BackendFailure')
            for i, invalid in enumerate([b'x-private,\xff', b'x-private bad', b'"x-private"', b'x-private:bad']):
                path = f'/bad-client/{i}'
                with socket.create_connection(('127.0.0.1', port), timeout=3) as client:
                    client.sendall(f'GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nConnection: '.encode() + invalid + b'\r\nX-Private: must-not-forward\r\n\r\n')
                    response = http.client.HTTPResponse(client)
                    response.begin()
                    require(response.status == 400, f'Invalid client header was accepted: {i}')
                    response.read()
                expected[path] = ('GET', 400, 'Rejected')
            connection = http.client.HTTPConnection('127.0.0.1', port, timeout=3)
            try:
                connection.putrequest('GET', '/via', skip_host=True)
                for name, value in [
                    ('Host', 'customer.example'), ('Via', '1.0 entry, 1.1 edge'),
                    ('Via', '1.1 cache'), ('Connection', ', \tX-Private,, keep-alive ,'),
                    ('Connection', 'X-Other'), ('X-Private', 'hidden'),
                    ('X-Other', 'hidden'), ('X-Public', 'keep'),
                ]:
                    connection.putheader(name, value)
                connection.endheaders()
                response = connection.getresponse()
                headers = response.getheaders()
                data = json.loads(response.read())
                require(response.status == 200, 'Valid multi-value headers rejected')
                require(data['host'] == 'customer.example', 'Host changed')
                require(data['via'] == ['1.0 entry, 1.1 edge', '1.1 cache', '1.1 exaprop'], 'Request Via chain changed')
                require(data['private'] is None and data['other'] is None and data['public'] == 'keep', 'Request hop filtering failed')
                require([value for name, value in headers if name.lower() == 'via'] == ['1.0 origin-gateway', '1.1 exaprop'], 'Response Via chain changed')
                require(sum(name.lower() == 'set-cookie' for name, _ in headers) == 2, 'Duplicate response headers changed')
                require(not any(name.lower() == 'x-response-hop' for name, _ in headers), 'Response hop header leaked')
                expected['/via'] = ('GET', 200, 'Success')
                # Two transactions over the same frontend connection, also checking
                # upstream pool reuse against the real backend's peer port.
                ports = []
                for path in ('/persistent-one', '/persistent-two'):
                    connection.request('GET', path)
                    response = connection.getresponse()
                    ports.append(json.loads(response.read())['backend_peer_port'])
                    require(response.status == 200, 'Persistent request failed')
                    expected[path] = ('GET', 200, 'Success')
                require(ports[0] == ports[1], 'Upstream connection was not reused')
            finally:
                connection.close()
            for status in (204, 205, 304):
                _, body = check(f'/{status}', status)
                require(body == b'', f'{status} unexpectedly had content')
            _, body = check('/head', 200, method='HEAD')
            require(body == b'', 'HEAD had content')
            with socket.create_connection(('127.0.0.1', port), timeout=3) as client:
                client.sendall(b'GET /205?wire=1 HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n')
                wire = bytearray()
                while True:
                    fragment = client.recv(8192)
                    if not fragment:
                        break
                    wire.extend(fragment)
            head, body = bytes(wire).split(b'\r\n\r\n', 1)
            require(head.startswith(b'HTTP/1.1 205'), 'Wrong wire status')
            require(b'content-length: 0' in head.lower().split(b'\r\n'), '205 lacks zero framing')
            require(b'transfer-encoding:' not in head.lower() and body == b'', '205 framing/content invalid')
            (output / 'http205-wire.txt').write_bytes(wire)
            expected['/205?wire=1'] = ('GET', 205, 'Success')
            stop_proxy(process)
        records = {}
        for line in (output / 'protocol-observations.log').read_text().splitlines():
            match = RECORD.fullmatch(line)
            require(match is not None, f'Unparseable record: {line!r}')
            method, path, status, outcome = match.groups()
            require(path not in records, f'Duplicate record: {path}')
            records[path] = (method, int(status), outcome)
        for path, record in expected.items():
            require(records.get(path) == record, f'Wrong or missing observation: {path}')
        diagnostics = (output / 'protocol-stderr.log').read_text()
        require(f'observation_queue enqueued={len(records)} dropped=0 full=0 closed=0' in diagnostics, 'Observation delivery loss')
        require(f'observation_output written={len(records)} unconfirmed=0 drained=true' in diagnostics, 'Observation drain failed')
        return {'checked_requests': len(expected), 'observations': len(records), 'via': 'PASS', 'http205_wire': 'PASS', 'connection_reuse': 'PASS'}
    finally:
        cleanup(process)


def stderr_checks(binary, backend_port, output, mode):
    process = None
    try:
        with (output / f'{mode}-observations.log').open('wb') as observations:
            process, port = start_proxy(binary, backend_port, observations, subprocess.PIPE, name=mode)
            if sys.platform.startswith('linux'):
                import fcntl
                fcntl.fcntl(process.stderr.fileno(), fcntl.F_SETPIPE_SZ, 4096)
            if mode == 'closed-stderr':
                process.stderr.close()
            wait_ready(process, port)
            count = 10 if mode == 'closed-stderr' else (600 if os.name == 'nt' or sys.platform.startswith('linux') else 2500)
            for i in range(count):
                require(request(port, 'GET', f'/diagnostic-load/{i}')[0] == 200, f'HTTP stalled with {mode}')
            captured = []
            if mode == 'drain-stderr':
                reader = threading.Thread(target=lambda: captured.append(process.stderr.read()), daemon=True)
                reader.start()
                stop_proxy(process)
                reader.join(timeout=5)
                require(not reader.is_alive(), 'Stderr reader did not finish')
                text = captured[0].decode('utf-8')
                (output / f'{mode}.log').write_text(text, encoding='utf-8')
                summary = re.search(r'diagnostics_queue enqueued=(\d+) written=(\d+) dropped_full=(\d+) dropped_closed=(\d+)', text)
                require(summary is not None, 'Missing diagnostics summary')
                enqueued, written, full, closed = map(int, summary.groups())
                require(enqueued == written and full > 0 and closed == 0, 'Expected bounded diagnostic drops did not occur')
                return {'http_200': count, 'enqueued': enqueued, 'written': written, 'dropped_full': full, 'exit_code': 0}
            started = time.monotonic()
            stop_proxy(process, expected_exit=1)
            elapsed = time.monotonic() - started
            if mode != 'closed-stderr':
                (output / f'{mode}.log').write_bytes(process.stderr.read())
            return {'http_200': count, 'exit_code': 1, 'shutdown_seconds': round(elapsed, 3)}
    finally:
        cleanup(process)


def echo_checks(binary, output):
    backend = proxy = None
    backend_port = available_port()
    code = 'from http.server import ThreadingHTTPServer; from examples.echo_server import EchoHandler; import sys; ThreadingHTTPServer(("127.0.0.1",int(sys.argv[1])),EchoHandler).serve_forever()'
    try:
        with (output / 'echo-backend.log').open('wb') as backend_log, (output / 'echo-observations.log').open('wb') as observations, (output / 'echo-stderr.log').open('wb') as diagnostics:
            def start_backend():
                child = subprocess.Popen([sys.executable, '-c', code, str(backend_port)], cwd=ROOT, stdout=backend_log, stderr=backend_log)
                wait_ready(child, backend_port)
                return child
            backend = start_backend()
            proxy, port = start_proxy(binary, backend_port, observations, diagnostics, name='echo-review')
            wait_ready(proxy, port)
            require(request(port, 'GET', '/hello?q=exact')[2] == b'GET /hello?q=exact\n', 'Echo GET failed')
            payload = b'{"service":"checkout"}'
            require(request(port, 'POST', '/echo', payload, {'Content-Type': 'application/json'})[2] == payload, 'JSON POST changed')
            require(request(port, 'HEAD', '/head')[2] == b'', 'Echo HEAD returned content')
            require(request(port, 'POST', '/echo', b'\x00\xff\x01\x80')[2] == b'\x00\xff\x01\x80', 'Binary echo changed')
            for i in range(3):
                require(request(port, 'GET', f'/sequential/{i}')[0] == 200, 'Sequential echo failed')
            with concurrent.futures.ThreadPoolExecutor(max_workers=5) as pool:
                results = list(pool.map(lambda i: request(port, 'GET', f'/concurrent/{i}')[0], range(5)))
                require(results == [200] * 5, 'Concurrent echo failed')
            cleanup(backend)
            backend = None
            require(request(port, 'GET', '/offline')[0] == 502, 'Unavailable backend did not return 502')
            backend = start_backend()
            require(request(port, 'GET', '/recovered')[0] == 200, 'Backend restart recovery failed')
            stop_proxy(proxy)
        return {'get_json_head_binary': 'PASS', 'sequential_concurrent': 'PASS', 'backend_restart': 'PASS'}
    finally:
        cleanup(proxy)
        cleanup(backend)


def main():
    binary = ROOT / 'target' / 'release' / ('exaprop.exe' if os.name == 'nt' else 'exaprop')
    require(binary.is_file(), 'Build the release binary first')
    output = ROOT / 'tmp' / 'http-review' / f'{time.strftime("%Y%m%d-%H%M%S")}-{os.getpid()}'
    output.mkdir(parents=True, exist_ok=False)
    report = {'status': 'FAIL', 'platform': sys.platform, 'python': sys.version}
    server = QuietServer(('127.0.0.1', 0), Backend)
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    try:
        backend_port = server.server_address[1]
        report['protocol'] = protocol_checks(binary, backend_port, output)
        for mode in ('drain-stderr', 'blocked-stderr', 'closed-stderr'):
            report[mode] = stderr_checks(binary, backend_port, output, mode)
        report['echo'] = echo_checks(binary, output)
        report['status'] = 'PASS'
    except Exception as error:
        report['error'] = str(error)
        raise
    finally:
        server.shutdown()
        server.server_close()
        worker.join(timeout=3)
        (output / 'result.json').write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
        print(f'{report["status"]}: {output}')


if __name__ == '__main__':
    main()
