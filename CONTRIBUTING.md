# Contributing to ExaProp

Contributions to code, tests, documentation, reproducible experiments, and tooling are welcome. M3 HTTP observability is implemented; M4 anomaly detection is planned. Check the [README](README.md#roadmap) before assuming a roadmap feature exists.

## Propose a change

Search existing issues and pull requests first. Discuss substantial protocol, API, architecture, dependency, or roadmap changes before implementing them. Small documentation corrections and straightforward fixes do not require a separate issue. Use the issue forms for reproducible bugs and concrete feature proposals.

Keep requests and examples synthetic. Do not put credentials, private URLs, authorization headers, private traffic captures, or suspected vulnerability details in public issues. Follow [SECURITY.md](SECURITY.md) for security reports and the [Code of Conduct](CODE_OF_CONDUCT.md) for participation.

## Set up a checkout

```bash
git clone https://github.com/dineshyr29-04/exaprop.git
cd exaprop
git switch -c fix/describe-the-change
cargo build --workspace --locked
```

External contributors should fork the repository and clone their fork. Start work from updated `main`, use a focused branch, and keep unrelated changes out. Useful prefixes include `fix/`, `feat/`, `docs/`, `test/`, and `chore/`.

Install Rust through rustup with your platform's native linker. The [toolchain file](rust-toolchain.toml) selects stable Rust plus rustfmt and Clippy; it does not pin an exact compiler or declare a tested MSRV. Confirm your environment with `rustup show`, `rustc -vV`, and `cargo --version`. No centrally hosted ExaProp backend or repository secret is needed for builds or tests.

## Verification

Run from the repository root:

```bash
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --release --locked
cargo doc --workspace --no-deps --locked
```

CI sets `RUSTDOCFLAGS=-D warnings` for documentation. To reproduce that locally, set it before `cargo doc`:

```bash
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
```

PowerShell:

```powershell
$env:RUSTDOCFLAGS = '-D warnings'
cargo doc --workspace --no-deps --locked
```

On Windows, the existing helper runs all Rust checks, including debug builds and strict rustdoc:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/check.ps1
```

That execution-policy option applies only to this process. The helper does not run the Python smoke suites. Use `cargo fmt --all` to apply formatting before rerunning the check. Behavioral changes should have meaningful regression coverage. Documentation-only changes do not need artificial unit tests; check the content, commands, and links instead and explain which runtime checks were unnecessary.

### Real HTTP backends and output failure checks

Use Python 3.12 and an isolated environment:

```text
python -m venv .venv
```

Windows PowerShell:

```powershell
.\.venv\Scripts\python.exe -m pip install -r examples/requirements-smoke.txt
cargo build --workspace --release --locked
.\.venv\Scripts\python.exe scripts/smoke_fastapi.py --include-timeouts
.\.venv\Scripts\python.exe scripts/smoke_http_review.py
```

Linux/macOS:

```bash
.venv/bin/python -m pip install -r examples/requirements-smoke.txt
cargo build --workspace --release --locked
.venv/bin/python scripts/smoke_fastapi.py --include-timeouts
.venv/bin/python scripts/smoke_http_review.py
```

Use `python3` to create the environment if that is your system command. The suites start local child backends and proxies and clean them up. Allow loopback sockets and child-process execution. FastAPI deliberately exercises two 15-second timeout paths; HTTP review exercises malformed headers, Via chains, upstream 101, HTTP 205 wire framing, the echo backend, and blocked/closed stderr. Keep the full timeout run when validating changes to these areas.

Evidence is saved under ignored `tmp/fastapi-smoke/` and `tmp/http-review/` directories. CI runs both suites on Linux and Windows and retains available smoke evidence for seven days, even after a failed step. A cancelled job may not finish writing or uploading evidence. Do not commit local virtual environments, build output, or logs. Review captured data before sharing it.

### Workflow and issue-form changes

Validate workflows with [actionlint](https://github.com/rhysd/actionlint) and parse changed YAML with a YAML 1.2-capable validator. Review GitHub issue-form field IDs, required fields, labels, and contact URLs separately; actionlint validates workflows, not issue forms. Check relative Markdown targets and heading anchors from the document's directory.

Actions are pinned to full commit SHAs with release-version comments. [Dependabot](.github/dependabot.yml) proposes GitHub Actions updates weekly. Review upstream release notes and runtime requirements, keep comments aligned with pins, and require CI before merging updates. Hosted `*-latest` runners and stable Rust intentionally move over time; record exact versions in investigations. The compiler version and lockfile participate in the Cargo cache key. Caches are performance aids, not proof of verification.

PR CI uses a read-only token, no checkout credentials, no project secrets, and no `pull_request_target` execution. Forked PRs run the same test suites. Keep that boundary when changing automation. Do not add publishing or deployment behavior as part of routine CI maintenance.

## Submit a useful pull request

Use the PR template to describe the problem, resulting behavior, technical decisions, validation commands and results, compatibility risks, documentation updates, and reviewer questions. Link real related issues when applicable. Explain skipped or unavailable checks; do not report unexecuted checks as passing. Screenshots are useful only for visual changes.

Keep protocol behavior, output privacy, bounded resource handling, and Linux/Windows behavior in mind. Do not weaken warnings, assertions, or coverage to obtain a green run. Preserve Cargo.lock for reproducible dependency resolution and isolate intentional dependency updates.

Conventional Commit examples:

```text
fix(proxy): reject invalid upstream framing
test(http): cover a connection reuse regression
docs: clarify observation latency boundaries
ci: preserve failed smoke evidence
```

Prefer focused commits that describe a real logical change. Do not force-push shared branches without coordinating with maintainers.

## Engineering and research standards

Use plain, accurate documentation. Distinguish observed facts, derived anomalies, and diagnostic hypotheses. Do not claim root causes, performance guarantees, or platform support without evidence. Experiments should record their hypothesis, setup, methodology, measurements, limitations, and reproduction steps.

Use the [usage guide](documents/EXAPROP_USAGE.md) and [observation contract](documents/M3_OBSERVABILITY.md) as references for current behavior. Historical engineering reports retain their original environment and scope; they do not replace checks on your change.
