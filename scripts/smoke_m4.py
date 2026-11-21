"""M4.4 integration campaign against the existing FastAPI fixture.

Run after a locked release build and installing examples/requirements-smoke.txt.
Keeps the M3 and M4.3 suites unchanged; reuses their HTTP, signal and V1 parsers.
All traffic and privacy sentinels are synthetic. Evidence stays under tmp/m4/.
"""

import argparse
import concurrent.futures
from decimal import Decimal
import http.client
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import sys
import time

from smoke_fastapi import available_port, cleanup, request, require, stop_proxy, wait_ready
from smoke_http_review import windows_options
from smoke_m4_cli import records, summary

ROOT = Path(__file__).resolve().parents[1]
THRESHOLD = 250
REASONS = ('http_server_error', 'elevated_request_duration', 'upstream_timeout')
NORMAL_FIELDS = {'seq', 'timestamp', 'service', 'method', 'path', 'query', 'status', 'outcome', 'duration_ms', 'truncated_fields'}
EVENT_FIELDS = (NORMAL_FIELDS - {'path', 'query'}) | {'origin', 'severity', 'reasons', 'threshold_ms'}


def assert_equal(actual, expected, message):
    require(actual == expected, f'{message}: expected {expected!r}, got {actual!r}')


def verify_capture(stdout, stderr, expected, threshold=THRESHOLD):
    observations, events = records(stdout)
    require(observations, 'No observation records')
    assert_equal(list(observations), list(range(1, len(observations) + 1)), 'Receive-order sequence identifiers')
    require(set(events) <= set(observations), 'Event without originating observation')
    seen, counts, readiness = {}, dict.fromkeys(REASONS, 0), 0
    for seq, obs in observations.items():
        assert_equal(set(obs), NORMAL_FIELDS, 'Observation field contract')
        assert_equal(obs['seq'], str(seq), 'Sequence scalar')
        require(re.fullmatch(r'-?\d+\.\d{9}', obs['timestamp']), 'Timestamp representation changed')
        require(re.fullmatch(r'\d+\.\d{6}', obs['duration_ms']), 'Duration must preserve integer-derived nanoseconds')
        assert_equal(obs['service'], '"m4-integration"', 'Service evidence')
        assert_equal(obs['truncated_fields'], 'none', 'Unexpected fixture truncation')
        match = re.search(r'(?:["&])case=([a-z0-9-]+)(?:["&])', obs['query'])
        if match:
            case = match.group(1)
            require(case in expected and case not in seen, f'Unknown or duplicate observation case: {case}')
            wanted = expected[case]
            assert_equal((obs['method'], obs['path'], int(obs['status']), obs['outcome']), wanted[:4], f'{case} factual evidence')
            if wanted[4] is not None:
                require(Decimal(obs['duration_ms']) >= wanted[4], f'{case} excluded expected body/work delay')
            seen[case] = seq
        else:
            assert_equal((obs['path'], obs['status'], obs['outcome']), ('"/health"', '200', 'Success'), 'Readiness observation')
            readiness += 1
        matched = []
        if 500 <= int(obs['status']) <= 599:
            matched.append(REASONS[0])
        if Decimal(obs['duration_ms']) > threshold:
            matched.append(REASONS[1])
        if obs['outcome'] == 'BackendTimeout':
            matched.append(REASONS[2])
        assert_equal(seq in events, bool(matched), 'At most one event per matched observation')
        for reason in matched:
            counts[reason] += 1
        if not matched:
            continue
        event = events[seq]
        assert_equal(set(event), EVENT_FIELDS, 'Anomaly privacy field allowlist')
        for key in NORMAL_FIELDS - {'path', 'query', 'truncated_fields'}:
            assert_equal(event[key], obs[key], f'Anomaly scalar evidence {key}')
        assert_equal(event['reasons'], ','.join(matched), 'Canonical matching reason order')
        assert_equal(event['severity'], 'warning' if matched == [REASONS[1]] else 'error', 'Severity maximum')
        assert_equal(event['origin'], 'backend' if obs['outcome'] == 'Success' else 'proxy', 'Outcome-derived provenance')
        assert_equal(event['threshold_ms'], str(threshold) if REASONS[1] in matched else 'none', 'Threshold evidence')
        assert_equal(event['truncated_fields'], 'none', 'Event truncation')
    assert_equal(set(seen), set(expected), 'Every completed test request observed exactly once')
    require(readiness >= 1, 'Missing real readiness observation')
    stats, delivery = summary(stderr, 'detection_stats'), summary(stderr, 'anomaly_output')
    totals = {
        'evaluated': len(observations), 'normal': len(observations) - len(events),
        'anomalous': len(events), 'http_5xx': counts[REASONS[0]],
        'duration': counts[REASONS[1]], 'timeout': counts[REASONS[2]],
        'batches_queued': len(observations), 'events_queued': len(events),
        'drops_obs_full': 0, 'drops_obs_closed': 0, 'drops_events_full': 0,
        'drops_events_closed': 0, 'truncated_obs': 0, 'truncated_events': 0,
    }
    assert_equal(set(stats), set(totals) | {'saturated'}, 'Classification summary fields')
    for key, value in totals.items():
        assert_equal(int(stats[key]), value, f'Settled classification counter {key}')
    assert_equal(stats['saturated'], 'false', 'Counter saturation')
    output_totals = {
        'written_obs': len(observations), 'written_events': len(events),
        'output_unconfirmed_obs': 0, 'output_unconfirmed_events': 0, 'write_errors': 0,
        'input_enqueued': len(observations), 'input_dropped_full': 0,
        'input_dropped_closed': 0, 'input_unevaluated': 0, 'input_unconfirmed': 0,
    }
    assert_equal(set(delivery), set(output_totals) | {'state'}, 'Output summary fields')
    for key, value in output_totals.items():
        assert_equal(int(delivery[key]), value, f'Settled output counter {key}')
    assert_equal(delivery['state'], 'clean', 'Telemetry completion')
    return {'observations': len(observations), 'events': len(events), 'readiness': readiness,
            'classification': stats, 'output': delivery}


def start_proxy(binary, backend_port, stdout, stderr):
    port = available_port()
    proxy = subprocess.Popen(
        [str(binary), 'attach', '--listen', f'127.0.0.1:{port}', '--target', f'127.0.0.1:{backend_port}',
         '--name', 'm4-integration', '--detect', '--latency-threshold-ms', str(THRESHOLD)],
        cwd=ROOT, stdout=stdout, stderr=stderr, **windows_options(),
    )
    return proxy, port


def client_body_timeout(port):
    # Send valid headers but never finish the declared body. The real production
    # 15-second client-frame timeout must be preserved (not an upstream timeout).
    with socket.create_connection(('127.0.0.1', port), timeout=25) as client:
        client.settimeout(25)
        client.sendall(b'POST /echo?case=client-timeout HTTP/1.1\r\nHost: localhost\r\nContent-Length: 100\r\nConnection: close\r\n\r\nx')
        response = http.client.HTTPResponse(client)
        response.begin()
        status = response.status
        response.read()
        assert_equal(status, 408, 'Client body timeout wire status')


def delayed_private_body(port):
    payload = b'M4_ANOMALOUS_BODY_SECRET\x00\xff'
    with socket.create_connection(('127.0.0.1', port), timeout=25) as client:
        client.settimeout(25)
        client.sendall(
            f'POST /echo?case=private-slow-body HTTP/1.1\r\nHost: localhost\r\nContent-Length: {len(payload)}\r\nAuthorization: Bearer M4_HEADER_SECRET\r\nConnection: close\r\n\r\n'.encode()
            + payload[:1]
        )
        time.sleep(0.4)
        client.sendall(payload[1:])
        response = http.client.HTTPResponse(client)
        response.begin()
        assert_equal(response.status, 200, 'Delayed body wire status')
        assert_equal(response.read(), payload, 'Delayed sensitive body forwarding')


def campaign(binary, output):
    backend = proxy = None
    expected = {}
    backend_port = available_port()
    stdout, stderr = output / 'observations.log', output / 'proxy-stderr.log'
    started = time.monotonic()
    try:
        with (output / 'backend.log').open('wb') as backend_log, stdout.open('wb') as out, stderr.open('wb') as err:
            backend = subprocess.Popen(
                [sys.executable, '-m', 'uvicorn', 'examples.fastapi_backend:app', '--host', '127.0.0.1',
                 '--port', str(backend_port), '--no-access-log'], cwd=ROOT, stdout=backend_log, stderr=backend_log,
            )
            wait_ready(backend, backend_port)
            proxy, port = start_proxy(binary, backend_port, out, err)
            wait_ready(proxy, port)

            def check(case, path, status=200, outcome='Success', method='GET', body=None, headers=None, minimum_ms=None):
                target = path + ('&' if '?' in path else '?') + 'case=' + case
                expected[case] = (json.dumps(method), json.dumps(path.split('?', 1)[0]), status, outcome, minimum_ms)
                actual, response_headers, response_body = request(port, method, target, body, headers)
                assert_equal(actual, status, f'{case} wire status')
                return response_headers, response_body

            _, body = check('privacy', '/items/42?token=M4_QUERY_SECRET&%2574oken=M4_NESTED_SECRET&user=visible',
                            headers={'Authorization': 'Bearer M4_HEADER_SECRET'})
            received = json.loads(body)
            assert_equal(received['item_id'], 42, 'Route parameter forwarding')
            require(received['authorization_received'], 'Authorization header was not forwarded')
            assert_equal(received['raw_query'], 'token=M4_QUERY_SECRET&%2574oken=M4_NESTED_SECRET&user=visible&case=privacy', 'Query forwarding unchanged')
            payload = b'M4_BODY_SECRET\x00\xff'
            _, body = check('binary-post', '/echo', method='POST', body=payload)
            assert_equal(body, payload, 'Binary body forwarding')
            delayed_private_body(port)
            expected['private-slow-body'] = ('"POST"', '"/echo"', 200, 'Success', 350)
            check('private-error', '/status/503?token=M4_QUERY_SECRET&%2574oken=M4_NESTED_SECRET',
                  status=503, headers={'Authorization': 'Bearer M4_HEADER_SECRET'})
            _, body = check('head', '/health', method='HEAD')
            assert_equal(body, b'', 'HEAD framing')
            for status in (404, 500, 502, 503, 504):
                check(f'status-{status}', f'/status/{status}', status=status)
            check('slow', '/delay?milliseconds=400', minimum_ms=350)
            _, body = check('buffered-stream', '/stream?milliseconds=400', minimum_ms=350)
            assert_equal(body, b'first-last', 'Buffered upstream frames')
            check('request-rejected', '/echo', method='POST', body=b'x' * 4194305, status=413, outcome='Rejected')
            check('response-rejected', '/bytes/4194305', status=502, outcome='BackendFailure')
            # Different real operations run concurrently, retaining full timeout
            # durations while keeping the campaign's wall time reasonable.
            with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
                jobs = [pool.submit(check, 'header-timeout', '/delay?milliseconds=16000', 504, 'BackendTimeout', minimum_ms=14000),
                        pool.submit(check, 'body-timeout', '/stream?milliseconds=16000', 504, 'BackendTimeout', minimum_ms=14000),
                        pool.submit(client_body_timeout, port)]
                expected['client-timeout'] = ('"POST"', '"/echo"', 408, 'ClientTimeout', 14000)
                for job in jobs:
                    job.result()
            with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
                jobs = []
                for i in range(24):
                    status = [200, 404, 503][i % 3]
                    path = f'/items/{i}' if status == 200 else f'/status/{status}'
                    case = f'concurrent-{i}'
                    expected[case] = ('"GET"', json.dumps(path), status, 'Success', None)
                    jobs.append((status, pool.submit(request, port, 'GET', path + '?case=' + case)))
                for status, job in jobs:
                    assert_equal(job.result()[0], status, 'Concurrent forwarding')
            # A refused backend socket and a backend-returned 502/504 are
            # different evidence. Observe an outage and recovery in one proxy.
            cleanup(backend)
            backend = None
            check('backend-offline', '/health', status=502, outcome='BackendFailure')
            backend = subprocess.Popen(
                [sys.executable, '-m', 'uvicorn', 'examples.fastapi_backend:app', '--host', '127.0.0.1',
                 '--port', str(backend_port), '--no-access-log'], cwd=ROOT, stdout=backend_log, stderr=backend_log,
            )
            wait_ready(backend, backend_port)
            check('backend-recovered', '/health')
            stop_proxy(proxy)
        result = verify_capture(stdout, stderr, expected)
        text = stdout.read_text(encoding='ascii')
        for secret in ('M4_QUERY_SECRET', 'M4_NESTED_SECRET', 'M4_HEADER_SECRET', 'M4_BODY_SECRET', 'M4_ANOMALOUS_BODY_SECRET'):
            require(secret not in text, f'Private sentinel leaked: {secret}')
        events = [line for line in text.splitlines() if line.startswith('ANOMALY_V1 ')]
        require(all('/items/' not in line and 'case=' not in line for line in events), 'Anomaly target metadata leaked')
        result.update(checked_requests=len(expected), real_timeout_cases=3, concurrent_requests=24,
                      backend_restart='PASS', privacy='PASS', elapsed_seconds=round(time.monotonic() - started, 3))
        return result
    finally:
        cleanup(proxy)
        cleanup(backend)


def incomplete_request_shutdown(binary, output):
    proxy = None
    stdout, stderr = output / 'cancel-stdout.log', output / 'cancel-stderr.log'
    try:
        # No backend is required: an unfinished body must not be forwarded.
        with stdout.open('wb') as out, stderr.open('wb') as err:
            proxy, port = start_proxy(binary, available_port(), out, err)
            # Readiness cannot use HTTP 200 here, as the backend is absent.
            deadline = time.monotonic() + 10
            while True:
                require(proxy.poll() is None, 'Proxy exited before cancellation check')
                try:
                    client = socket.create_connection(('127.0.0.1', port), timeout=1)
                    break
                except OSError:
                    require(time.monotonic() < deadline, 'Proxy did not bind')
                    time.sleep(0.05)
            with client:
                client.sendall(b'POST /cancel HTTP/1.1\r\nHost: localhost\r\nContent-Length: 100\r\n\r\nx')
                client.settimeout(0.2)
                try:
                    premature = client.recv(1)
                    require(False, f'Unfinished body produced response/closure: {premature!r}')
                except socket.timeout:
                    pass
                start = time.monotonic()
                stop_proxy(proxy)
                elapsed = time.monotonic() - start
                require(elapsed < 7, 'Incomplete client held shutdown beyond bounded waits')
        assert_equal(stdout.read_bytes(), b'', 'Cancelled/incomplete request fabricated telemetry')
        assert_equal(summary(stderr, 'detection_stats')['evaluated'], '0', 'Cancellation classification')
        assert_equal(summary(stderr, 'anomaly_output')['state'], 'clean', 'Empty pipeline completion')
        return {'evaluated': 0, 'exit_code': 0, 'shutdown_seconds': round(elapsed, 3),
                'limitation': 'Incomplete/cancelled requests need not produce observations; this does not prove all emissions are covered.'}
    finally:
        cleanup(proxy)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target' / 'release' / ('exaprop.exe' if os.name == 'nt' else 'exaprop'))
    args = parser.parse_args()
    binary = args.binary.resolve()
    require(binary.is_file(), f'Build the release binary first: {binary}')
    output = ROOT / 'tmp' / 'm4' / f'{time.strftime("%Y%m%d-%H%M%S")}-{os.getpid()}'
    output.mkdir(parents=True, exist_ok=False)
    report = {'status': 'FAIL', 'binary': str(binary), 'platform': sys.platform, 'python': sys.version}
    try:
        report['fastapi_campaign'] = campaign(binary, output)
        report['incomplete_request_shutdown'] = incomplete_request_shutdown(binary, output)
        report['status'] = 'PASS'
    except Exception as error:
        report['error'] = str(error)
        raise
    finally:
        (output / 'result.json').write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
        print(f'{report["status"]}: {output}')


if __name__ == '__main__':
    main()
