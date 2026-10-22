# ExaProp practical HTTP usage guide

ExaProp M3 forwards plain HTTP to one backend and prints request observations. Direct clients to its listening address. The backend needs no ExaProp SDK. Use local development environments or controlled networks with systems you own.

## Build and install

Install Rust through rustup, including your platform's native linker. From the repository root:

```text
cargo build --release
cargo install --path cli --locked
exaprop --help
exaprop attach --help
```

Cargo installs the CLI into `$HOME/.cargo/bin` on Linux/macOS or `%USERPROFILE%\.cargo\bin` on Windows. Put that directory on your user PATH and open a new terminal to use `exaprop` from any directory. Installation builds the current checkout; rerun it with `--force` when deliberately replacing an installed version. To use the repository build directly, run `./target/release/exaprop` or `.\target\release\exaprop.exe`.

Build separately for each operating system and architecture. A Windows executable does not run natively on Linux. Linux and Windows have CI jobs; see the [final report](M3_FINAL_ENGINEERING_REPORT.md) for which platforms were actually executed in the latest local review. macOS is not part of that CI matrix.

## Start with a local HTTP backend

Use three terminals. In terminal 1, from the repository root:

```text
python examples/echo_server.py
```

Use `python3` on systems where that is the Python command. The example listens on `127.0.0.1:8081`.

In terminal 2, start the proxy:

```text
exaprop attach --listen 127.0.0.1:8080 --target 127.0.0.1:8081 --name echo
```

In terminal 3:

```text
curl -i "http://127.0.0.1:8080/hello?page=2"
curl -i --data "service=checkout" "http://127.0.0.1:8080/echo"
curl -I "http://127.0.0.1:8080/health"
```

In Windows PowerShell, use `curl.exe` if `curl` is an alias. The GET body is `GET /hello?page=2` followed by a newline; POST echoes the bytes. Requests sent directly to port 8081 bypass ExaProp. Stop the proxy with Ctrl+C and let it finish draining.

For another existing HTTP backend, set `--target` to its numeric IP and port. For example, a backend on port 3000 uses `--target 127.0.0.1:3000`. Addresses must be socket addresses, not URLs or DNS names. IPv6 uses brackets, for example `[::1]:3000`. Keep listen and target different and never target the proxy itself.

## FastAPI

Create a Python 3.12 environment from the repository root:

```text
python -m venv .venv
```

Windows PowerShell:

```powershell
.\.venv\Scripts\python.exe -m pip install -r examples/requirements-smoke.txt
.\.venv\Scripts\python.exe -m uvicorn examples.fastapi_backend:app --host 127.0.0.1 --port 8081
```

Linux/macOS:

```bash
.venv/bin/python -m pip install -r examples/requirements-smoke.txt
.venv/bin/python -m uvicorn examples.fastapi_backend:app --host 127.0.0.1 --port 8081
```

Start ExaProp in another terminal with the same command as above, optionally using `--name fastapi`. Check `/health`, `/items/42?page=2`, and POST `/echo` through port 8080. For your own app, replace `examples.fastapi_backend:app` with its module and application name. FastAPI's upstream HTTP 404/500 responses are forwarded as completed exchanges; they do not indicate proxy transport failure.

To repeat the automated real-backend checks, use the environment's Python:

```text
python scripts/smoke_fastapi.py --include-timeouts
python scripts/smoke_http_review.py
```

The first deliberately waits for two 15-second timeouts. The second includes blocked logging destinations, so a temporary lack of diagnostics is expected. Both own their child processes and save evidence under `tmp/`. The manual [test guide](M3_OBSERVABILITY_TESTING.md) covers additional experiments.

## Express

For an existing Express app listening on plain HTTP port 3000:

```text
exaprop attach --listen 127.0.0.1:8080 --target 127.0.0.1:3000 --name express
curl -i "http://127.0.0.1:8080/health"
```

For a minimal example, create a separate demo directory, run `npm init -y` and `npm install express`, and save `server.cjs` there:

```javascript
const express = require('express');
const app = express();
app.use(express.json());
app.get('/health', (_req, res) => res.json({ ok: true }));
app.post('/echo', (req, res) => res.json(req.body));
app.listen(3000, '127.0.0.1');
```

Start it with `node server.cjs`, then run the proxy command above. To POST JSON without shell-quoting differences, save `{"service":"checkout"}` in `payload.json` and use:

```text
curl -i -H "Content-Type: application/json" --data-binary "@payload.json" "http://127.0.0.1:8080/echo"
```

This Express recipe was documented, not executed in the latest local review; the automated real framework test used FastAPI. Express body-size middleware can reject a request before ExaProp's 4 MiB limit. The client's Host is preserved, so configure any backend host allowlist accordingly. ExaProp does not synthesize `X-Forwarded-*` identity headers; do not enable framework proxy trust just because you added ExaProp.

## Read observations and diagnostics

Stdout contains delivered observations; stderr contains best-effort operational diagnostics. Example:

```text
[echo] GET /hello?page=2 -> 200 (0.125ms) [Success] timestamp=1792627200.000000000
```

`Success` means a completed upstream exchange, including upstream application errors. `BackendFailure` means a proxy-detected upstream failure, `BackendTimeout` a timed-out upstream operation, and `Rejected` a handled request rejection. See the typed [outcomes](../core/src/observation.rs) for all variants. Duration is handler processing time through buffering/response construction; it excludes header parsing, observation output, and delivery to the client. It is not end-to-end client latency.

You can redirect output to separate files, using paths appropriate for your shell:

```text
exaprop attach --name orders > observations.log 2> proxy-stderr.log
```

Observations use a bounded 1,024-record queue; diagnostics use a separate 256-message queue with messages capped at 1,024 bytes. Full or closed queues drop new entries without delaying HTTP handling. Diagnostic sink failure can also abandon accepted entries. Final summaries report accepted, written, dropped, and unconfirmed output when stderr remains usable. A blocked stderr cannot deliver its own warning; library counters are available without I/O. Output drain or writer failure gives a nonzero exit code. Counters and queues are not durable storage.

Bodies and headers are excluded from observations. Sensitive/ambiguous query names are filtered, but paths, original query-name spelling, and values under unrecognized names may contain secrets. Treat logs as potentially sensitive. See the [exact observation contract](M3_OBSERVABILITY.md).

## Current limits

| Area | M3 behavior |
| --- | --- |
| Transport | Plain HTTP/1.x; one numeric backend address; no DNS targets or HTTPS termination/upstream TLS |
| Unsupported protocols | No HTTP/2, CONNECT tunnels, WebSockets, or protocol upgrades; upstream 101 returns 502 |
| Buffering | Full request and response buffering, each at most 4 MiB; no streaming/SSE behavior |
| Concurrency | 16 active client connections, including idle keep-alive connections |
| Deadlines | 15 seconds for configured header/connect stages and each body-frame wait; no total transaction deadline |
| Unsupported fields | Expect rejected with 417; request trailers/upgrade requests with 501; upstream trailers with 502 |
| Header handling | Malformed Connection tokens: client 400, upstream 502; strip standard/nominated hop fields; preserve duplicate end-to-end headers |
| Via | Append a fixed `exaprop` pseudonym while preserving prior chains; no local infrastructure details added |
| HTTP 205 | No content; explicit Content-Length: 0; no Transfer-Encoding |
| Shutdown | Stop accepting, up to 2 seconds for connections, then up to 2 seconds each for observation and diagnostic output |
| Deployment | No built-in authentication, service registration, durable telemetry, or public-edge deployment hardening |
| Future scope | No fault injection, anomaly inference, or propagation analysis implemented here |

## Troubleshooting

| Symptom | Practical check |
| --- | --- |
| `exaprop` not found | Confirm `cargo install --path cli --locked` succeeded, add Cargo's bin directory to PATH, and reopen the terminal |
| Cannot bind listener | Choose an unused local port and valid interface address; check the OS error in diagnostics |
| Invalid target argument | Use `127.0.0.1:8081`, not `http://localhost:8081`; use brackets for IPv6 |
| 502 | Call the backend directly; inspect its logs; check that it is HTTP rather than TLS, within body limits, and not sending 101, malformed Connection, or trailers |
| 504 | Check backend response/header or body-frame stalls; distinguish this from an upstream application's own 504 observation |
| 400 | Check client HTTP syntax and Connection tokens; parser-level rejection may produce no observation |
| 413 or backend body rejection | Check ExaProp's 4 MiB limit and the framework's independent parser limits |
| Connections seem stuck | Check idle keep-alive clients against the 16-connection limit, backend health, and frame-by-frame timeouts |
| Missing observation lines | Inspect full/closed queue counters and unconfirmed output; check the stdout consumer; parser errors and cancelled handlers may have no record |
| Missing diagnostics | Check the stderr consumer; full queues drop new messages; closed/broken sinks stop delivery; counters are available to library callers |
| Exit code 1 after Ctrl+C | Inspect output/drain errors where available; blocked stdout/stderr intentionally causes bounded unsuccessful draining |
| Browser real-time app fails | Streaming/SSE and WebSockets require capabilities beyond this HTTP-buffering milestone |

For the exact reviewed source, regression names, actual results, and remaining risks, use the [M3 final engineering report](M3_FINAL_ENGINEERING_REPORT.md).
