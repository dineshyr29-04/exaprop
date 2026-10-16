# ExaProp

[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-2024_Edition-orange.svg)](Cargo.toml)
[![Tests](https://img.shields.io/badge/Tests-21_Passed-brightgreen.svg)](core/src/proxy.rs)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg?style=flat-square)](CONTRIBUTING.md)
[![Contributor Covenant](https://img.shields.io/badge/Contributor%20Covenant-2.1-4baaaa.svg)](CODE_OF_CONDUCT.md)

> **A high-performance, local-first resilience observability and failure experimentation platform for distributed systems.**

---

## What is ExaProp?

In a distributed system or microservice architecture, services constantly talk to each other over HTTP. When one service slows down, crashes, or drops connections, failures often cascade uncontrollably (thread pools exhaust, retries cause storms, downstream services collapse).

**ExaProp** acts like a **flight recorder and chaos lab** that sits directly between your services. It attaches locally without requiring any code changes to your application:

```text
  [ Client / Frontend ]
          │
          │ HTTP Request (:8080)
          ▼
   ┌──────────────┐
   │   ExaProp    │  <── Observes metrics, detects anomalies,
   │ Reverse Proxy│  <── and injects controlled faults
   └──────┬───────┘
          │
          │ Forwarded HTTP (:8081)
          ▼
  [ Backend Service ]
```

### The Engineering Lifecycle
```text
Attach ➔ Observe ➔ Inject ➔ Measure ➔ Investigate ➔ Improve ➔ Verify
```

---

## ⚡ Quickstart (Try it in 60 seconds)

You can run and test ExaProp locally on your machine with 3 simple steps:

### 1. Start the included test backend
In your first terminal, start the Python backend server:
```bash
python3 examples/echo_server.py
# Backend running on http://127.0.0.1:8081
```

### 2. Start ExaProp
In a second terminal, launch ExaProp attached to that backend:
```bash
cargo run -p cli -- attach --listen 127.0.0.1:8080 --target 127.0.0.1:8081
# ExaProp listening on http://127.0.0.1:8080 and forwarding to :8081
```

### 3. Send test requests
In a third terminal, send traffic through ExaProp:
```bash
# Basic GET request
curl -i http://127.0.0.1:8080/hello?query=test

# POST request with JSON payload
curl -i -X POST -H "Content-Type: application/json" -d '{"service":"checkout"}' http://127.0.0.1:8080/echo
```
ExaProp receives your traffic, validates protocol correctness, safely forwards it to the backend, and relays the response back to you.

---

## 🗺️ Project Milestones & Progress

ExaProp is built incrementally. Each milestone delivers a solid, thoroughly tested layer:

| Milestone | Capability | Status | Verification |
| :--- | :--- | :---: | :--- |
| **M0** | Project charter, Cargo workspace, Apache 2.0 governance | **Completed** | Clean workspace structure |
| **M1** | Bounded local HTTP/1.1 reverse proxy engine & CLI | **Completed** | 14 integration tests |
| **M2** | HTTP/1.1 correctness, RFC 9112 hardening & reliability | **Completed** | **21 integration tests (100% green)** |
| **M3** | Resilience observability, telemetry & anomaly detection | **In Progress** | Next implementation target |
| **M4** | Chaos engine & programmable fault injection (latency, 5xx) | Planned | Target: Nov 2026 |
| **M5** | Failure propagation graphing & automated resilience scoring | Planned | Target: Nov 2026 |

---

## 🛡️ Built-in Safety Guarantees

An observability tool must never become the cause of a system outage. ExaProp implements three strict invariants:

1. **Bounded Concurrency (16 Max Connections)**:
   * Traffic is regulated through an asynchronous semaphore (`tokio::sync::Semaphore`).
   * A surge of client requests cannot cause thread exhaustion. Disconnected clients immediately release their permit.
2. **Bounded Memory Buffering (4 MiB Max Body)**:
   * Request and response payloads are read frame-by-frame up to **4 MiB (4,194,304 bytes)**.
   * Oversized client payloads are immediately rejected with `413 Payload Too Large`; oversized backend payloads receive `502 Bad Gateway`. ExaProp will never run out of memory (OOM).
3. **Deterministic 15-Second Timeouts**:
   * Stalled reads or slowloris attacks trigger a `408 Request Timeout`.
   * Unresponsive backend servers trigger a `504 Gateway Timeout` after 15 seconds.

---

## 📋 HTTP Protocol Contract (RFC 9110 & RFC 9112)

Every protocol behavior is validated by automated regression tests in [`core/src/proxy.rs`](core/src/proxy.rs):

### What ExaProp Supports:
* **Methods**: Full forwarding for `GET`, `HEAD`, and `POST`.
* **Path & Query Strings**: Exact preservation of URI paths and query arguments.
* **Headers**: Preserves custom headers and duplicate multi-value headers (e.g., multiple `Set-Cookie` lines).
* **Binary Data**: Transparent, corruption-free forwarding of binary payloads (images, files, protobuf).
* **Chunked Framing**: Transparently decodes chunked streams and emits valid `Content-Length` frames.
* **RFC 9112 §6.2 (Empty Requests)**: Automatically omits `Content-Length` on empty `GET` and `HEAD` requests.
* **RFC 9110 §15.3.5 / RFC 9112 §6.3 (No-Body Responses)**:
  * For `204 No Content` and `205 Reset Content`: Strips `Content-Length` and guarantees zero body bytes.
  * For `304 Not Modified` and `HEAD`: Preserves cached metadata while strictly guaranteeing zero body bytes.
* **RFC 9112 §7.6.1 (Hop-by-Hop Stripping)**: Removes transport-only headers (`Connection`, `Keep-Alive`, `Upgrade`, etc.), including dynamically nominated connection tokens.

### What ExaProp Explicitly Rejects:
| Client Request | Status Code | Reason |
| :--- | :---: | :--- |
| `CONNECT` Method | `501 Not Implemented` | Prevents unauthorized TCP proxy tunneling |
| `Upgrade` / WebSockets | `501 Not Implemented` | Protocol switching out of scope for v0.1.0 |
| `Expect: 100-continue` | `417 Expectation Failed` | Avoids stalled upstream handshakes |
| Request Trailers (`Trailer`) | `501 Not Implemented` | Chunk trailers are unsupported |
| Payload > 4 MiB | `413 Payload Too Large` | Enforces memory bounding |
| Malformed HTTP bytes | `400 Bad Request` | Rejected by Hyper parser |

---

## 🔬 Research Context & Academic Motivation

ExaProp is also designed as a systems research testbed for investigating **failure propagation in distributed microservices**.

### Key Research Questions:
1. **Lightweight Interception vs Heavyweight Meshes**:
   Can a sub-millisecond, bounded user-space reverse proxy written in Rust provide deterministic resilience experimentation without the heavy resource footprint of Envoy/Istio sidecars (~100MB per pod) or the kernel complexity of eBPF?
2. **Evidence Before Inference**:
   Traditional APMs rely heavily on statistical sampling and probabilistic inferences. How can an observability engine track deterministic causal chains (*HTTP Transaction ➔ Exact Evidence ➔ Empirical Anomaly Classification*) without false alarms?
3. **Blast-Radius Quantification**:
   When synthetic latency or packet drop is injected into a single microservice, how far and how fast does the degradation propagate through the service dependency graph?

---

## 📂 Codebase Organization

The repository is organized as a clean Rust Cargo workspace:

```text
exaprop/
├── Cargo.toml               # Workspace configuration
├── README.md                # Project documentation and quickstart
├── LICENSE                  # Apache 2.0 license
├── CONTRIBUTING.md           # Contribution guidelines
│
├── core/                    # Core proxy and networking library (`core_engine`)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs           # Public library exports
│       └── proxy.rs         # Reverse proxy engine, bounds & 21 tests
│
├── cli/                     # CLI binary (`exaprop`)
│   ├── Cargo.toml
│   └── src/
│       └── main.rs          # CLI argument parser (`exaprop attach`)
│
└── examples/
    └── echo_server.py       # Python backend fixture for local testing
```

---

## 🛠️ Verification & Testing Commands

To run tests and verify the entire codebase:

```bash
# Check compilation across all crates
cargo check --workspace

# Run all 21 automated integration tests
cargo test --workspace

# Verify code formatting
cargo fmt --all -- --check

# Build the release binary
cargo build --release
```

---

## 📜 License

ExaProp is open-source software licensed under the **[Apache License 2.0](LICENSE)**.
