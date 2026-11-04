# M3 observability verification report

Review date: 22 October 2026 (Asia/Calcutta)

Historical verification snapshot: later refinements to counters, privacy, signal handling, and real-backend checks are defined in the current [M3 observation contract](../documents/M3_OBSERVABILITY.md). The test counts and limitations below describe the original review, not a replacement for current run results.

## Scope and repository state

- Branch: `m3-observability`.
- Starting commit: `9bed697` — structured HTTP request observability.
- Initial `git status --short --branch`: clean, tracking `origin/m3-observability`.
- The requested branch was already checked out; no checkout or reset was needed.
- Reviewed the observation model, proxy request paths, CLI output and lifecycle, manifests, regression suite, README, contributor checks, and workflows.
- At the original review handoff, changes were local and uncommitted; no push or merge had been performed. This report records that verification snapshot.

## Assessment

The branch implements structured request observations, not the complete future observability platform. Records contain timestamps, service labels, methods, paths, sanitized queries, status codes, monotonic durations, and transport/proxy outcomes. An upstream HTTP error remains a completed exchange; a proxy-generated error has a separate outcome.

The review found and corrected privacy, delivery, output, protocol, and test reliability issues. The revised suite contains **46 tests**, compared with 35 at the starting commit. All 46 passed on Windows with localhost socket access enabled.

## Findings and changes

| Finding | Change | Evidence |
| --- | --- | --- |
| Percent-encoded sensitive query names could bypass redaction | Decode names once for matching; preserve original spelling; redact malformed encodings conservatively | Sanitizer tests and a proxy test that also verifies the backend receives the original query |
| Full or closed observation channels silently discarded records | Retain nonblocking delivery and count dropped records per proxy run; report the count at shutdown | Full/closed channel counter test and forwarding test with a saturated or closed observer |
| Synchronous stdout writes ran inside a Tokio task | Move output to a dedicated thread with fallible writes | Output error regression and static review of thread ownership |
| Shutdown immediately aborted the printer | Allow queued records to drain after senders close, with a two-second completion wait | Queued-record drain test and occupied-address CLI startup smoke check |
| Service labels could inject terminal controls or additional log lines | Escape service, path, and query text at the output boundary | Output regression includes newline and escape characters |
| CLI output omitted timestamp and truncated sub-millisecond durations | Print Unix timestamp and milliseconds to three decimal places | Output regression checks timestamp and 0.125 ms formatting |
| Upgrade detection inspected only the first Connection value | Inspect all Connection header values | Raw HTTP regression with an upgrade token in a later header |
| Missing observations could hang tests; shutdown helper ignored errors | Bound observation receives and assert successful server task completion | Helpers used throughout the networking regression suite |
| Existing body-privacy test only constructed a record | Exercise real request/response bodies and sensitive headers through the proxy | Assertions verify forwarding while observation debug output excludes private data |
| Timeout regression used a narrow 20 ms budget | Increase the timeout budget and backend delay | Backend timeout observation regression |
| README described older features and overstated guarantees | Carry forward the professional README and document M3 behavior and limits | Local link validation and implementation comparison |
| No Rust verification workflow existed | Add Linux/Windows checks for formatting, compilation, tests, Clippy, release build, and documentation | Workflow source added; hosted execution remains pending publication |

## Verification results

Commands used the existing dependency lockfile. Cargo commands ran offline after fetching the locked dependencies.

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `cargo check --workspace --locked --offline` | Passed |
| `cargo test --workspace --locked --offline` | Passed: 43 library tests and 3 CLI tests; no failures |
| Documentation tests | Passed; no documentation test cases are currently defined |
| `cargo clippy --workspace --all-targets --all-features --locked --offline -- -D warnings` | Passed |
| `cargo build --workspace --locked --offline` | Passed |
| `cargo build --workspace --release --locked --offline` | Passed |
| `cargo doc --workspace --no-deps --locked --offline`, with `RUSTDOCFLAGS=-D warnings` | Passed |
| CLI top-level and attach help | Passed; addresses and service-label defaults displayed |
| CLI invalid backend address | Passed; usage error returned exit code 2 |
| CLI occupied listen address | Passed; startup error returned exit code 1 and output shutdown completed |
| `git diff --check` | Passed |
| README relative file links | Passed: 6 relative file targets exist |

### Environment and initial failures

Rust and Cargo were absent from PATH and the usual user toolchain location. WSL had no configured distribution. An isolated Rust 1.99.1 toolchain for `x86_64-pc-windows-gnu`, including rustfmt and Clippy, was installed under the ignored `tmp/m3-toolchain` directory. System PATH was not changed. Dependencies were downloaded using the lockfile; the lockfile was not modified.

The first networking test run failed with Windows socket permission error 10013 inside the sandbox. The permitted rerun outside the sandbox passed all 46 tests. These initial failures were environmental socket restrictions, not failing HTTP assertions. Initial dependency downloads also required permitted network access.

## Coverage

The suite exercises:

- Successful GET and POST observations, service/method/path tagging, monotonic duration, concurrent records, and one record per completed request when delivery capacity is available.
- Completed upstream HTTP 500/502 responses versus proxy-generated backend failures, backend timeout, request rejection, oversized client body, and client body timeout.
- Sensitive plain and encoded query names, duplicate names, case variants, malformed encoding, empty query structure, and preservation of the forwarded query.
- Exclusion of bodies and headers from records, queue saturation, closed observers, dropped-record accounting, queued-output draining, terminal escaping, and broken output sinks.
- HTTP paths/queries, binary payloads, duplicate headers, chunked request framing, HEAD and bodyless responses, exact body-size boundary, oversized bodies, malformed HTTP, hop-by-hop filtering, unsupported features, concurrency, and client disconnect recovery.
- CLI service-name parsing, default addresses, invalid address handling, help, and bind errors.

## Remaining limitations

1. **Best-effort delivery:** a bounded in-memory queue can drop records. The counter covers failed channel sends, not requests cancelled before record creation or a record already dequeued when output fails. There is no durable storage or external metrics API for the counter.
2. **Observation boundary:** duration ends at response buffering, not client delivery. Parser-level rejections and cancelled handlers may have no observation. Response status records the proxy's selected response, not proof that the client received it.
3. **Privacy scope:** paths and unrecognized query values remain visible. Redaction uses a heuristic and a single form-decoding pass, not configurable allowlisting or comprehensive secret detection. Headers and bodies are excluded from observations, but the proxy necessarily processes them while forwarding.
4. **Timeout semantics:** body timeouts reset for each frame. There is no overall transaction deadline. A sufficiently slow but progressing stream can occupy a connection for longer than 15 seconds.
5. **Output lifecycle (historical finding, fixed in final review):** draining is bounded; a blocked stdout sink may lose queued records at exit. Operational stderr was synchronous at this review point. The [final report](../documents/M3_FINAL_ENGINEERING_REPORT.md) records its replacement with bounded nonblocking diagnostics and actual blocked-pipe regressions.
6. **Protocol scope:** plain HTTP/1.1, one backend, fully buffered bodies, and unsupported upgrades/trailers remain the current contract. Broader protocol conformance is not established by this suite.
7. **M3 scope:** no anomaly detector, persistent exporter, cross-service causal correlation, or user-facing dashboard is implemented. Fault injection and propagation scoring remain later work.
8. **Verification scope:** local results cover Windows GNU. Linux and Windows hosted CI are configured but have not run for these local changes. No load benchmark, fuzz campaign, dependency vulnerability audit, or production-readiness certification was performed.

The passing suite validates the tested behavior; it is not a guarantee that all possible workloads or failure modes are covered.

## Reproduction

With Rust, rustfmt, and Clippy available, run:

```bash
cargo fmt --all -- --check
cargo check --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo build --workspace --locked
cargo build --workspace --release --locked
```

Then build documentation with `RUSTDOCFLAGS=-D warnings` set in the environment:

```bash
cargo doc --workspace --no-deps --locked
git diff --check
```

Networking tests require permission to bind and connect to local ephemeral ports. In PowerShell, set documentation flags with `$env:RUSTDOCFLAGS = '-D warnings'` before running the documentation command.

## User-wide development setup follow-up

At the user's request, Rust was subsequently installed in the normal user account rather than relying on the temporary repository toolchain:

- Rustup home: `%USERPROFILE%\.rustup`.
- Cargo and tool shims: `%USERPROFILE%\.cargo\bin`, added to the persistent user PATH.
- Default toolchain: stable Rust 1.99.1 for `x86_64-pc-windows-gnu` at verification time.
- Installed components: Cargo, rustc, standard library, GNU linker tools, rustfmt, Clippy, rust-src, and rust-analyzer.
- Tool availability was verified from a directory outside this repository with the persistent machine/user PATH and no CARGO_HOME or RUSTUP_HOME overrides.
- Restart an existing terminal or editor to pick up the updated PATH. New sessions can use Rust tools from any folder in this user account.

The repository now includes `rust-toolchain.toml` to select stable Rust with rustfmt and Clippy. Stable is a moving channel, so this file selects the toolchain and required checks without pinning an exact compiler release.

`scripts/check.ps1` runs the complete check sequence and stops at the first failure. It resolves the project directory from its own location, so it can also be invoked by absolute path from another directory. The script restores the caller's working directory and documentation flags when it finishes.

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/check.ps1
```

The execution policy flag affects only that PowerShell process; it does not change the computer's persistent execution policy. The GNU toolchain successfully built this project without a separate Visual Studio C++ installation; other projects with native dependencies may have additional requirements.

The script was invoked through Windows PowerShell from outside the repository using the user-wide installation. All steps passed: formatting, compilation, all 46 tests, strict Clippy, debug build, release build, and documentation with warnings denied. The persistent PATH resolved Cargo, rustc, and rust-analyzer to `%USERPROFILE%\.cargo\bin`. No temporary toolchain environment overrides were needed.
