"""M4.3 CLI acceptance checks using a local, standard-library HTTP backend.

Run after cargo build --workspace --release --locked. Keeps the M3 smoke suites
unchanged; broader integration verification remains a separate M4.4 milestone.
"""

import argparse
import concurrent.futures
from decimal import Decimal
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

from smoke_fastapi import available_port, cleanup, request, require, stop_proxy, wait_ready
from smoke_http_review import windows_options

ROOT = Path(__file__).resolve().parents[1]
FIELD = re.compile(r'([a-z_][a-z0-9_]*)=("(?:\\.|[^"\\])*"|[^ ]+)')


class Backend(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def handle(self):
        try:
            super().handle()
        except ConnectionError:
            # On Windows, the intentionally timed-out proxy can abort the
            # persistent connection while the fixture reads its next request.
            pass

    def log_message(self, *_):
        pass

    def do_GET(self):
        path = self.path.split('?', 1)[0]
        if path == '/slow':
            time.sleep(1.2)
        elif path == '/timeout':
            time.sleep(16)
        status = {'/missing': 404, '/500': 500, '/503': 503, '/504': 504}.get(path, 200)
        body = b'backend response\n'
        try:
            self.send_response(status)
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        except ConnectionError:
            pass  # The real timeout case deliberately closes its upstream socket.


class Server(ThreadingHTTPServer):
    daemon_threads = True


def parse_line(line, prefix):
    require(line.startswith(prefix + ' '), f'Unexpected prefix: {line}')
    fields = {}
    pos = len(prefix) + 1
    while pos < len(line):
        match = FIELD.match(line, pos)
        require(match is not None, f'Malformed V1 field at {pos}: {line}')
        key, value = match.groups()
        require(key not in fields, f'Duplicate field: {key}')
        fields[key] = value
        pos = match.end()
        if pos < len(line):
            require(line[pos] == ' ', f'Malformed field separator: {line}')
            pos += 1
    return fields


def records(path):
    data = path.read_bytes()
    require(not data or data.endswith(b'\n'), 'Incomplete output line')
    observations, events = {}, {}
    for raw in data.splitlines(keepends=True):
        require(raw.isascii(), 'V1 output must use ASCII escaping')
        require(all(byte >= 32 or byte == 10 for byte in raw) and b'\x7f' not in raw, 'Raw control character in V1 output')
        line = raw.decode('ascii').rstrip('\n')
        prefix = line.split(' ', 1)[0]
        require(prefix in ('OBSERVATION_V1', 'ANOMALY_V1'), f'Unexpected stdout: {line}')
        require(len(raw) <= (4096 if prefix == 'OBSERVATION_V1' else 1024), 'Output byte cap exceeded')
        fields = parse_line(line, prefix)
        seq = int(fields['seq'])
        destination = observations if prefix == 'OBSERVATION_V1' else events
        require(seq not in destination, f'Reused sequence: {seq}')
        destination[seq] = fields
    return observations, events


def summary(path, prefix):
    matches = [line for line in path.read_text().splitlines() if line.startswith(prefix + ' ')]
    require(len(matches) == 1, f'Missing or duplicate {prefix} summary')
    require(len(matches[0].encode()) + 1 <= 1024, 'Diagnostic line cap exceeded')
    return parse_line(matches[0], prefix)


def start(binary, backend_port, stdout, stderr, extra=()):
    port = available_port()
    process = subprocess.Popen(
        [str(binary), 'attach', '--listen', f'127.0.0.1:{port}', '--target', f'127.0.0.1:{backend_port}',
         '--name', 'cli-review', '--detect', *extra],
        cwd=ROOT, stdout=stdout, stderr=stderr, **windows_options(),
    )
    return process, port


def preflight(binary):
    bad = [
        ['--latency-threshold-ms', '500'],
        *[['--detect', '--latency-threshold-ms', value] for value in ('0', '-1', '1.5', 'text', '3600001', '18446744073709551616')],
        ['--detect', '--latency-threshold-ms', '1', '--latency-threshold-ms', '2'],
        ['--detect', '--detect'],
        *[['--detect', '--name', value] for value in ('', ' ', 'x\x1b[31m', 'x\nname', 'x' * 129)],
    ]
    for flags in bad:
        port = available_port()
        result = subprocess.run([str(binary), 'attach', '--listen', f'127.0.0.1:{port}', *flags], capture_output=True, timeout=6)
        require(result.returncode == 2, f'Invalid flags accepted: {flags}')
        require(result.stdout == b'', 'Invalid arguments produced telemetry')
        require(b'\x1b' not in result.stderr, 'Raw terminal escape in CLI error')
        with socket.socket() as probe:
            probe.bind(('127.0.0.1', port))
    for flags in (['--help'], ['--version'], ['attach', '--help']):
        result = subprocess.run([str(binary), *flags], capture_output=True, timeout=6)
        require(result.returncode == 0 and result.stdout, 'Help/version failed')
    require(b'--detect' in result.stdout and b'--latency-threshold-ms' in result.stdout, 'Missing flag help')
    return {'invalid_invocations': len(bad), 'help_and_version': 'PASS'}


def classification(binary, backend_port, output, custom):
    label = 'custom' if custom else 'default'
    out, err = output / f'{label}-stdout.log', output / f'{label}-stderr.log'
    process = None
    threshold = 500 if custom else 1000
    try:
        with out.open('wb') as stdout, err.open('wb') as stderr:
            process, port = start(binary, backend_port, stdout, stderr, ['--latency-threshold-ms', str(threshold)] if custom else [])
            wait_ready(process, port)  # A real observation; included in settled counts.
            for path, expected in [('/fast?token=NEVER_SHOW_TOKEN', 200), ('/missing', 404), ('/500', 500), ('/503', 503), ('/504', 504), ('/slow', 200), ('/timeout', 504)]:
                status, _, body = request(port, 'GET', path)
                require(status == expected, f'Wrong HTTP status for {path}: {status}')
                if path != '/timeout':
                    require(body == b'backend response\n', 'Forwarded body changed')
            stop_proxy(process)
        observations, events = records(out)
        require(list(observations) == list(range(1, len(observations) + 1)), 'Sequence gaps without saturation')
        require(set(events) <= set(observations), 'Anomaly without observation')
        for seq, obs in observations.items():
            reasons = []
            if 500 <= int(obs['status']) <= 599:
                reasons.append('http_server_error')
            if Decimal(obs['duration_ms']) > threshold:
                reasons.append('elevated_request_duration')
            if obs['outcome'] == 'BackendTimeout':
                reasons.append('upstream_timeout')
            require((seq in events) == bool(reasons), f'Wrong classification: {obs}')
            if reasons:
                event = events[seq]
                require(event['reasons'] == ','.join(reasons), 'Wrong reason set/order')
                for key in ('timestamp', 'service', 'method', 'status', 'outcome', 'duration_ms'):
                    require(event[key] == obs[key], f'Evidence mismatch: {key}')
                require(event['origin'] == ('backend' if obs['outcome'] == 'Success' else 'proxy'), 'Wrong provenance')
                require(event['severity'] == ('warning' if reasons == ['elevated_request_duration'] else 'error'), 'Wrong severity')
                require(event['threshold_ms'] == (str(threshold) if 'elevated_request_duration' in reasons else 'none'), 'Wrong threshold evidence')
                require(set(event) == {'seq', 'timestamp', 'service', 'method', 'status', 'outcome', 'origin', 'severity', 'duration_ms', 'reasons', 'threshold_ms', 'truncated_fields'}, 'Anomaly privacy allowlist changed')
        paths = {obs['path']: obs for obs in observations.values()}
        require(paths['"/timeout"']['outcome'] == 'BackendTimeout', 'Real timeout not classified')
        require(paths['"/504"']['outcome'] == 'Success', 'Backend 504 mistaken for timeout')
        require('NEVER_SHOW_TOKEN' not in out.read_text(), 'Recognized query secret leaked')
        stats, delivery = summary(err, 'detection_stats'), summary(err, 'anomaly_output')
        require(delivery['state'] == 'clean', 'Not a clean drain')
        require(int(stats['evaluated']) == len(observations), 'Input was not fully classified')
        require(int(stats['anomalous']) == len(events), 'Wrong anomaly count')
        require(int(delivery['written_obs']) == len(observations), 'Wrong observation writes')
        require(int(delivery['written_events']) == len(events), 'Wrong event writes')
        require(delivery['input_unevaluated'] == '0' and delivery['input_unconfirmed'] == '0', 'Unexpected losses')
        return {'threshold_ms': threshold, 'observations': len(observations), 'anomalies': len(events), 'state': 'clean'}
    finally:
        cleanup(process)


def bind_failure(binary, backend_port, output):
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        listener.listen()
        result = subprocess.run([str(binary), 'attach', '--listen', f'127.0.0.1:{listener.getsockname()[1]}', '--target', f'127.0.0.1:{backend_port}', '--detect'], capture_output=True, timeout=6)
        require(result.returncode == 1, 'Listener bind error did not fail')
        require(not result.stdout, 'Bind error emitted observations')
        (output / 'bind-failure-stderr.log').write_bytes(result.stderr)
        require(summary(output / 'bind-failure-stderr.log', 'anomaly_output')['state'] == 'clean', 'Empty telemetry did not tear down after bind failure')
    return 'PASS'


def sink_failure(binary, backend_port, output, mode):
    out, err = output / f'{mode}-stdout.log', output / f'{mode}-stderr.log'
    process = None
    try:
        with out.open('wb') as stdout, err.open('wb') as stderr:
            process, port = start(binary, backend_port, subprocess.PIPE if mode != 'blocked-stderr' else stdout, subprocess.PIPE if mode == 'blocked-stderr' else stderr)
            if mode == 'closed-stdout':
                process.stdout.close()
            wait_ready(process, port)
            count = 5 if mode == 'closed-stdout' else 600
            time.sleep(0.1)
            with concurrent.futures.ThreadPoolExecutor(max_workers=8) as executor:
                statuses = list(executor.map(lambda _: request(port, 'GET', '/500')[0], range(count)))
            require(all(status == 500 for status in statuses), 'Telemetry sink stalled or altered HTTP forwarding')
            started = time.monotonic()
            stop_proxy(process, expected_exit=1)
            elapsed = time.monotonic() - started
            require(elapsed < 7, 'Shutdown exceeded telemetry/diagnostics wait budgets')
            if mode == 'blocked-stderr':
                err.write_bytes(process.stderr.read())
            elif mode == 'blocked-stdout':
                out.write_bytes(process.stdout.read())
        result = {'checked_requests': count, 'exit_code': process.returncode, 'shutdown_seconds': round(elapsed, 3)}
        if mode != 'blocked-stderr':
            stats, delivery = summary(err, 'detection_stats'), summary(err, 'anomaly_output')
            expected = 'failed' if mode == 'closed-stdout' else 'timed_out'
            require(delivery['state'] == expected, 'Sink failure reported clean')
            result.update({'state': expected, 'output_dropped': int(stats['drops_obs_full']) + int(stats['drops_obs_closed'])})
            if mode == 'closed-stdout':
                require(int(delivery['write_errors']) == 1, 'Writer error counted incorrectly')
                require(int(stats['drops_events_closed']) > 0, 'Closed sink losses not counted')
            else:
                require(result['output_dropped'] > 0, 'Blocked output did not exercise queue saturation')
        else:
            observations, events = records(out)
            require(len(events) == count and len(observations) >= count, 'Blocked stderr affected stdout drainage')
        return result
    finally:
        cleanup(process)
        if process is not None:
            for stream in (process.stdout, process.stderr):
                if stream is not None and not stream.closed:
                    stream.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target' / 'release' / ('exaprop.exe' if os.name == 'nt' else 'exaprop'))
    args = parser.parse_args()
    output = ROOT / 'tmp' / 'm4-cli' / f'{time.strftime("%Y%m%d-%H%M%S")}-{os.getpid()}'
    output.mkdir(parents=True)
    report = {'status': 'FAIL', 'platform': sys.platform, 'binary': str(args.binary)}
    server = Server(('127.0.0.1', 0), Backend)
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    try:
        report['preflight'] = preflight(args.binary)
        report['default'] = classification(args.binary, server.server_port, output, False)
        report['custom'] = classification(args.binary, server.server_port, output, True)
        report['bind_failure'] = bind_failure(args.binary, server.server_port, output)
        for mode in ('closed-stdout', 'blocked-stdout', 'blocked-stderr'):
            report[mode] = sink_failure(args.binary, server.server_port, output, mode)
        report['status'] = 'PASS'
    finally:
        server.shutdown()
        server.server_close()
        worker.join(timeout=2)
        (output / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
        print(f'{report["status"]}: {output}')


if __name__ == '__main__':
    main()
