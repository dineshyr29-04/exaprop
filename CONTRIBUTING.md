# Contributing to ExaProp

Thank you for your interest in contributing to **ExaProp**.

ExaProp is an open-source systems engineering and research project focused on evidence-based analysis of failure propagation in distributed systems.

Contributions are welcome in code, tests, documentation, experiments, benchmarks, and research.

## Before You Start

For significant changes, please open or discuss an issue before starting implementation.

This helps keep the project architecture consistent and avoids duplicated work.

For small fixes such as documentation corrections or obvious bugs, opening an issue first is optional.

## Development Setup

Clone the repository:

```bash
git clone https://github.com/dineshyr29-04/exaprop.git
cd exaprop
```

Install the current stable Rust toolchain through `rustup`.

Before submitting changes, run:

```bash
cargo fmt --all
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace
```

## Branches

Create a branch from `main`.

Use a descriptive prefix:

```text
feat/<name>       New functionality
fix/<name>        Bug fix
refactor/<name>   Code restructuring
docs/<name>       Documentation
test/<name>       Tests
bench/<name>      Benchmarking
research/<name>   Research or experimental work
chore/<name>      Tooling or maintenance
```

Examples:

```text
feat/http-parser
fix/connection-reset
test/proxy-integration
docs/m1-architecture
bench/proxy-overhead
research/failure-propagation
```

## Pull Requests

Pull requests should:

* clearly describe what changed and why
* reference a related issue when applicable
* include tests for behavioral changes
* update documentation when necessary
* pass the project's CI checks
* keep unrelated changes out of the PR

Prefer small, focused pull requests over large mixed changes.

## Commit Messages

Use clear commit messages.

The project generally follows Conventional Commits:

```text
feat: add HTTP request observation
fix: handle closed target connections
docs: update M1 architecture
test: add proxy integration test
refactor: separate observation model
```

## Design Principles

Contributions should follow the project's core principles:

```text
Observe before assuming.
Evidence before inference.
Measure before claiming.
Reproduce before concluding.
Experiment before generalizing.
```

In particular, ExaProp should distinguish between:

* **Observed facts**
* **Derived anomalies**
* **Diagnostic hypotheses**

The system should not claim a root cause without sufficient evidence.

## Research Contributions

Research and experimental contributions are welcome.

Experiments should document:

* hypothesis
* setup
* methodology
* variables
* measurements
* results
* limitations
* reproduction steps

Do not present experimental results as established conclusions without sufficient evidence.

## Security

Please do not publicly disclose security vulnerabilities through GitHub Issues.

See [SECURITY.md](SECURITY.md) for the security reporting process.

## Code of Conduct

By participating in this project, you agree to follow the project's [Code of Conduct](CODE_OF_CONDUCT.md).

## Questions

For questions about the project, open a GitHub Discussion or Issue when appropriate.

Thank you for helping improve ExaProp.
