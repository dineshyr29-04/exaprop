# ExaProp

**A local HTTP reverse proxy with bounded request observability, written in Rust.**

[![CI](https://github.com/dineshyr29-04/exaprop/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/dineshyr29-04/exaprop/actions/workflows/ci.yml)
[Apache-2.0](LICENSE)

ExaProp runs between your client and an HTTP backend. It forwards requests and emits structured observations of status, processing duration, and exchange outcome. It runs on your computer or a controlled network and needs no centrally hosted ExaProp service.

M3 implements HTTP observability. M4 anomaly detection is planned. ExaProp is an early-stage development and research tool, not a complete distributed observability platform or a hardened public-facing proxy.

[Quick start](#quick-start) · [Installation](#installation) · [CLI](#cli-usage) · [Observations](#observability) · [Limitations](#http-behavior-and-limitations) · [Development](#development) · [Documentation](#documentation)

## Current capabilities

- Forward HTTP/1.x traffic to one backend while preserving paths, queries, binary payloads, Host, and duplicate end-to-end headers.
- Validate Connection tokens, strip hop-by-hop fields, preserve Via chains, and append a fixed `exaprop` pseudonym.
- Buffer bodies within fixed limits and return explicit proxy errors for failures or unsupported features.
- Produce typed observations with conservative query-value redaction and measurable queue drops.
- Keep observation and diagnostic output off HTTP tasks using separate bounded, best-effort queues.
- Stop accepting connections on graceful shutdown and allow bounded connection and output draining.

The core library is in [core/](core/); the `exaprop` binary is in [cli/](cli/).

## Request flow

```text
Client -- HTTP --> ExaProp :8080 -- HTTP --> Backend :8081
       <-- response ------------ response --
                      |
                      +-- stdout: delivered observations
                      +-- stderr: operational diagnostics
```

Direct clients to the proxy's listen address. Requests sent straight to the backend bypass ExaProp. No application SDK is required. Responses are buffered; this is not a streaming transport.

## Quick start

Prerequisites: Rust installed through rustup with a native linker, Python 3 for the example backend, and curl. Run from the repository root in three terminals.

**1. Start the backend:**

```bash
python3 examples/echo_server.py
```

On Windows, use `python` or `py -3` when available. The backend listens on `127.0.0.1:8081`.

**2. Start ExaProp:**

```bash
cargo run --locked -p cli -- attach --listen 127.0.0.1:8080 --target 127.0.0.1:8081 --name echo
```

**3. Send requests:**

```bash
curl -i "http://127.0.0.1:8080/hello?page=2"
curl -i --data "service=checkout" "http://127.0.0.1:8080/echo"
```

GET returns `GET /hello?page=2` followed by a newline; POST echoes the submitted bytes. In Windows PowerShell, use `curl.exe` if `curl` is an alias. Press Ctrl+C in the proxy terminal to stop and drain output.

## Installation

The [toolchain file](rust-toolchain.toml) selects stable Rust with rustfmt and Clippy. It follows the stable channel rather than pinning an exact compiler; no separate minimum supported Rust version is declared. The workspace uses Cargo resolver 3.

```bash
cargo build --workspace --release --locked
cargo install --path cli --locked
exaprop --help
```

The build produces `target/release/exaprop` (`exaprop.exe` on Windows). Installation places the command in Cargo's bin directory; add `$HOME/.cargo/bin` or `%USERPROFILE%/.cargo/bin` to PATH to use it anywhere. Build a native binary for each OS and architecture. CI covers Linux and Windows; macOS is not in the CI matrix.

See the [usage guide](documents/EXAPROP_USAGE.md) for setup, FastAPI, Express, and troubleshooting.

## CLI usage

```bash
exaprop attach --listen 127.0.0.1:8080 --target 127.0.0.1:3000 --name orders
exaprop attach --help
```

| Option | Default | Purpose |
| --- | --- | --- |
| `--listen` | `127.0.0.1:8080` | Local bind address |
| `--target` | `127.0.0.1:8081` | Single HTTP backend address |
| `--name` | `backend-service` | Observation service label |

Addresses must be numeric socket addresses, such as `127.0.0.1:3000` or `[::1]:3000`, rather than URLs or DNS names. Keep listen and target addresses different. The client's Host is preserved for backend routing; ExaProp does not synthesize `X-Forwarded-*` identity headers.

## Observability

Delivered observations go to stdout; operational diagnostics go to stderr:

```text
[orders] GET /items?page=1 -> 200 (0.125ms) [Success] timestamp=1792627200.000000000
```

| Field | Meaning |
| --- | --- |
| Service | Label provided by `--name` |
| Method and target | HTTP method, visible path, and sanitized query |
| Status | Forwarded upstream status or constructed proxy error status |
| Duration | Monotonic handler processing time in milliseconds |
| Outcome | Completed exchange, upstream failure/timeout, client failure/timeout, or rejection |
| Timestamp | Handler-entry wall-clock time as Unix seconds and nanoseconds |

`Success` means the upstream exchange completed, including upstream HTTP 4xx/5xx responses. It does not mean application success. Duration includes request and response buffering and upstream work; it excludes request-header parsing, observation output, and delivery to the client. Parser-level rejections and cancelled handlers may have no observation.

The CLI observation queue holds 1,024 records. Diagnostics have a separate queue of 256 messages capped at 1,024 bytes each. Full or closed queues drop new entries without waiting. Writer failures and drain timeouts can lose accepted output. Summaries report drops and unconfirmed output when stderr remains usable; a blocked sink cannot reliably report its own failure. These queues are not durable storage. Library callers can read live observation and diagnostic counters without parsing output.

Bodies and headers are excluded from observations. Sensitive or ambiguous query names trigger value redaction, but paths, original parameter-name spelling, and unrecognized query values can still expose secrets. Treat output as potentially sensitive. See the [exact latency, privacy, and delivery contract](documents/M3_OBSERVABILITY.md).

## HTTP behavior and limitations

| Area | Current behavior |
| --- | --- |
| Transport | Plain HTTP/1.x to one numeric backend; no DNS targets, HTTPS, HTTP/2, CONNECT tunnels, or WebSockets |
| Buffering | Request and response bodies each limited to 4 MiB; no streaming/SSE semantics |
| Connections | At most 16 active clients, including idle keep-alive connections |
| Deadlines | 15-second header/connect stages and each body-frame wait; no total transaction deadline or universal downstream write deadline |
| Header filtering | Standard and Connection-nominated hop fields removed; malformed client Connection returns 400, malformed upstream Connection returns 502 |
| Via | Existing chains retained; append received protocol version and `exaprop`, without local addresses, ports, or service labels |
| Bodyless responses | Suppress content for HEAD, 204, 205, and 304; 204 omits Content-Length, 205 uses Content-Length: 0; Hyper may omit optional 304 length metadata |
| Unsupported requests | Upgrade/CONNECT and request trailers return 501; Expect returns 417 |
| Limits and failures | Oversized request: 413; client body timeout: 408; upstream failures, 101, oversized bodies, or trailers: 502; upstream timeout: 504 |
| Shutdown | Up to 2 seconds for connections, then 2 seconds each for observation and diagnostic output; failed output/draining returns nonzero |
| Security | No built-in authentication, public-edge hardening, or comprehensive secret detection |

The CLI handles Ctrl+C, Unix SIGTERM, and Windows Ctrl+Break. Forced termination or connection cancellation can lose observations. Resource limits do not establish a fixed total-memory ceiling. No published benchmark establishes proxy overhead. Use systems and traffic you own or are authorized to test.

## Development

Run the same Rust checks used in CI:

```bash
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --release --locked
cargo doc --workspace --no-deps --locked
```

CI denies documentation warnings as well. On Windows, [scripts/check.ps1](scripts/check.ps1) runs Rust verification, including debug builds and strict rustdoc. Linux and Windows CI also run the full FastAPI and HTTP review suites, including deliberate timeout and blocked-output scenarios. See [CONTRIBUTING.md](CONTRIBUTING.md#verification) for environment setup and exact smoke commands.

## Documentation

- [Practical usage: local HTTP, FastAPI, Express, and troubleshooting](documents/EXAPROP_USAGE.md)
- [M3 observation, privacy, latency, and shutdown contract](documents/M3_OBSERVABILITY.md)
- [Manual external test guide](documents/M3_OBSERVABILITY_TESTING.md)
- [M3 engineering review and historical local evidence](documents/M3_FINAL_ENGINEERING_REPORT.md)
- [Contributing and validation](CONTRIBUTING.md)
- [Security reporting policy](SECURITY.md)

Engineering reports describe their recorded checkout and environment; they are not proof that a later commit or another platform passed.

## Roadmap

| Milestone | Scope | Status |
| --- | --- | --- |
| M0 | Workspace and project foundation | Implemented |
| M1 | HTTP proxy engine and CLI | Implemented |
| M2 | HTTP correctness and bounded resource handling | Implemented |
| M3 | HTTP request observability | Implemented and merged |
| M4 | Anomaly detection | Planned; no detector implemented |
| Later work | Controlled fault experiments and cross-service propagation analysis | Planned; scope and scheduling remain open |

## Contributing and security

Focused code, documentation, test, and research contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) and the [Code of Conduct](CODE_OF_CONDUCT.md). Discuss substantial design changes before implementation and distinguish observed evidence from derived anomalies or diagnostic hypotheses.

Do not publish vulnerabilities, credentials, private URLs, authorization headers, or traffic captures in issues. Follow [SECURITY.md](SECURITY.md); private vulnerability reporting currently requires maintainer setup.

## License

ExaProp is licensed under [Apache-2.0](LICENSE). Existing license terms and attribution are preserved.
