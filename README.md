# Project Exaprop

[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg?style=flat-square)](CONTRIBUTING.md)
[![Contributor Covenant](https://img.shields.io/badge/Contributor%20Covenant-2.1-4baaaa.svg)](CODE_OF_CONDUCT.md)
[![Issues](https://img.shields.io/github/issues/dineshyr29-04/faultlab.svg)](https://github.com/dineshyr29-04/faultlab/issues)




# ExaProp

> **Evidence-based analysis of failure propagation in distributed systems.**

ExaProp is an open-source systems engineering and research project for observing, analyzing, and eventually experimentally validating how failures propagate through distributed applications.

The project starts at the network boundary: ExaProp can act as an explicit local HTTP observation proxy between a client and a target application, allowing request/response behavior to be measured without modifying the target application's source code.

```text
                    ExaProp
                       │
                       │ observes
                       ▼
Client ───────► HTTP Observer ───────► Target Application
                    │
                    ├── latency
                    ├── status
                    ├── failures
                    └── evidence
```

---

## Project Status

**Current milestone:** M1 — Local HTTP Observer

**Development status:** Early development

### Implemented / in progress

* [x] Rust workspace
* [x] Core library + CLI separation
* [x] Local development lab
* [x] TCP proxy foundation
* [ ] Complete HTTP/1.1 observation
* [ ] Response observation
* [ ] Latency measurement
* [ ] Basic anomaly detection
* [ ] Structured diagnostic events

### Planned

* Diagnostic engine
* Local system discovery
* Dependency graphs
* Failure propagation analysis
* Controlled failure simulation
* Fault injection
* Causal timelines
* Blast-radius analysis
* Resilience validation
* CI/CD experiments
* OpenTelemetry correlation
* Incident investigation
* Web UI

See the [Roadmap](#roadmap) for the complete development plan.

---

# Why ExaProp?

Distributed systems rarely fail at the exact location where the first problem occurs.

A dependency becomes slow.

```text
Database
   │
   │ +800 ms
   ▼
Order Service
   │
   │ request becomes slow
   ▼
API
   │
   │ timeout
   ▼
Client
```

The visible symptom may be an HTTP 504 at the API while the original problem was a slow database.

Traditional observability systems are excellent at collecting telemetry, but understanding **how an observed failure propagated through a system** requires combining evidence across multiple layers.

ExaProp is being developed around that problem.

The long-term goal is to move from:

```text
"What failed?"
```

toward:

```text
"What happened?"
        ↓
"Where did it begin?"
        ↓
"How did it propagate?"
        ↓
"What did it affect?"
        ↓
"How resilient was the system?"
```

---

# Design Philosophy

ExaProp follows several principles.

### 1. Observe before modifying

The initial observer should work without requiring changes to application source code.

### 2. Evidence before conclusions

The system distinguishes between what was directly observed and what was inferred.

```text
Observed fact
    ↓
Evidence
    ↓
Derived anomaly
    ↓
Hypothesis
```

For example:

```text
Observed:
GET /checkout → HTTP 500 → 1247 ms

Derived:
HIGH_LATENCY
SERVER_ERROR

Possible hypothesis:
Downstream dependency may have contributed
```

A hypothesis is not presented as a proven root cause unless sufficient evidence exists.

### 3. The core should not depend on presentation

The analysis engine produces structured data.

The CLI is responsible for displaying it.

```text
                    ┌──────────────┐
                    │     CLI      │
                    │   display    │
                    └──────┬───────┘
                           │
                           ▼
                    ┌──────────────┐
                    │ ExaProp Core │
                    └──────┬───────┘
                           │
              ┌────────────┼────────────┐
              ▼            ▼            ▼
          Transport     Protocol     Detection
```

### 4. Cross-platform systems design

The core networking implementation is designed around Rust's portable standard-library abstractions rather than Linux-only system calls wherever possible.

Linux is useful for development and experimentation, but platform-specific functionality will be isolated when required.

### 5. Reproducible experiments

The research components should eventually allow experiments to be reproduced from the repository.

---

# Current M1 Architecture

The first working scenario is deliberately simple.

```text
┌───────────────┐
│ Browser /     │
│ Postman /     │
│ HTTP Client   │
└───────┬───────┘
        │
        │ HTTP
        ▼
┌───────────────────────┐
│ ExaProp               │
│ Local HTTP Observer   │
│                       │
│ 127.0.0.1:8080       │
└──────────┬────────────┘
           │
           │ HTTP
           ▼
┌───────────────────────┐
│ Target Application    │
│                       │
│ 127.0.0.1:8081       │
└───────────────────────┘
```

The target application does not need to know that ExaProp is observing the traffic.

---

# Repository Structure

```text
exaprop/
│
├── Cargo.toml
├── Cargo.lock
│
├── README.md
├── LICENSE
├── CONTRIBUTING.md
├── CODE_OF_CONDUCT.md
├── SECURITY.md
│
├── crates/
│   └── exaprop-core/
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           ├── model.rs
│           ├── proxy.rs
│           ├── http.rs
│           └── detector.rs
│
├── cli/
│   ├── Cargo.toml
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
├── docs/
│   ├── architecture.md
│   ├── roadmap.md
│   └── milestones/
│       └── M1-observer.md
│
└── .github/
    ├── ISSUE_TEMPLATE/
    ├── PULL_REQUEST_TEMPLATE.md
    └── workflows/
```

The repository will grow as functionality becomes real. Future modules should not be created merely because they appear on the roadmap.

---

# Requirements

## Required

### Rust

Install the current stable Rust toolchain using `rustup`.

Verify:

```bash
rustc --version
cargo --version
```

Recommended:

```bash
rustup update stable
rustup default stable
```

### Git

```bash
git --version
```

### Python

Python 3 is required for the local test server.

Verify:

```bash
python3 --version
```

---

# Getting Started

## 1. Clone the repository

```bash
git clone https://github.com/dineshyr29-04/exaprop.git
cd exaprop
```

## 2. Verify the workspace

```bash
cargo check --workspace
```

## 3. Format the project

```bash
cargo fmt --all
```

Check formatting without modifying files:

```bash
cargo fmt --all -- --check
```

## 4. Run tests

```bash
cargo test --workspace
```

## 5. Run Clippy

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

## 6. Build

```bash
cargo build --workspace
```

For an optimized build:

```bash
cargo build --workspace --release
```

The release binary will be located under:

```text
target/release/exaprop
```

---

# Running ExaProp

The intended M1 command is:

```bash
cargo run -p exaprop -- attach \
    --listen 127.0.0.1:8080 \
    --target 127.0.0.1:8081 \
    --name backend-api
```

After building:

```bash
./target/debug/exaprop attach \
    --listen 127.0.0.1:8080 \
    --target 127.0.0.1:8081 \
    --name backend-api
```

The exact command-line interface may evolve during M1 development.

Run:

```bash
cargo run -p exaprop -- --help
```

to see the current implementation.

---

# Running the Local Lab

The repository contains a minimal HTTP server for development and integration testing.

Start it with:

```bash
cd lab/simple-http-server
python3 server.py
```

The server should listen on:

```text
127.0.0.1:8081
```

Keep this terminal running.

Open another terminal and start ExaProp:

```bash
cargo run -p exaprop -- attach \
    --listen 127.0.0.1:8080 \
    --target 127.0.0.1:8081 \
    --name simple-server
```

Then send a request through the ExaProp listener:

```bash
curl http://127.0.0.1:8080/
```

The traffic path is:

```text
curl
 │
 │ :8080
 ▼
ExaProp
 │
 │ :8081
 ▼
Python HTTP Server
```

ExaProp should eventually report the observed request and response.

---

# Example Observation

The intended M1 output looks conceptually like:

```text
2026-09-16T12:10:21Z  GET  /  200  4.82ms
2026-09-16T12:10:23Z  GET  /health  200  1.31ms
2026-09-16T12:10:26Z  GET  /checkout  500  1247ms
```

With detection enabled, an event may look like:

```text
[ANOMALY] SERVER_ERROR
GET /checkout
status=500
elapsed=1247ms
```

The exact output format is intentionally subject to change while M1 is being implemented.

---

# Understanding Latency

ExaProp measures elapsed time around the observation path.

It should therefore be described as:

> **proxy-observed elapsed duration**

rather than claiming it is the exact application latency.

For example:

```text
Client
  │
  │ request
  ▼
ExaProp
  │
  │ request
  ▼
Application
  │
  │ response
  ▼
ExaProp
  │
  │ response
  ▼
Client
```

The measured duration includes behavior associated with the observation path.

This distinction becomes important when ExaProp is evaluated experimentally.

---

# Core Data Model

The long-term data model follows this conceptual pipeline:

```text
Request
   │
   ▼
Response
   │
   ▼
Observation
   │
   ▼
Evidence
   │
   ▼
Anomaly
   │
   ▼
Incident
```

An observation might contain:

```text
timestamp
request method
request path
request headers
response status
response headers
elapsed duration
connection information
```

An anomaly might contain:

```text
type
severity
observation reference
threshold
measured value
timestamp
```

Later versions can add:

```text
dependency
trace
process
service
failure propagation
causal hypothesis
blast radius
```

---

# Development Workflow

ExaProp uses a feature-branch + pull-request workflow.

Do not commit directly to `main` for normal development.

```text
Issue
  │
  ▼
Design / discussion
  │
  ▼
Feature branch
  │
  ▼
Implementation
  │
  ├── tests
  ├── documentation
  └── benchmarks when relevant
  │
  ▼
Pull Request
  │
  ▼
CI
  │
  ▼
Review
  │
  ▼
main
```

## Branch naming

Use descriptive prefixes:

```text
feat/tcp-proxy
feat/http-parser
feat/anomaly-detector

fix/connection-reset

refactor/observation-model

test/proxy-integration

docs/m1-architecture

bench/proxy-overhead

experiment/latency-threshold
```

Avoid permanent milestone branches such as:

```text
M1
M2
M3
```

Milestones describe project state; branches describe development work.

---

# Creating a Feature Branch

Update your local `main`:

```bash
git checkout main
git pull --ff-only origin main
```

Create a branch:

```bash
git checkout -b feat/http-parser
```

Make your changes.

Run:

```bash
cargo fmt --all
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Check the diff:

```bash
git diff
git diff --check
```

Commit:

```bash
git add .
git commit -m "feat: implement HTTP/1.1 request parser"
```

Push:

```bash
git push -u origin feat/http-parser
```

Then open a Pull Request against `main`.

---

# Testing

ExaProp uses multiple levels of testing.

## Unit tests

Test individual components:

```bash
cargo test --workspace
```

Examples:

```text
HTTP parsing
observation model
latency measurement
status detection
anomaly detection
```

## Integration tests

Integration tests should verify real component interaction:

```text
client
  ↓
ExaProp
  ↓
test server
```

Examples:

```text
request forwarding
response forwarding
HTTP status detection
connection failures
latency thresholds
```

## Manual testing

The local lab exists for manual experiments.

```bash
python3 lab/simple-http-server/server.py
```

Then:

```bash
cargo run -p exaprop -- attach \
    --listen 127.0.0.1:8080 \
    --target 127.0.0.1:8081
```

---

# Code Quality

Before opening a Pull Request, run:

```bash
cargo fmt --all -- --check
```

```bash
cargo check --workspace
```

```bash
cargo test --workspace
```

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

And:

```bash
git diff --check
```

All should pass.

---

# CI

ExaProp uses GitHub Actions for automated validation.

The CI pipeline is intended to validate:

```text
Formatting
   ↓
Compilation
   ↓
Tests
   ↓
Clippy
   ↓
Cross-platform builds
```

The long-term CI matrix will include:

```text
Linux
Windows
macOS
```

This is important because cross-platform support is part of the engineering goal.

---

# Roadmap

## M0 — Engineering Foundation

Establish the professional project foundation.

* Cargo workspace
* CLI/core separation
* CI
* formatting
* linting
* testing
* documentation
* cross-platform build foundation

---

## M1 — Local HTTP Observer

Observe local HTTP applications without source-code modification.

```text
Client
  ↓
ExaProp
  ↓
Target
```

Features:

* TCP listener
* connection handling
* HTTP/1.1
* request observation
* response observation
* forwarding
* latency measurement
* status detection
* timestamps
* terminal events

---

## M2 — Diagnostic Engine

Convert observations into structured anomalies.

Potential detections:

```text
5xx
4xx
timeout
high latency
connection failure
malformed response
connection reset
```

Model:

```text
Observation
     ↓
Evidence
     ↓
Anomaly
```

---

## M3 — System Discovery

Discover local system relationships.

Potential sources:

```text
processes
ports
listeners
network connections
services
```

Produce a system model.

---

## M4 — Dependency / Failure Graph

Represent relationships such as:

```text
API
 ↓
Order Service
 ↓
Payment Service
 ↓
Database
```

Then model failure propagation.

---

## M5 — Controlled Failure Simulation

Model hypothetical failures without necessarily modifying the real system.

Example:

```text
Database latency +500ms
        ↓
predict request impact
        ↓
predict timeout risk
        ↓
predict downstream effects
```

---

## M6 — Real Fault Injection

Controlled experiments involving:

```text
latency
packet loss
connection failure
HTTP errors
process failure
CPU pressure
memory pressure
```

Safety and isolation will be critical.

---

## M7 — Causal Timeline

Reconstruct temporal chains:

```text
fault
  ↓
request
  ↓
dependency
  ↓
latency
  ↓
timeout
  ↓
retry
  ↓
downstream load
  ↓
error
```

---

## M8 — Blast Radius

Estimate affected:

```text
services
endpoints
requests
critical paths
system components
```

---

## M9 — Resilience Validation

Experimentally evaluate mechanisms such as:

```text
timeouts
retries
circuit breakers
bulkheads
connection pools
backpressure
fallbacks
```

---

## M10 — CI/CD Experiments

Allow resilience experiments and assertions to run automatically.

Example:

```text
Experiment
   ↓
Inject condition
   ↓
Observe system
   ↓
Evaluate assertions
   ↓
Pass / Fail
```

---

## M11 — OpenTelemetry Correlation

Combine:

```text
ExaProp observations
        +
OpenTelemetry traces
        +
logs
        +
system metrics
```

into a unified evidence model.

---

## M12 — Incident Investigation

Given collected evidence:

```text
logs
traces
metrics
network observations
system events
```

reconstruct:

```text
origin
  ↓
propagation
  ↓
symptoms
  ↓
impact
```

---

## M13 — Web UI

Eventually provide:

```text
System topology
Failure graph
Causal timeline
Blast radius
Experiments
Evidence
Reports
```

The CLI will remain a first-class interface.

---

# Research Direction

ExaProp is being developed alongside a research program focused on:

> **Evidence-Based Failure Propagation Analysis for Distributed Systems**

A central research question is:

> Can observable runtime behavior be used to reconstruct failure propagation and estimate system impact without requiring modifications to application source code?

The project will evaluate this through reproducible experiments rather than relying solely on conceptual claims.

Potential research metrics include:

```text
Detection accuracy
Detection latency
False-positive rate
False-negative rate
Causal reconstruction accuracy
Blast-radius accuracy
Throughput overhead
Latency overhead
CPU overhead
Memory overhead
```

Research artifacts are intended to include:

```text
experiment definitions
benchmark scripts
datasets
results
reproduction instructions
paper
```

---

# Research vs Product

ExaProp has two connected goals.

### Engineering goal

Build a useful open-source systems tool.

### Research goal

Develop and evaluate methods for evidence-based failure propagation analysis.

The software should therefore be:

```text
usable
testable
measurable
reproducible
```

rather than simply demonstrating a concept.

---

# Security

ExaProp is intended to observe and eventually manipulate system behavior.

Some future functionality, particularly fault injection, can have significant operational consequences.

Until those capabilities are implemented and documented:

* run experiments only on systems you own or are authorized to test
* avoid production systems
* isolate experiments
* use explicit target configuration
* avoid exposing observation endpoints unnecessarily
* validate experiment parameters
* treat fault-injection functionality as potentially destructive

See [`SECURITY.md`](SECURITY.md) for security-related reporting and guidance.

---

# Contributing

Contributions are welcome.

Before starting substantial work:

1. Check existing issues.
2. Open an issue for significant changes.
3. Discuss architectural changes before implementation.
4. Create a focused branch.
5. Add tests.
6. Update documentation where necessary.
7. Run the complete local validation suite.
8. Open a Pull Request.

See [`CONTRIBUTING.md`](CONTRIBUTING.md).

---

# License

ExaProp is distributed under the license contained in [`LICENSE`](LICENSE).

---

# Project Principles

ExaProp is guided by a few simple rules:

```text
Observe before assuming.

Evidence before inference.

Measure before claiming.

Reproduce before concluding.

Experiment before generalizing.
```

The goal is not merely to tell developers **that something failed**.

The goal is to understand **what happened, how it propagated, and what evidence supports that explanation.**

---

# Current Development Command Reference

For contributors, the most commonly used commands are:

```bash
# Check
cargo check --workspace

# Build
cargo build --workspace

# Release build
cargo build --workspace --release

# Test
cargo test --workspace

# Format
cargo fmt --all

# Check formatting
cargo fmt --all -- --check

# Lint
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Run CLI
cargo run -p exaprop -- --help

# Git status
git status

# Review changes
git diff

# Check whitespace errors
git diff --check
```

---

# Maintainer

**Dinesh**

ExaProp is currently maintained as an independent open-source systems engineering and research project.

As the project grows, subsystem ownership and additional maintainers will be documented here.

---

# Status

```text
Project:       ExaProp
Focus:         Distributed systems
Current:       M1 — Local HTTP Observer
Language:      Rust
Interface:     CLI
License:       See LICENSE
Research:      Active
```

**ExaProp is under active development. APIs, CLI commands, architecture, and experimental interfaces may change before the first stable release.**
