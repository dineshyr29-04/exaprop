# Project Exaprop

[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg?style=flat-square)](CONTRIBUTING.md)
[![Contributor Covenant](https://img.shields.io/badge/Contributor%20Covenant-2.1-4baaaa.svg)](CODE_OF_CONDUCT.md)
[![Issues](https://img.shields.io/github/issues/dineshyr29-04/exaprop.svg)](https://github.com/dineshyr29-04/exaprop/issues)




# ExaProp

> Evidence-based analysis of failure propagation in distributed systems.

ExaProp is an open-source systems engineering and research project for observing distributed applications, collecting runtime evidence, detecting anomalies, and eventually analyzing how failures propagate through a system.

The project is being developed incrementally, with each milestone adding one layer of capability.

## Status

**Current development:** M1 — local HTTP/1.1 reverse proxy foundation

M1 currently forwards HTTP requests and responses. Structured observations and
diagnostics are future work; the proxy does not yet collect or display them.

---

## M1 — Local HTTP/1.1 Reverse Proxy

M1 provides an explicit local reverse proxy. Clients must send their HTTP
traffic through ExaProp; the target application's source code does not need to
change.

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
    --target 127.0.0.1:8081
```

`--listen` and `--target` default to those addresses.

The proxy currently:

* Forwards HTTP/1.1 requests and responses, including GET, HEAD, and POST.
* Preserves request paths and query strings, response status, and end-to-end headers.
* Relays binary bodies without interpreting their contents.
* Supports `Content-Length` and chunked framing by decoding and reframing bodies; the forwarded body uses a recalculated `Content-Length`.
* Supports persistent client connections and sequential requests on them.
* Handles clients concurrently, up to 16 active connections.
* Limits each request and response body to 4 MiB and applies a 15-second per-read/backend I/O timeout.
* Removes hop-by-hop headers, including headers named by `Connection`; persistence is managed independently on each side.

HTTP trailers, `Expect` headers, `CONNECT`, protocol upgrades/WebSockets, TLS,
and HTTP/2 are unsupported. Oversized request bodies receive `413`, request
body timeouts receive `408`, and unsupported `Expect` requests receive `417`.
Oversized or invalid backend responses produce `502`; backend timeouts produce
`504`. Malformed HTTP/1.1 framing is rejected by Hyper's parser. Bodies are buffered
up to the documented limit before forwarding, so this is intended for local
development and experiments rather than large streaming payloads. The proxy
has no authentication; keep its listen address on a trusted interface.

M1 does not yet record observations, detect anomalies, or determine root cause.

For example:

```text
GET /checkout → backend response: 500
```

The proxy relays this response; it does not yet report a structured observation.

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

## Current architecture

The CLI parses addresses and starts the proxy. Core owns the listener, HTTP
handling, backend client, bounded body relay, and connection lifecycle.

```text
cli (Clap arguments)
       ↓
core_engine::proxy
       ↓
HTTP/1.1 listener → backend HTTP client
```

---

## Project Structure

```text
exaprop/
├── Cargo.toml
├── Cargo.lock
├── README.md
├── LICENSE
├── CONTRIBUTING.md
├── SECURITY.md
├── CODE_OF_CONDUCT.md
│
├── core/
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       └── proxy.rs
│
├── cli/
│   ├── Cargo.toml
│   └── src/main.rs
├── examples/
│   └── echo_server.py
└── README.md
```

---

## Development

Requirements:

* Rust stable
* Cargo
* Python 3 for the included manual-test backend

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

For a manual end-to-end check, start the included backend:

```bash
python3 examples/echo_server.py
```

In a second terminal, run the proxy:

```bash
cargo run -p cli -- attach --listen 127.0.0.1:8080 --target 127.0.0.1:8081
```

In a third terminal, send requests through it:

```bash
curl -i 'http://127.0.0.1:8080/health?check=1'
curl -i -X POST --data 'hello=exaprop' http://127.0.0.1:8080/echo
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
