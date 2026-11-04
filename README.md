<div align="center">

# ExaProp

**A high-performance, deterministic HTTP reverse proxy and resilience telemetry engine built with Rust.**

[![Status: Active Development](https://img.shields.io/badge/Status-Active_Development-success.svg)](#milestones--roadmap)
[![Protocol: RFC 9110](https://img.shields.io/badge/Protocol-RFC_9110_HTTP%2F1.1-informational.svg)](#key-features)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![Rust: Stable](https://img.shields.io/badge/Rust-1.80%2B-orange.svg)](rust-toolchain.toml)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](CONTRIBUTING.md)

[Quick Start](#quick-start) • [Architecture](#architecture) • [Features](#key-features) • [Observability](#structured-request-observability-m3) • [Anomaly Detection](#deterministic-anomaly-detection-m41) • [Roadmap](#milestones--roadmap) • [Contributing](#contributing)

</div>

---

## Overview

**ExaProp** is a local HTTP/1.x reverse proxy built with Rust, Tokio and Hyper. It records request observations and offers opt-in deterministic anomaly detection. HTTP handlers never wait for telemetry queue capacity or output I/O. Telemetry can be dropped, and paths, queries and service labels can contain sensitive data.

Body sizes, active connections, queue counts and serialized V1 output lengths are bounded. These limits do not establish a fixed process-memory ceiling or eliminate resource-exhaustion risks. See the [usage guide](documents/EXAPROP_USAGE.md) for supported protocols and operational limits.

---

## Key Features

| Capability | Engineering Guarantee |
| :--- | :--- |
| **Zero-Blocking Network Core** | Asynchronous Tokio HTTP/1.x forwarding engine; telemetry emission uses non-blocking `try_send` so proxying is never stalled by slow terminal sinks. |
| **Bounded Observability (M3)** | Captures monotonic processing durations (`Instant`), HTTP status codes, and typed outcome variants per request into a bounded 1,024-element queue. |
| **Deterministic Anomaly Detection (M4.1)** | Pure, stateless rule classifier evaluating HTTP 5xx errors, strict nanosecond latency boundaries, and upstream timeouts with zero heap allocations. |
| **Privacy-Safe Query Redaction** | Automatically detects and masks sensitive parameters (`token`, `secret`, `password`, `api_key`, `auth`) while preserving endpoint telemetry. |
| **Memory & Resource Bounded** | Fixed 4 MiB body buffering limits, 16 concurrent client caps, bounded diagnostic channels, and deterministic shutdown timeouts. |
| **Strict RFC 9110 Compliance** | Automatic hop-by-hop header stripping, Connection token validation, Via chain preservation, and proper 204 / 205 / 304 framing. |

---

## Architecture

```text
 ┌──────────────┐             ┌────────────────────────────────────────────────────────┐             ┌─────────────────┐
 │  HTTP Client │ ── HTTP ──> │               ExaProp Reverse Proxy                    │ ── HTTP ──> │ Upstream Target │
 │ (curl / app) │ <─ RESP ─── │                   (:8080)                              │ <─ RESP ─── │  (:8081 / svc)  │
 └──────────────┘             └──────────────────────────┬─────────────────────────────┘             └─────────────────┘
                                                         │
                                               Observation::new()
                                           [Non-blocking try_send]
                                                         │
                                                         ▼
                                       ┌───────────────────────────────────┐
                                       │   Bounded Observation Mailbox     │
                                       │        (Capacity: 1,024)          │
                                       └─────────────────┬─────────────────┘
                                                         │
                                                         ▼
                                       ┌───────────────────────────────────┐
                                       │        Coordinator Worker         │
                                       │  - Assigns monotonic seq number   │
                                       │  - Evaluates M4.1 Detector        │
                                       │  - Updates Atomic DetectionStats  │
                                       └─────────────────┬─────────────────┘
                                                         │
                                                         ▼
                                       ┌───────────────────────────────────┐
                                       │      Dedicated Output Thread      │
                                       │  stdout: Structured Observations  │
                                       │  stderr: Operational Diagnostics  │
                                       └───────────────────────────────────┘
```

---

## Quick Start

The architecture above depicts detection mode (`--detect`), whose coordinator sends to a bounded 256-batch queue before the stdout writer. Diagnostics use an independent stderr writer. Without detection, the default remains the legacy M3 observation printer.

### 1. Prerequisites
- **Rust Toolchain:** Stable Rust (1.80+) via [rustup](https://rustup.rs/)
- **Python 3:** For the bundled test backend
- **curl:** For sending test HTTP requests

### 2. Run in 3 Easy Steps

#### Step 1: Start the Mock Upstream Backend
In your first terminal, start the provided Python test server:
```bash
python3 examples/echo_server.py
# Listening on 127.0.0.1:8081
```

#### Step 2: Launch ExaProp Proxy
In a second terminal, build and start ExaProp attached to the backend:
```bash
cargo run --locked -p cli -- attach --listen 127.0.0.1:8080 --target 127.0.0.1:8081 --name api-service
```

#### Step 3: Send Test Requests
In a third terminal, send traffic through ExaProp:
```bash
# Standard GET request
curl -i "http://127.0.0.1:8080/users?page=1"

# Query with sensitive parameters (automatically redacted)
curl -i "http://127.0.0.1:8080/auth?user=alice&token=supersecret123&api_key=998877"

# POST request with JSON payload
curl -i -X POST -d '{"event":"checkout"}' "http://127.0.0.1:8080/events"
```

#### Live Telemetry Output in ExaProp Terminal:
```text
[api-service] GET /users?page=1 -> 200 (0.412ms) [Success] timestamp=1792627200.124500000
[api-service] GET /auth?user=alice&token=[REDACTED]&api_key=[REDACTED] -> 200 (0.285ms) [Success] timestamp=1792627200.450120000
[api-service] POST /events -> 200 (0.830ms) [Success] timestamp=1792627200.781200000
```

---

## Structured Request Observability (M3)

ExaProp guarantees **at most one observation per handled request**.

### Captured Observation Fields:
* **Service Name:** Configured via `--name <id>` (escaped, non-empty identifier).
* **HTTP Method:** Standard uppercase representation (`GET`, `POST`, `HEAD`, etc.).
* **Sanitized Path & Query:** Target path preserved; sensitive key values replaced with `[REDACTED]`.
* **Status Code:** Raw 3-digit HTTP response code (e.g., `200`, `404`, `502`).
* **Processing Latency:** High-precision monotonic elapsed duration (`std::time::Instant`) from request arrival to backend body completion.
* **Typed Outcome (`RequestOutcome`):**
  - `Success`: Valid HTTP exchange completed.
  - `BackendFailure`: Connection refused, reset, or broken pipe upstream.
  - `BackendTimeout`: Upstream response exceeded proxy timeout.
  - `ClientTimeout`: Downstream client stalled during body transmission.
  - `ClientFailure`: Downstream connection terminated prematurely.
  - `Rejected`: Invalid framing, oversized payload, or prohibited HTTP method.

---

## Deterministic Anomaly Detection (M4.1)

ExaProp includes a pure, mathematically deterministic anomaly detection engine in [`core/src/detection.rs`](core/src/detection.rs).

### Detection Rules:
1. **`HttpServerError`**: Any HTTP response status within `500..=599`.
2. **`ElevatedRequestDuration`**: Processing duration strictly exceeding the configured threshold (nanosecond precision, default: `1000ms`).
3. **`UpstreamTimeout`**: Proxy timed out waiting for upstream frames (`RequestOutcome::BackendTimeout`).

### Security & Invariants:
- **Zero Allocations:** Rule evaluation runs in zero heap allocations using fixed bitflags.
- **Scalar-Only Provenance:** Anomalies retain only scalar facts (timestamps, status, durations); **request/response bodies and headers are strictly excluded**.
- **No Global Mutexes:** Detectors are immutable, pure, and implement `Send + Sync` for lock-free parallel execution across CPU cores.

---

## CLI Reference

Enable CLI anomaly reporting (M4.3) against an existing plain HTTP backend:

```text
exaprop attach --target 127.0.0.1:8000 --name orders --detect --latency-threshold-ms 500
```

This emits bounded `OBSERVATION_V1` lines and at most one `ANOMALY_V1` line per evaluated observation, containing all matching reasons. Duration-only events are warnings; HTTP 5xx and upstream-timeout events are errors. Detection is best effort over received observations, not a complete request census. Anomaly lines omit paths, queries, headers and bodies, but include potentially sensitive service/method labels. See [commands, output, loss accounting and exit codes](documents/EXAPROP_USAGE.md#enable-anomaly-detection-m43).

Backends need no ExaProp SDK: supported plain HTTP behavior works across languages. Python/FastAPI and a standard-library HTTP backend have automated checks; Express has a manual example. Other frameworks are protocol-compatible candidates, not individually verified integrations.

```bash
exaprop attach [OPTIONS]
```

| Option | Flag | Default | Description |
| :--- | :--- | :--- | :--- |
| **Listen Address** | `--listen <ADDR>` | `127.0.0.1:8080` | Local socket address to bind and receive client traffic |
| **Target Address** | `--target <ADDR>` | `127.0.0.1:8081` | Upstream backend socket address to forward traffic to |
| **Service Label** | `--name <NAME>` | `backend-service` | Service identifier used to tag telemetry observations |
| **Anomaly Detection** | `--detect` | off | Enable all three anomaly rules and V1 output |
| **Duration Threshold** | `--latency-threshold-ms <INTEGER>` | `1000` with detection | Range 1–3,600,000; requires `--detect`; strict duration comparison |
| **Help** | `-h`, `--help` | — | Print command-line help information |
| **Version** | `-V`, `--version` | — | Print version information |

---

## Research & Systems Resilience Context

ExaProp is engineered as an empirical testbed for **distributed systems resilience, cascading failure analysis, and fault observability**:

1. **Monotonic Timing vs Wall-Clock Skew:** Eliminates NTP time synchronization artifacts by measuring latency strictly through hardware CPU ticks (`Instant`).
2. **Backpressure Decoupling:** Evaluates producer-consumer isolation under load shedding, proving that slow logging pipelines cannot degrade reverse-proxy forwarding throughput.
3. **Fault Provenance Isolation:** Formally distinguishes transport-layer breakdowns (TCP reset, connect errors) from protocol-level failures (HTTP 502/504 responses generated by intermediate gateways).

---

## Milestones & Roadmap

| Milestone | Focus Area | Status |
| :--- | :--- | :--- |
| **M0** | Cargo workspace architecture, linting, and CI pipelines | Completed |
| **M1** | Non-blocking HTTP reverse proxy engine & CLI attachment | Completed |
| **M2** | HTTP/1.1 correctness, body caps, hop headers & connection limits | Completed |
| **M3** | Structured request observability & privacy-safe query redaction | Completed |
| **M4.1** | Pure deterministic anomaly detection model & scalar invariants | Completed |
| **M4.2** | Consumer-side pipeline coordinator, bounded queues & worker isolation | Implemented |
| **M4.3** | CLI anomaly reporting flags (`--detect`) and V1 telemetry formatting | Implemented on integration branch; hosted verification pending |
| **M4.4** | End-to-end integration verification under simulated upstream faults | Planned |
| **M4.5** | Production hardening, audit validation, and telemetry benchmarks | Planned |
| **M5+** | Controlled fault injection, chaos orchestration & resilience metrics | Research Horizon |

---

## Development & Verification

ExaProp maintains rigorous code quality and security standards. Verify the workspace locally using:

```bash
# Check code formatting
cargo fmt --all -- --check

# Workspace typecheck
cargo check --workspace --locked

# Comprehensive clippy linting (zero warnings permitted)
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

# Execute entire test suite (unit, integration, and doctests)
cargo test --workspace --locked

# Verify documentation builds cleanly
cargo doc --workspace --no-deps --locked
```

---

## Contributing

Contributions, bug reports, and research discussions are warmly welcomed!

- Read our [Contributing Guide](CONTRIBUTING.md) for branch policies and validation instructions.
- Adhere to our [Code of Conduct](CODE_OF_CONDUCT.md).
- For security inquiries, consult our [Security Policy](SECURITY.md).

---

## License

ExaProp is licensed under the [Apache License, Version 2.0](LICENSE).  
Copyright (c) 2026 Dinesh Y R & ExaProp Contributors.
