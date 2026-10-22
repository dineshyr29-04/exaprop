"""Black-box smoke checks against a real FastAPI/Uvicorn backend.

Run after building ExaProp and installing examples/requirements-smoke.txt.
All child processes are owned by this harness and cleaned up on failure.
"""

import argparse
import concurrent.futures
import http.client
import json
import os
from pathlib import Path
import re
import signal
import socket
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
OBSERVATION = re.compile(
    r'^\[fastapi-smoke\] (\S+) (\S+) -> (\d+) '
    r'\(([\d.]+)ms\) \[(\w+)\] timestamp=\d+\.\d{9}$'
)


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def available_port():
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        return listener.getsockname()[1]


def request(port, method, path, body=None, headers=None):
    connection = http.client.HTTPConnection('127.0.0.1', port, timeout=25)
    try:
        connection.request(method, path, body=body, headers=headers or {})
        response = connection.getresponse()
        return response.status, response.getheaders(), response.read()
    finally:
        connection.close()


def wait_ready(process, port):
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        require(process.poll() is None, f'Process exited during startup: {process.returncode}')
        try:
            if request(port, 'GET', '/health')[0] == 200:
                return
        except (OSError, http.client.HTTPException):
            pass
        time.sleep(0.05)
    raise TimeoutError(f'No health response on port {port}')


def stop_proxy(process, expected_exit=0):
    if os.name != 'nt':
        process.send_signal(signal.SIGTERM)
    else:
        # The proxy has its own hidden console. Attach an isolated helper to
        # that console; no signal is sent to the user's terminal or other apps.
        helper = r'''
import ctypes, sys, time
kernel = ctypes.WinDLL('kernel32', use_last_error=True)
kernel.FreeConsole()
if not kernel.AttachConsole(int(sys.argv[1])):
    raise ctypes.WinError(ctypes.get_last_error())
handler_type = ctypes.WINFUNCTYPE(ctypes.c_int, ctypes.c_uint)
handler = handler_type(lambda event: 1)
if not kernel.SetConsoleCtrlHandler(handler, True):
    raise ctypes.WinError(ctypes.get_last_error())
if not kernel.GenerateConsoleCtrlEvent(1, 0):
    raise ctypes.WinError(ctypes.get_last_error())
time.sleep(0.2)
'''
        subprocess.run(
            [sys.executable, '-c', helper, str(process.pid)],
            creationflags=subprocess.CREATE_NO_WINDOW,
            check=True, timeout=5, capture_output=True, text=True,
        )
    actual_exit = process.wait(timeout=8)
    require(actual_exit == expected_exit, f'Expected proxy exit {expected_exit}, got {actual_exit}')


def cleanup(process):
    if process is not None and process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target' / 'release' / ('exaprop.exe' if os.name == 'nt' else 'exaprop'))
    parser.add_argument('--output', type=Path)
    parser.add_argument('--include-timeouts', action='store_true', help='Also exercise both 15-second upstream timeout paths')
    args = parser.parse_args()
    binary = args.binary.resolve()
    require(binary.is_file(), f'Build the binary first: {binary}')
    output = args.output or ROOT / 'tmp' / 'fastapi-smoke' / f'{time.strftime("%Y%m%d-%H%M%S")}-{os.getpid()}'
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    backend_port = available_port()
    proxy_port = available_port()
    while proxy_port == backend_port:
        proxy_port = available_port()
    expected = {}
    backend = proxy = broken_proxy = None
    started = time.monotonic()
    report = {'status': 'FAIL', 'binary': str(binary), 'python': sys.version, 'platform': sys.platform, 'include_timeouts': args.include_timeouts}
    try:
        with (output / 'backend.log').open('w', encoding='utf-8') as backend_log, \
                (output / 'observations.log').open('w', encoding='utf-8') as observations, \
                (output / 'proxy-stderr.log').open('w', encoding='utf-8') as diagnostics:
            backend = subprocess.Popen(
                [sys.executable, '-m', 'uvicorn', 'examples.fastapi_backend:app', '--host', '127.0.0.1', '--port', str(backend_port), '--no-access-log'],
                cwd=ROOT, stdout=backend_log, stderr=backend_log,
            )
            wait_ready(backend, backend_port)
            windows_options = {}
            if os.name == 'nt':
                startup = subprocess.STARTUPINFO()
                startup.dwFlags |= subprocess.STARTF_USESHOWWINDOW
                startup.wShowWindow = 0
                windows_options = {'creationflags': subprocess.CREATE_NEW_CONSOLE, 'startupinfo': startup}
            proxy = subprocess.Popen(
                [str(binary), 'attach', '--listen', f'127.0.0.1:{proxy_port}', '--target', f'127.0.0.1:{backend_port}', '--name', 'fastapi-smoke'],
                cwd=ROOT, stdout=observations, stderr=diagnostics, **windows_options,
            )
            wait_ready(proxy, proxy_port)

            def check(case, path, method='GET', body=None, headers=None, status=200, outcome='Success'):
                path += ('&' if '?' in path else '?') + 'case=' + case
                expected[path] = (method, status, outcome)
                actual_status, actual_headers, actual_body = request(proxy_port, method, path, body, headers)
                require(actual_status == status, f'{case}: expected HTTP {status}, got {actual_status}')
                return actual_headers, actual_body

            _, body = check('get', '/items/42', headers={'Authorization': 'Bearer HEADER_SENTINEL'})
            require(json.loads(body)['item_id'] == 42, 'FastAPI route parameter changed')
            require(json.loads(body)['authorization_received'], 'Authorization header was not forwarded')
            query = '%2574oken=NESTED_SENTINEL&a.p.i-k+e+y=ODD_SENTINEL&p%D0%B0ssword=UNICODE_SENTINEL&pwd=PWD_SENTINEL&page=2'
            _, body = check('privacy', '/items/7?' + query)
            require(json.loads(body)['raw_query'] == query + '&case=privacy', 'Privacy filtering changed the backend request')
            # Observation targets are sanitized; update only the expected log key.
            raw_target = '/items/7?' + query + '&case=privacy'
            safe_target = '/items/7?%2574oken=[REDACTED]&a.p.i-k+e+y=[REDACTED]&p%D0%B0ssword=[REDACTED]&pwd=[REDACTED]&page=2&case=privacy'
            expected[safe_target] = expected.pop(raw_target)
            payload = b'BODY_SENTINEL\x00\xff'
            _, body = check('post', '/echo', method='POST', body=payload)
            require(body == payload, 'Binary POST body changed')
            headers, _ = check('cookies', '/cookies')
            require(sum(name.lower() == 'set-cookie' for name, _ in headers) == 2, 'Duplicate Set-Cookie headers lost')
            _, body = check('head', '/health', method='HEAD')
            require(body == b'', 'HEAD returned body bytes')
            for status in (404, 500, 502):
                check(f'status-{status}', f'/status/{status}', status=status)
            check('delay', '/delay?milliseconds=200')
            _, body = check('stream', '/stream?milliseconds=200')
            require(body == b'first-last', 'Streamed response body changed')
            _, body = check('request-limit', '/echo', method='POST', body=b'x' * 4194304)
            require(len(body) == 4194304, 'Exact-limit request was truncated')
            check('request-over', '/echo', method='POST', body=b'x' * 4194305, status=413, outcome='Rejected')
            _, body = check('response-limit', '/bytes/4194304')
            require(len(body) == 4194304, 'Exact-limit response was truncated')
            check('response-over', '/bytes/4194305', status=502, outcome='BackendFailure')
            with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
                # Separate requests and unique paths avoid shared expected-map mutation.
                jobs = {f'/items/{i}?case=concurrent-{i}': pool.submit(request, proxy_port, 'GET', f'/items/{i}?case=concurrent-{i}') for i in range(20)}
                for path, future in jobs.items():
                    require(future.result()[0] == 200, f'Concurrent request failed: {path}')
                    expected[path] = ('GET', 200, 'Success')
            if args.include_timeouts:
                check('header-timeout', '/delay?milliseconds=16000', status=504, outcome='BackendTimeout')
                check('body-timeout', '/stream?milliseconds=16000', status=504, outcome='BackendTimeout')
            # Exercise actual OS-pipe failure, not just a mock writer. Forwarding
            # must continue, closed-consumer drops must be counted, and shutdown
            # must report failure rather than claim a successful drain.
            broken_port = available_port()
            with (output / 'broken-sink-stderr.log').open('w', encoding='utf-8') as broken_log:
                broken_proxy = subprocess.Popen(
                    [str(binary), 'attach', '--listen', f'127.0.0.1:{broken_port}', '--target', f'127.0.0.1:{backend_port}', '--name', 'broken-sink'],
                    cwd=ROOT, stdout=subprocess.PIPE, stderr=broken_log, **windows_options,
                )
                broken_proxy.stdout.close()
                wait_ready(broken_proxy, broken_port)
                for i in range(3):
                    time.sleep(0.05)
                    require(request(broken_port, 'GET', f'/items/{i}')[0] == 200, 'Broken telemetry sink blocked forwarding')
                stop_proxy(broken_proxy, expected_exit=1)
            broken_text = (output / 'broken-sink-stderr.log').read_text(encoding='utf-8')
            queue = re.search(r'observation_queue enqueued=(\d+) dropped=(\d+) full=(\d+) closed=(\d+)', broken_text)
            require(queue is not None, 'Missing broken-sink queue summary')
            enqueued, dropped, full, closed = map(int, queue.groups())
            require(closed > 0 and dropped == closed and full == 0, 'Closed-sink drops were not measured correctly')
            require(f'observation_output written=0 unconfirmed={enqueued} drained=false' in broken_text, 'Broken output was reported as drained')
            report['broken_sink'] = {'exit_code': 1, 'dropped_closed': closed, 'unconfirmed': enqueued, 'forwarding': 'PASS'}
            cleanup(backend)
            backend = None
            check('backend-offline', '/health', status=502, outcome='BackendFailure')
            stop_proxy(proxy)

        text = (output / 'observations.log').read_text(encoding='utf-8')
        diagnostics = (output / 'proxy-stderr.log').read_text(encoding='utf-8')
        records = {}
        for line in text.splitlines():
            match = OBSERVATION.fullmatch(line)
            require(match is not None, f'Malformed observation: {line!r}')
            method, path, status, duration, outcome = match.groups()
            require(path not in records, f'Duplicate observation: {path}')
            records[path] = (method, int(status), outcome, float(duration))
        for path, wanted in expected.items():
            require(path in records, f'Missing observation: {path}')
            require(records[path][:3] == wanted, f'Incorrect observation: {path}: {records[path]}')
        for path, record in records.items():
            if 'case=delay' in path or 'case=stream' in path:
                require(record[3] >= 180, f'Latency did not include upstream body/work: {path}')
        for secret in ('HEADER_SENTINEL', 'BODY_SENTINEL', 'NESTED_SENTINEL', 'ODD_SENTINEL', 'UNICODE_SENTINEL', 'PWD_SENTINEL', 'synthetic-session'):
            require(secret not in text, f'Private value leaked: {secret}')
        require(f'observation_queue enqueued={len(records)} dropped=0 full=0 closed=0' in diagnostics, 'Queue accounting mismatch')
        require(f'observation_output written={len(records)} unconfirmed=0 drained=true' in diagnostics, 'Queue did not drain completely')
        report.update(status='PASS', checked_requests=len(expected), observed_records=len(records), shutdown='graceful', elapsed_seconds=round(time.monotonic() - started, 3))
    except Exception as error:
        report['error'] = str(error)
        raise
    finally:
        cleanup(broken_proxy)
        cleanup(proxy)
        cleanup(backend)
        (output / 'result.json').write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
        print(f'{report["status"]}: {output}')


if __name__ == '__main__':
    main()
