# Project Exaprop

[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg?style=flat-square)](CONTRIBUTING.md)
[![Contributor Covenant](https://img.shields.io/badge/Contributor%20Covenant-2.1-4baaaa.svg)](CODE_OF_CONDUCT.md)
[![Issues](https://img.shields.io/github/issues/dineshyr29-04/faultlab.svg)](https://github.com/dineshyr29-04/faultlab/issues)




# ExaProp

> Evidence-based analysis of failure propagation in distributed systems.

ExaProp is an open-source systems engineering and research project for observing distributed applications, collecting runtime evidence, detecting anomalies, and eventually analyzing how failures propagate through a system.

The project is being developed incrementally, with each milestone adding one layer of capability.

## Status

**Current development:** M1 — HTTP Observer

M2 will build the first diagnostic layer on top of the observations collected by M1.

---

## M1 — HTTP Observer

M1 provides a local HTTP observation proxy that requires **no changes to the target application's source code**.

```text
Client
  |
  | HTTP
  v
ExaProp :8080
  |
  | HTTP
  v
Target App :8081
```

Example:

```bash
exaprop attach \
    --listen 127.0.0.1:8080 \
    --target 127.0.0.1:8081 \
    --name backend-api
```

M1 focuses on:

* TCP connection handling
* HTTP/1.1 requests
* HTTP/1.1 responses
* Request/response observation
* Status-code observation
* Measured elapsed duration
* Request timestamps
* Forwarding traffic to the target application
* Basic evidence collection

M1 does **not** attempt to determine root cause.

For example:

```text
GET /checkout
Status: 500
Duration: 1247 ms
```

This is an observation.

It can later become evidence for higher-level diagnostics.

### M1 Design Principle

> **Observe before assuming.**

---

## M2 — Diagnostic Engine

M2 builds on M1's observations and introduces basic anomaly detection.

Conceptually:

```text
HTTP Request
     ↓
HTTP Response
     ↓
Observation
     ↓
Evidence
     ↓
Anomaly
```

Initial anomaly types may include:

```text
HIGH_LATENCY
SERVER_ERROR
CLIENT_ERROR
CONNECTION_FAILURE
```

Example:

```text
Observation:
GET /checkout
500
1247 ms

Diagnostic:
SERVER_ERROR
HIGH_LATENCY
```

M2 should distinguish between:

* **Observed facts** — directly measured by ExaProp
* **Derived anomalies** — detected from observations
* **Hypotheses** — possible explanations requiring more evidence

ExaProp should not claim a root cause from insufficient evidence.

### M2 Design Principle

> **Evidence before inference.**

---

## Architecture

The early architecture is intentionally small:

```text
             ExaProp
                |
        ┌───────┴───────┐
        ↓               ↓
   HTTP Observer   Diagnostic Engine
        |               |
        └───────┬───────┘
                ↓
             Evidence
```

The observer is the foundation. Diagnostic capabilities are built on top of the evidence it produces.

---

## Project Structure

```text
exaprop/
├── Cargo.toml
├── README.md
├── LICENSE
├── CONTRIBUTING.md
├── SECURITY.md
├── CODE_OF_CONDUCT.md
│
├── crates/
│   └── exaprop-core/
│       └── src/
│           ├── lib.rs
│           ├── model.rs
│           ├── proxy.rs
│           ├── http.rs
│           └── detector.rs
│
├── cli/
│   └── src/
│       ├── main.rs
│       └── display.rs
│
├── tests/
│   └── integration/
│
├── lab/
│   └── simple-http-server/
│       └── server.py
│
└── docs/
    ├── architecture.md
    └── milestones/
```

---

## Development

Requirements:

* Rust stable
* Cargo
* Python 3 for the local test server

Clone the repository:

```bash
git clone https://github.com/dineshyr29-04/exaprop.git
cd exaprop
```

Check the workspace:

```bash
cargo check --workspace
```

Run tests:

```bash
cargo test --workspace
```

Format:

```bash
cargo fmt --all
```

Run Clippy:

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Build:

```bash
cargo build --workspace
```

---

## Development Philosophy

ExaProp follows a simple progression:

```text
Observe
   ↓
Measure
   ↓
Collect Evidence
   ↓
Detect Anomalies
   ↓
Infer Carefully
   ↓
Understand Failure Propagation
```

The project prioritizes measurable evidence and reproducible experiments over unsupported conclusions.

---

## Contributing

Contributions are welcome in:

* Rust implementation
* Tests
* Documentation
* Benchmarks
* Experiments
* Research

See [CONTRIBUTING.md](CONTRIBUTING.md) for development guidelines.

## License

ExaProp is licensed under the **Apache License 2.0**.

See [LICENSE](LICENSE) for the full license text.
