# ExaProp

**A local-first foundation for resilience observability and controlled failure experiments.**

[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![Rust Edition: 2021](https://img.shields.io/badge/Rust_Edition-2021-orange.svg)](core/Cargo.toml)
[![Status: Early Development](https://img.shields.io/badge/Status-Early_Development-yellow.svg)](#project-status-and-roadmap)

ExaProp is a Rust-based HTTP reverse proxy being developed into a tool for studying how failures propagate through distributed services. Its intended workflow connects traffic observation, controlled fault injection, and measurable resilience improvements.

The current implementation provides a bounded HTTP/1.1 proxy and a command-line interface. Observability, anomaly detection, and fault injection are planned capabilities.

[Quickstart](#quickstart) · [Capabilities](#current-capabilities) · [Roadmap](#project-status-and-roadmap) · [Development](#development) · [Contributing](#contributing)

## Why ExaProp?

A slow or unavailable dependency can trigger retries, exhaust connection pools, and degrade other services. Investigating that behavior requires reproducible experiments and evidence that connects an observed failure to its effects.

ExaProp starts with a small interception layer placed between a client and an HTTP backend. Applications do not need source-code changes, but clients or routing configuration must direct traffic through the proxy.

```text
Client                   ExaProp                    Backend
                         HTTP/1.1 proxy
   ── HTTP request ────> 127.0.0.1:8080 ──────────> 127.0.0.1:8081
   <─ HTTP response ────                <──────────
```

The longer-term engineering workflow is:

```text
Attach → Observe → Inject → Measure → Investigate → Improve → Verify
```

## Current capabilities

- Forward HTTP traffic to a single configured backend, with test coverage for `GET`, `HEAD`, and `POST`.
- Preserve paths, query strings, binary payloads, and end-to-end headers, including duplicate response headers.
- Remove hop-by-hop headers and headers nominated by `Connection`.
- Decode chunked bodies and buffer accepted payloads before forwarding.
- Limit active client connections and the size of each request and response body.
- Return explicit errors for unsupported protocol features and failed upstream requests.
- Stop accepting connections on Ctrl+C and allow a short grace period for active connections to finish.

The current transport is plain HTTP/1.1. HTTPS termination, HTTP/2, WebSockets, streaming responses, and multi-backend routing are outside the current implementation.

## Quickstart

### Prerequisites

- A Rust toolchain with Cargo that supports workspace resolver version 3.
- Python 3 for the included example backend.
- `curl` for sending requests.

Run these commands from the repository root in three separate terminals.

### 1. Start the example backend

```bash
python3 examples/echo_server.py
```

The backend listens on `127.0.0.1:8081`. On Windows, use `python` or `py -3` if `python3` is unavailable.

### 2. Start ExaProp

```bash
cargo run -p cli -- attach --listen 127.0.0.1:8080 --target 127.0.0.1:8081
```

ExaProp accepts requests on `127.0.0.1:8080` and forwards them to the backend. Both addresses are also the defaults for `attach`.

### 3. Send requests through the proxy

```bash
curl -i "http://127.0.0.1:8080/hello?query=test"
curl -i --data "service=checkout" "http://127.0.0.1:8080/echo"
```

The GET response includes `GET /hello?query=test`; the POST response echoes `service=checkout`. In Windows PowerShell, use `curl.exe` if `curl` resolves to a PowerShell alias.

Press Ctrl+C in the proxy terminal to stop ExaProp.

### Command-line options

| Option | Default | Purpose |
| --- | --- | --- |
| `--listen` | `127.0.0.1:8080` | Local IP address and port to bind |
| `--target` | `127.0.0.1:8081` | Backend IP address and port |

Both options accept socket addresses rather than URLs or hostnames. To inspect the command help:

```bash
cargo run -p cli -- attach --help
```

## Resource limits and timeouts

The proxy applies the following fixed limits:

| Control | Current behavior |
| --- | --- |
| Active client connections | At most 16, including keep-alive connections; capacity is acquired before accepting another connection |
| Request body | At most 4 MiB (4,194,304 bytes); larger bodies receive `413 Payload Too Large` |
| Response body | At most 4 MiB; larger upstream bodies produce `502 Bad Gateway` |
| Client request headers | 15-second header-read timeout |
| Upstream connection | 15-second connection timeout |
| Upstream response headers | 15-second timeout around the upstream request |
| Body reads | 15 seconds waiting for each next body frame |
| Shutdown | Up to 2 seconds for active connections before cancellation |

Body-read timeouts apply to individual frames, not to the total transaction duration. A body that continues producing frames within that interval can take longer than 15 seconds overall.

Bodies are fully buffered before forwarding. These limits constrain payload buffering and active connections; they do not establish a fixed ceiling for total process memory. Performance and resource overhead have not yet been established by published benchmarks in this repository.

## HTTP behavior

Hyper handles HTTP parsing and framing. The proxy adds destination rewriting, header filtering, body limits, and error handling.

### Forwarding

| Area | Behavior |
| --- | --- |
| Routing | Forward to one configured backend while preserving the path and query string |
| Host header | Preserve the client's `Host` header when present |
| Headers | Preserve end-to-end headers and duplicate values; strip hop-by-hop headers |
| Bodies | Forward binary bytes within the size limit |
| Chunked transfer | Decode and buffer accepted bodies before forwarding with recalculated framing |
| Empty requests | Omit an explicit `Content-Length` for empty `GET` and `HEAD` requests |
| Bodyless responses | Suppress response bodies for `HEAD`, `204`, `205`, and `304`; remove `Content-Length` for `204` and `205` |

### Unsupported features and failure responses

| Condition | Response |
| --- | --- |
| `CONNECT` or protocol upgrade requests | `501 Not Implemented` |
| Requests containing an `Expect` header | `417 Expectation Failed` |
| Request trailers | `501 Not Implemented` |
| Request body exceeds 4 MiB | `413 Payload Too Large` |
| Request body read times out | `408 Request Timeout` |
| Malformed HTTP requests | Rejected by Hyper; regression coverage includes a `400 Bad Request` case |
| Backend connection or response read fails | `502 Bad Gateway` |
| Backend response exceeds 4 MiB or contains trailers | `502 Bad Gateway` |
| Upstream request or response body read times out | `504 Gateway Timeout` |

Regression coverage lives in [core/src/proxy.rs](core/src/proxy.rs) and includes forwarding, framing, body boundaries, unsupported features, and disconnect handling.

## Project status and roadmap

ExaProp is in early development. The proxy foundation is implemented; the remaining milestones describe the intended direction rather than available functionality. Scope and scheduling will evolve as the design is validated.

| Milestone | Scope | Status |
| --- | --- | --- |
| M0 — Foundation | Cargo workspace, project governance, and Apache 2.0 licensing | Implemented |
| M1 — Proxy engine | Local HTTP/1.1 forwarding and CLI | Implemented |
| M2 — Protocol hardening | Header handling, body limits, timeouts, and regression coverage | Implemented |
| M3 — Observability | Request telemetry, bounded evidence collection, and explainable anomaly detection | Next milestone |
| M4 — Fault experiments | Controlled latency and HTTP error injection | Planned |
| M5 — Propagation analysis | Cross-service correlation, failure propagation graphs, and resilience scoring | Planned |

M3 should establish measurable observations before introducing diagnostic inference. Cross-service analysis in M5 will require a correlation design that connects observations across multiple services or proxy instances.

## Research direction

ExaProp is intended to support reproducible systems experiments around three questions:

1. **Interception overhead:** What latency, throughput, CPU, and memory costs does a bounded user-space proxy introduce under different workloads?
2. **Evidence and diagnosis:** How can observed transactions, derived anomalies, and diagnostic hypotheses remain distinguishable and auditable?
3. **Failure propagation:** How does an injected failure affect downstream services, and what evidence is needed to measure its reach and duration?

These are research objectives. Claims about performance, causal attribution, and resilience scores require documented experiments and validation.

## Repository structure

```text
exaprop/
├── Cargo.toml              # Workspace configuration
├── Cargo.lock              # Dependency lockfile
├── core/                   # core_engine library
│   └── src/
│       ├── lib.rs          # Public exports
│       └── proxy.rs        # Proxy implementation and regression tests
├── cli/                    # exaprop binary
│   └── src/main.rs         # Command-line interface
├── examples/
│   └── echo_server.py      # Local backend fixture
├── CONTRIBUTING.md         # Development and contribution guidelines
├── CODE_OF_CONDUCT.md      # Community standards
├── SECURITY.md             # Vulnerability reporting policy
└── LICENSE                 # Apache 2.0 license
```

## Development

Run the repository checks from the workspace root:

```bash
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace
```

The proxy module currently contains 21 tests spanning unit-level checks and local networking scenarios. Run the test suite to verify results for your environment and checkout.

To build the optimized CLI:

```bash
cargo build --release -p cli
```

The executable is written to `target/release/exaprop` (`exaprop.exe` on Windows).

## Contributing

Contributions to implementation, documentation, tests, benchmarks, and reproducible experiments are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for setup, checks, and pull request expectations, and follow the [Code of Conduct](CODE_OF_CONDUCT.md).

For significant changes, discuss the proposed approach in an issue before starting implementation.

## Security

Report vulnerabilities through the process described in [SECURITY.md](SECURITY.md). Keep local experiments on systems you own or are authorized to test.

## License

ExaProp is licensed under the [Apache License 2.0](LICENSE).
