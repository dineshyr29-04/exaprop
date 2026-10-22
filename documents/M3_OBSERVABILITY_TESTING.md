# M3 Observability: Implementation and External Test Guide

Prepared: 22 October 2026

Branch: `m3-observability`

Purpose: explain the current M3 implementation and provide reproducible tests against the compiled application.

## 1. Scope and verification status

M3 currently adds structured HTTP request observations to the bounded HTTP/1.1 proxy. It does not yet implement anomaly detection, durable telemetry storage, cross-service tracing, or a dashboard.

**Already verified locally:** all 46 Rust tests, formatting, compilation, Clippy with warnings denied, debug/release builds, documentation checks, CLI help, invalid address handling, and occupied-port startup handling. The complete development check script also passed with the normal user-level Rust installation.

**Planned external tests:** the black-box procedures in this guide. They have not been executed as a complete campaign. Do not mark a case passed merely because a related Rust regression passed. Hosted Linux/Windows CI, performance measurements, dependency advisory scanning, and fuzzing also require their own results.

See the [verification report](../docs/m3-observability-review.md) for the earlier run's evidence, fixes, and limitations. See the [README](../README.md) for the current product contract.

## 2. What M3 implements

| Component | Responsibility |
| --- | --- |
| [Observation model](../core/src/observation.rs) | Timestamp, service name, method, path, sanitized query, status, duration, and outcome |
| [Proxy integration](../core/src/proxy.rs) | Create records for handled requests; classify failures; attempt nonblocking delivery; count failed sends |
| [CLI](../cli/src/main.rs) | `--name` service label; bounded queue; escaped text output on a dedicated thread; bounded drain at shutdown |
| [Development checks](../scripts/check.ps1) | Formatting, compilation, tests, linting, builds, and documentation |
| [CI workflow](../.github/workflows/ci.yml) | Configure the checks on Linux and Windows |

### Observation semantics

- Timestamp: wall-clock time when the parsed request enters the handler, printed as Unix seconds and nanoseconds.
- Duration: monotonic elapsed processing time through response buffering. It excludes sending the buffered response to the client.
- `Success`: the upstream exchange completed, including an upstream `4xx` or `5xx`. Inspect the HTTP status separately for application failure.
- `BackendFailure`: upstream connection, body read, or response constraint failed.
- `BackendTimeout`: upstream request or response frame read timed out.
- `ClientFailure`: a client request body failed after its headers were parsed.
- `ClientTimeout`: the next client body frame did not arrive in time.
- `Rejected`: the proxy refused a feature or request payload under its current contract.

Example output, with illustrative timing and timestamp:

```text
[orders] GET /inspect?token=[REDACTED]&page=2 -> 200 (0.125ms) [Success] timestamp=1792627200.000000000
```

### Boundaries that tests must respect

| Property | Current contract |
| --- | --- |
| Protocol | Plain HTTP/1.1; one backend; buffered bodies |
| Connections | At most 16 active client connections, including keep-alive connections |
| Body size | At most 4 MiB for each request and response |
| Body timeout | 15 seconds waiting for each next frame; no total transaction deadline |
| Observation queue | CLI capacity of 1,024 records; full or closed channels drop records without waiting |
| Drop reporting | Failed channel-send count printed at proxy shutdown; not a count of every possible lost transaction |
| Shutdown | Proxy connection grace of up to 2 seconds, then output completion wait of up to 2 seconds |
| Privacy | No body/header fields in records; heuristic query-value redaction; paths and unrecognized query values remain visible |
| Missing records | Parser-level rejections, cancelled handlers, queue drops, and failed/blocked output can leave no delivered record |

Do not use record counts as a complete traffic ledger. Do not interpret a proxy-selected status as proof that the client received it.

## 3. Tools and test environment

Required tools are Rust/Cargo, Python 3, PowerShell, and `curl.exe`. The external checks use only standard Python libraries; no third-party Python packages are required. All endpoints below run on localhost and use synthetic test data.

Open a new terminal after installing Rust, then run from the repository root:

```powershell
git status --short --branch
rustup show
rustc --version
cargo --version
py -3 --version
curl.exe --version
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/check.ps1
```

If your Python installation uses `python` instead of the launcher, replace `py -3` throughout this guide with `python`.

**Current-machine prerequisite:** when preparing this guide, `py -3` reported `No installed Python found!`. The launcher alone is not a Python runtime. Install a Python 3 interpreter, or replace the commands with the path to an available interpreter, before starting the Python fixtures. This does not block the Rust development checks. The Python snippets were syntax-checked with the bundled workspace interpreter; the external campaign remains unrun.

The branch should be `m3-observability`. Record existing changes; do not reset them to perform a test. The check script should finish with `All development checks passed.`

### Collect evidence

Choose a new run directory each time. The examples use `run-001`; change it for later campaigns. Set the same `$run` in each PowerShell terminal that uses it.

```powershell
$run = 'tmp/m3-evidence/run-001'
New-Item -ItemType Directory -Path $run -Force | Out-Null
git rev-parse HEAD | Set-Content "$run/commit.txt"
git status --short --branch | Set-Content "$run/worktree.txt"
rustc -Vv | Set-Content "$run/rustc.txt"
cargo --version | Set-Content "$run/cargo.txt"
curl.exe --version | Set-Content "$run/curl.txt"
Get-Date -Format o | Set-Content "$run/start-time.txt"
```

Keep status/headers, response bodies or hashes where relevant, stdout observations, stderr diagnostics, process exit codes, tool versions, OS, hardware, and the final checklist. If the tree is dirty, the commit alone does not identify the tested code: retain the diff and any new source/configuration files used for the run.

## 4. Start a controllable backend

The repository's basic echo server is sufficient for ordinary GET/POST checks. For status, delay, truncation, and size cases, save this fixture as `tmp/m3-backend.py`:

```python
import json
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit


class Backend(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def send_payload(self, status, body):
        self.send_response(status)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)
            self.wfile.flush()

    def do_HEAD(self):
        self.send_payload(200, b"head-metadata")

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        self.send_payload(200, body)

    def do_GET(self):
        path = urlsplit(self.path).path
        if path.startswith("/status/"):
            self.send_payload(int(path.rsplit("/", 1)[1]), b"synthetic-status")
        elif path.startswith("/delay/"):
            time.sleep(int(path.rsplit("/", 1)[1]) / 1000)
            self.send_payload(200, b"delayed")
        elif path.startswith("/bytes/"):
            self.send_payload(200, b"x" * int(path.rsplit("/", 1)[1]))
        elif path == "/stream-stall":
            self.send_response(200)
            self.send_header("Content-Length", "2")
            self.end_headers()
            self.wfile.write(b"x")
            self.wfile.flush()
            time.sleep(16)
            self.wfile.write(b"y")
        elif path == "/short-body":
            self.send_response(200)
            self.send_header("Content-Length", "10")
            self.end_headers()
            self.wfile.write(b"x")
            self.wfile.flush()
            self.close_connection = True
        elif path == "/reset":
            self.close_connection = True
        elif path == "/no-content":
            self.send_response(204)
            self.end_headers()
        else:
            body = json.dumps({
                "method": self.command,
                "target": self.path,
                "authorization_received": "Authorization" in self.headers,
            }).encode()
            self.send_payload(200, body)


if __name__ == "__main__":
    print("M3 fixture listening on 127.0.0.1:8081", flush=True)
    ThreadingHTTPServer(("127.0.0.1", 8081), Backend).serve_forever()
```

In terminal A:

```powershell
py -3 tmp/m3-backend.py
```

Use only the documented fixture endpoints. Deliberately timed-out requests can produce a backend broken-pipe diagnostic when it eventually tries to respond; distinguish that expected fixture effect from a proxy crash.

In terminal B, start the release binary and capture its two output streams:

```powershell
$run = 'tmp/m3-evidence/run-001'
& ./target/release/exaprop.exe attach --name orders --listen 127.0.0.1:8080 --target 127.0.0.1:8081 1> "$run/observations.log" 2> "$run/proxy-stderr.log"
```

Keep it running in the foreground. Perform requests from terminal C. At the end, press Ctrl+C in terminal B, wait for exit, and inspect both logs. Reopen captured files if your viewer does not refresh while the process is writing.

## 5. External HTTP and observation checks

In terminal C, set `$run` to the same directory. Run cases sequentially first. Use `curl.exe --max-time 25` so the 15-second proxy timeouts can complete before the client gives up.

### E01: successful forwarding and observation identity

```powershell
$run = 'tmp/m3-evidence/run-001'
curl.exe -sS --max-time 25 -D "$run/e01.headers.txt" -o "$run/e01.body.txt" "http://127.0.0.1:8080/inspect?page=2&case=e01"
Get-Content "$run/e01.body.txt"
```

Expected: HTTP 200; JSON reports the original target; an observation has service `orders`, method `GET`, path `/inspect`, status 200, outcome `Success`, a timestamp, and nonnegative duration. Do not require an exact duration.

### E02: POST integrity and excluded bodies/headers

```powershell
curl.exe -sS --max-time 25 -D "$run/e02.headers.txt" -o "$run/e02.body.txt" -H "Authorization: Bearer M3_HEADER_SENTINEL" --data-binary "M3_BODY_SENTINEL" "http://127.0.0.1:8080/echo?case=e02"
Get-Content "$run/e02.body.txt"
```

Expected: HTTP 200; response body equals `M3_BODY_SENTINEL`; observation method is POST. Neither sentinel belongs in `observations.log`. The fixture echoes the body, so exclude the response evidence file from the observation-privacy assertion. Header forwarding itself can be checked with a GET to `/inspect` using the same Authorization header: `authorization_received` should be true.

### E03: redaction without changing the forwarded query

```powershell
curl.exe -sS --max-time 25 -o "$run/e03.body.txt" "http://127.0.0.1:8080/inspect?token=M3_SECRET_A&%74oken=M3_SECRET_B&API%5fKEY=M3_SECRET_C&token=M3_SECRET_D&page=2&case=e03"
Get-Content "$run/e03.body.txt"
```

Expected: backend JSON retains the original query and fake values. The observation replaces all four sensitive values with `[REDACTED]` while retaining `page=2` and `case=e03`. After shutdown:

```powershell
Select-String -Path "$run/observations.log" -SimpleMatch -Pattern 'M3_SECRET_A','M3_SECRET_B','M3_SECRET_C','M3_SECRET_D','M3_BODY_SENTINEL','M3_HEADER_SENTINEL'
```

Expected: no matches. Also check no-query, empty-query, mixed-case keys, valueless sensitive keys, and malformed encoded names such as `%GG=synthetic`; malformed names should have their values redacted if the request reaches the handler. A parser rejection is a separate outcome from successful sanitization.

### E04: HTTP status versus transport outcome

```powershell
curl.exe -i --max-time 25 "http://127.0.0.1:8080/status/404?case=e04a"
curl.exe -i --max-time 25 "http://127.0.0.1:8080/status/500?case=e04b"
curl.exe -i --max-time 25 "http://127.0.0.1:8080/status/502?case=e04c"
curl.exe -i --max-time 25 "http://127.0.0.1:8080/reset?case=e04d"
curl.exe -i --max-time 25 "http://127.0.0.1:8080/short-body?case=e04e"
```

Expected: backend 404/500/502 responses retain their status and have outcome `Success`. `/reset` and `/short-body` produce proxy HTTP 502 with `BackendFailure`. A backend-generated 502 must remain distinguishable from a proxy-generated 502.

### E05: upstream request and body timeouts

```powershell
curl.exe -i --max-time 25 "http://127.0.0.1:8080/delay/16000?case=e05a"
curl.exe -i --max-time 25 "http://127.0.0.1:8080/stream-stall?case=e05b"
curl.exe -i --max-time 25 "http://127.0.0.1:8080/delay/200?case=e05c"
```

Expected: the first two return HTTP 504 with `BackendTimeout` around the applicable 15-second wait, allowing scheduling tolerance. The short delay returns 200/`Success` and duration should include roughly the backend delay. `/stream-stall` must return a proxy error rather than exposing its buffered partial upstream body as a successful response.

### E06: request/response size boundaries

```powershell
[IO.File]::WriteAllBytes((Join-Path $PWD 'tmp/m3-exact.bin'), [byte[]]::new(4194304))
[IO.File]::WriteAllBytes((Join-Path $PWD 'tmp/m3-over.bin'), [byte[]]::new(4194305))
curl.exe -sS --max-time 25 -D "$run/e06a.headers.txt" -o "$run/e06a.body.bin" -H "Expect:" --data-binary "@tmp/m3-exact.bin" "http://127.0.0.1:8080/echo?case=e06a"
Get-FileHash tmp/m3-exact.bin
Get-FileHash "$run/e06a.body.bin"
curl.exe -i --max-time 25 -H "Expect:" --data-binary "@tmp/m3-over.bin" "http://127.0.0.1:8080/echo?case=e06b"
curl.exe -sS --max-time 25 -D "$run/e06c.headers.txt" -o "$run/e06c.body.bin" "http://127.0.0.1:8080/bytes/4194304?case=e06c"
curl.exe -i --max-time 25 "http://127.0.0.1:8080/bytes/4194305?case=e06d"
```

Expected: exact-size echo succeeds with matching hashes; oversized request is 413/`Rejected`; exact-size response is 200/`Success` with 4,194,304 bytes; oversized response is 502/`BackendFailure`. Clear `Expect` for these size tests so a client's automatic `100-continue` behavior does not turn the test into an Expect-header rejection.

### E07: supported bodyless responses and explicit rejections

```powershell
curl.exe -I --max-time 25 "http://127.0.0.1:8080/inspect?case=e07a"
curl.exe -i --max-time 25 "http://127.0.0.1:8080/no-content?case=e07b"
curl.exe -i --max-time 25 -H "Expect: 100-continue" --data "x" "http://127.0.0.1:8080/echo?case=e07c"
curl.exe -i --max-time 25 -H "Upgrade: websocket" "http://127.0.0.1:8080/inspect?case=e07d"
curl.exe -i --max-time 25 -H "Trailer: X-Checksum" --data "x" "http://127.0.0.1:8080/echo?case=e07e"
```

Expected: HEAD has no body; 204 has no body and no Content-Length. Expect returns 417/`Rejected`; upgrade and declared request trailers return 501/`Rejected`. Use the raw-client helper below for malformed framing, actual trailers, and duplicate Connection headers.

## 6. Raw-client failure checks

Save the following as `tmp/m3-raw-client.py`:

```python
import socket
import sys

mode = sys.argv[1]
requests = {
    "malformed": b"not-http\r\n\r\n",
    "duplicate-upgrade": (
        b"GET /inspect?case=raw-upgrade HTTP/1.1\r\nHost: localhost\r\n"
        b"Connection: keep-alive\r\nConnection: upgrade\r\n\r\n"
    ),
    "client-timeout": (
        b"POST /echo?case=raw-timeout HTTP/1.1\r\nHost: localhost\r\n"
        b"Content-Length: 10\r\n\r\nx"
    ),
    "client-eof": (
        b"POST /echo?case=raw-eof HTTP/1.1\r\nHost: localhost\r\n"
        b"Content-Length: 10\r\n\r\nx"
    ),
    "actual-trailers": (
        b"POST /echo?case=raw-trailers HTTP/1.1\r\nHost: localhost\r\n"
        b"Transfer-Encoding: chunked\r\n\r\n1\r\nx\r\n0\r\nX-Checksum: fake\r\n\r\n"
    ),
}
with socket.create_connection(("127.0.0.1", 8080), timeout=3) as connection:
    connection.settimeout(25)
    connection.sendall(requests[mode])
    if mode == "client-eof":
        connection.shutdown(socket.SHUT_WR)
    response = connection.recv(8192)
    print(response.decode("iso-8859-1"))
```

Run each mode separately and save its output:

```powershell
py -3 tmp/m3-raw-client.py malformed
py -3 tmp/m3-raw-client.py duplicate-upgrade
py -3 tmp/m3-raw-client.py client-timeout
py -3 tmp/m3-raw-client.py client-eof
py -3 tmp/m3-raw-client.py actual-trailers
```

| Case | Expected external behavior | Observation expectation |
| --- | --- | --- |
| Malformed request | HTTP 400 for this payload; process remains usable | No handler observation required for parser rejection |
| Upgrade token in a later Connection header | HTTP 501 | `Rejected` |
| Partial body held open | HTTP 408 after the frame-read timeout | `ClientTimeout` |
| Premature request EOF | HTTP 400 if the handler completes; connection can instead be terminated | `ClientFailure` if emitted; cancellation may prevent a record |
| Actual chunk trailers | HTTP 501 when parsed and delivered as trailers | `Rejected`; record a parser-level refusal separately if the HTTP stack rejects earlier |

After each failure, make a fresh ordinary GET. It must still return 200. The helper prints only the first received response fragment; it is a status/failure probe, not a complete HTTP framing validator.

## 7. Concurrency and performance measurements

Save this dependency-free driver as `tmp/m3-load.py`:

```python
import argparse
import concurrent.futures
import json
import time
import urllib.error
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument("url")
parser.add_argument("--requests", type=int, default=200)
parser.add_argument("--workers", type=int, default=8)
args = parser.parse_args()

def request_one(_):
    start = time.perf_counter()
    try:
        with urllib.request.urlopen(args.url, timeout=30) as response:
            response.read()
            status = response.status
    except urllib.error.HTTPError as error:
        error.read()
        error.close()
        status = error.code
    except (OSError, urllib.error.URLError):
        status = 0
    return (time.perf_counter() - start) * 1000, status

start = time.perf_counter()
with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
    results = list(pool.map(request_one, range(args.requests)))
elapsed = time.perf_counter() - start
latencies = sorted(duration for duration, _ in results)
def percentile(fraction):
    return latencies[max(0, min(len(latencies) - 1, int(len(latencies) * fraction) - 1))]
print(json.dumps({
    "requests": len(results), "workers": args.workers,
    "http_200": sum(status == 200 for _, status in results),
    "failed_or_non_200": sum(status != 200 for _, status in results),
    "elapsed_s": elapsed, "requests_per_s": len(results) / elapsed,
    "p50_ms": percentile(0.50), "p95_ms": percentile(0.95),
    "p99_ms": percentile(0.99),
}, indent=2))
```

Use positive request and worker counts. Start with these runs:

```powershell
py -3 tmp/m3-load.py "http://127.0.0.1:8081/bytes/1024?case=load-direct" --requests 200 --workers 8 > "$run/load-direct.json"
py -3 tmp/m3-load.py "http://127.0.0.1:8080/bytes/1024?case=load-proxy" --requests 200 --workers 8 > "$run/load-proxy.json"
```

Warm up both paths, repeat at least three measured trials, and compare medians across trials. Then repeat with 1, 16, and 32 workers. Keep backend, payload, request count, logging sink, and machine conditions consistent. Record p50/p95/p99 latency, throughput, error count, and any dropped-record report.

These are whole-request client timings and include connection creation, scheduling, Python fixture overhead, and proxy output costs. They do not isolate proxy CPU time or prove sub-millisecond overhead. Counts above the 16-connection limit exercise waiting/backpressure; do not expect more than 16 active proxy connections.

Use Task Manager to record the proxy process's CPU and working set during a longer bounded run. Check for memory that continues growing across repeated comparable runs, and verify the process becomes responsive again after the load stops. A normal allocator high-water mark alone is not proof of a leak. Establish numeric performance budgets from measured baselines rather than inventing thresholds.

For a longer soak, run bounded batches for a documented duration and check memory, failures, responsiveness, and shutdown. The small Python fixture and this client are smoke/measurement tools, not a production-capacity certification harness.

## 8. Queue, output, and shutdown experiments

| Experiment | Procedure | Acceptance/evidence |
| --- | --- | --- |
| Normal drain | Finish known sequential requests, then press Ctrl+C with a writable stdout file | All expected completed, delivered records present; no duplicate case IDs; process exits cleanly |
| Queue saturation | In a dedicated run, launch the binary from a subprocess harness with stdout piped but deliberately unread; generate bounded traffic, then release or close the pipe | Forwarding remains responsive; queue memory stays bounded; record any dropped-send count at shutdown |
| Broken output sink | Close the stdout read end while traffic is active, then stop the proxy | No panic; fallible output error is reported; proxy handles later requests while it remains running |
| Slow/blocked output at exit | Keep stdout unread, then request normal Ctrl+C shutdown using a platform-appropriate signal harness | Output drain warning may appear; no unlimited wait caused by the output thread |
| Active request at exit | Start a delayed request, then press Ctrl+C before completion | New accepts stop; remaining connections are cancelled after the grace; missing cancellation observations are documented |
| Terminal control escaping | Start with a synthetic service name containing a newline or escape character and send a GET | Record stays on one physical line; control characters are escaped rather than executed |
| Backend recovery | Stop terminal A, send a request, restart A, send another | Failure returns proxy 502; later successful request returns 200 without restarting the proxy |

Queue saturation is not guaranteed merely by sending 1,025 requests: the consumer may drain faster than the producer. Control the sink and record that saturation actually occurred. The automated suite already tests full/closed channels deterministically; this external experiment tests OS pipes and the compiled process.

For the terminal-control test, PowerShell can construct a fake name with ``$label = "orders`nline$([char]27)[31m"``; start `attach --name $label` in a separate run and inspect the captured file.

For shutdown timing, measure the proxy grace and output wait separately where possible. They can add up to roughly four seconds plus scheduling/process-exit overhead. A deliberately blocked stderr reader can stall synchronous operational logging, so keep stderr draining when isolating stdout behavior. That stderr limitation is distinct from the bounded stdout drain.

## 9. Independent checks before merging or releasing

| Check | What to do | Required evidence |
| --- | --- | --- |
| Hosted CI | Publish the branch through the normal review process; inspect both configured OS jobs | Run URL, tested commit, job conclusions, and failure logs if any |
| Cross-platform behavior | Repeat core HTTP, privacy, timeout, and shutdown cases on Linux and Windows | OS/toolchain/target and per-case results; Windows GNU and hosted Windows MSVC are different targets |
| Dependency advisory audit | Scan the exact Cargo.lock against a current advisory database using your selected advisory scanner | Scanner version, database date/revision, findings, affected dependencies, and resolution |
| Malformed-input/fuzz testing | Add a bounded, repeatable campaign for request framing, query sanitization, duplicate headers, and body boundaries | Seed corpus, limits, reproduction input, crashes/hangs, and regression cases for fixes |
| Wire-level verification | Capture local traffic when diagnosing framing or forwarding concerns | Request/response bytes proving body integrity and removal of transport-only headers |
| Performance and soak | Use the controlled comparisons above, then a dedicated harness if stronger capacity claims are needed | Hardware, workload, logging settings, repetitions, latency/throughput/memory results |

The advisory scanner, fuzz harness, and packet capture are not installed or executed by this document. Their setup depends on the selected tool and platform; they are additional campaigns, not results implied by a green Rust test suite.

## 10. Acceptance checklist and result template

Complete the applicable items before calling the implemented M3 observation scope validated:

- [ ] Fresh development check script passes on the exact tested checkout.
- [ ] Compiled binary works through the external GET/POST and status tests.
- [ ] Sensitive synthetic query values and excluded body/header sentinels do not appear in observation output.
- [ ] Backend receives the original query and payload where forwarding is supported.
- [ ] Backend errors, proxy errors, timeouts, and explicit rejections have the expected distinctions.
- [ ] Size boundaries and recovery after failure behave as documented.
- [ ] Concurrent traffic produces independent records without corrupting service/path tags.
- [ ] Normal shutdown drains delivered records; saturation and cancellation losses are described accurately.
- [ ] Output and pipe failures do not produce a panic or an unexplained permanent proxy stall.
- [ ] CI results, platform coverage, and any deferred campaigns are recorded.
- [ ] No anomaly detection, durable export, or cross-service causality claim is made for this implementation.

Use `PASS`, `FAIL`, `BLOCKED`, or `NOT RUN` for every case. A blocked prerequisite is not a pass. A documented unsupported feature is not a failed implementation when it returns the promised rejection.

```text
M3 external verification run
Date/time and timezone:
Tester:
Commit and working-tree diff/new files:
OS / Rust version / target:
Binary profile and command:
Fixture version/file hash:
Evidence directory:

Case ID | Status | Actual HTTP/outcome | Evidence file | Notes
E01     |        |                     |               |
E02     |        |                     |               |
E03     |        |                     |               |
E04     |        |                     |               |
E05     |        |                     |               |
E06     |        |                     |               |
E07     |        |                     |               |
Raw-client cases:
Concurrency/performance trials:
Queue/output/shutdown experiments:
Hosted CI and cross-platform checks:
Advisory/fuzz/wire-level campaigns:

Failures and reproducible inputs:
Deferred checks and reasons:
Known limitations accepted for this milestone:
Decision: ready for implemented M3 scope / needs fixes / insufficient evidence
```

Preserve the evidence before changing code, fix failures in focused changes, and rerun affected cases plus the full development checks. New features such as anomaly detection need their own acceptance criteria and tests.
