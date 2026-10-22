# ExaProp M3 final engineering report

Review date: 22 October 2026. Branch: `m3-observability`. Starting and ending HEAD: `fdd0e0f` (the checkout already had eight unpushed commits and pending work). No commit, push, merge, branch creation, or branch switch was performed for this review.

**Verdict: ready for the next milestone's development, with the M3 limitations below.** All five approved corrections are implemented and the required local checks passed. No unresolved high-severity finding was identified in this focused review. This is not a public-production deployment certification or a claim that every operating system has been tested.

## Scope and exact files

This pass changed the following files. Paths are relative to the repository root; the final checks cover their current working-tree contents, including pending earlier work.

| File | Changes in this final pass |
| --- | --- |
| [core/src/diagnostics.rs](../core/src/diagnostics.rs) | New bounded diagnostics writer, counters, process-exit drain, and three regressions |
| [core/src/lib.rs](../core/src/lib.rs) | Export diagnostics module |
| [core/src/proxy.rs](../core/src/proxy.rs) | Replace synchronous diagnostics; validate Connection; reject upstream 101; append Via; explicit 205 framing; seven regressions |
| [cli/src/main.rs](../cli/src/main.rs) | Nonblocking operational diagnostics; bounded diagnostic drain; explicit ExitCode to avoid implicit synchronous error printing |
| [scripts/smoke_http_review.py](../scripts/smoke_http_review.py) | New executable protocol, blocked/closed stderr, and shipped echo-backend checks |
| [.github/workflows/ci.yml](../.github/workflows/ci.yml) | Add HTTP review smoke suite and preserve both smoke suites' evidence on Linux/Windows |
| [README.md](../README.md) | Correct 205 contract; document Via, malformed headers, diagnostic drops, deadlines, and guide/report links |
| [documents/M3_OBSERVABILITY.md](M3_OBSERVABILITY.md) | Add diagnostic accounting and drain contract; distinguish earlier evidence from this final review |
| [documents/M3_OBSERVABILITY_TESTING.md](M3_OBSERVABILITY_TESTING.md) | Replace stale synchronous-stderr limitation and four-second shutdown estimate |
| [docs/m3-observability-review.md](../docs/m3-observability-review.md) | Mark the prior synchronous-stderr finding historical and link its resolution |
| [documents/EXAPROP_USAGE.md](EXAPROP_USAGE.md) | New installation, local HTTP, FastAPI, Express, limits, output, and troubleshooting guide |
| [documents/M3_FINAL_ENGINEERING_REPORT.md](M3_FINAL_ENGINEERING_REPORT.md) | This report |

Five additional files were already pending at the start of this approved final pass and were retained without further source edits in this pass:

| File | Retained earlier work |
| --- | --- |
| [.gitignore](../.gitignore) | Remove redundant ignore entries |
| [core/src/observation.rs](../core/src/observation.rs) | Live queue counters and stronger encoded/unusual query-name filtering |
| [examples/fastapi_backend.py](../examples/fastapi_backend.py) | Real framework smoke fixture |
| [examples/requirements-smoke.txt](../examples/requirements-smoke.txt) | Pinned Python smoke dependencies |
| [scripts/smoke_fastapi.py](../scripts/smoke_fastapi.py) | FastAPI/observation smoke campaign and broken stdout scenario |

The total pending change set contains these 17 files: nine tracked modifications and eight new files. Generated builds, virtual environments, and `tmp/` evidence remain ignored. Existing test fixtures, documentation, and earlier pending fixes were preserved. No M4 behavior, TLS, fault injection, or unrelated routing refactor was added.

## Root causes and corrections

### 1. Blocked operational stderr stalled HTTP

The accept loop and request/error paths called synchronous `eprintln!`. When an unread stderr pipe filled, the accept loop could block inside a write, stopping further requests. The earlier reproduction completed 124 requests before stalling; draining about 4 KiB restored progress. The CLI's implicit Rust `Result` termination path could also write stderr synchronously on errors.

All proxy operational diagnostics now format at most 1,024 UTF-8 bytes and use `try_send` into a process-wide 256-message queue. A dedicated OS thread alone performs stderr writes. Full queues drop the new message; closed queues drop later sends. Counts distinguish full/closed drops, truncation, writer failures, thread-spawn failures, accepted messages, and completed writes. A writer failure may abandon accepted messages, reflected by accepted-minus-written unconfirmed output.

The CLI permits two seconds for diagnostic draining at process exit, after the connection and observation-output phases. It returns an explicit ExitCode rather than letting Rust print an error implicitly. A permanently blocked writer is detached and cannot keep the Tokio runtime alive. Drops alone are best effort and do not cause failure; output failure or an unsuccessful drain does. Summaries cannot be guaranteed when their own destination is blocked, so the library also exposes atomic snapshots without I/O. No stdout fallback corrupts observation output.

This is a small isolated output component; proxy routing, observation schema, and forwarding architecture remain intact.

### 2. Unsupported upstream 101 appeared successful

The response path previously forwarded an unsolicited `101 Switching Protocols`, although no upgrade tunnel was supported, and classified it as `Success`. It now constructs `502 Bad Gateway` immediately and emits `BackendFailure`. A backend's ordinary HTTP 502 remains a completed exchange (`Success`), preserving the transport/application distinction.

### 3. Invalid Connection values bypassed nominated-field filtering

The old parser silently skipped Connection values that failed string conversion and did not validate every list member as a field-name token. For example, `x-private,\xff` could bypass removal of the nominated private field.

The new parser validates every value and token before mutating headers. It accepts valid multiple field lines, comma-separated lists, SP/HTAB optional whitespace, and ignorable empty list members; it rejects non-ASCII, quoted, whitespace-containing, and colon-containing tokens. Valid tokens still nominate case-insensitive hop-by-hop removal. Invalid client values return 400 with `Rejected`; invalid upstream values return 502 with `BackendFailure`. Error diagnostics use static descriptions and do not echo offending values.

### 4. Forwarded messages lacked Via

The proxy previously omitted Via. Forwarded requests and responses now retain prior Via values and append the received HTTP version plus the fixed pseudonym `exaprop`: normally `1.1 exaprop`, or `1.0 exaprop` for received HTTP/1.0 messages. It does not add local hostnames, addresses, ports, service labels, or build versions. Existing Via contents are preserved, not anonymized. Headers validly nominated by Connection remain subject to hop-by-hop removal.

RFC 9110 requires an HTTP-to-HTTP gateway to add Via on inbound forwarded requests; response Via is optional for this role and is added consistently here. See [section 7.6.3](https://www.rfc-editor.org/rfc/rfc9110#section-7.6.3).

### 5. HTTP 205 documentation confused content and framing

The prior README grouped 205 with 204 as prohibiting Content-Length. HTTP 205 forbids content, but `Content-Length: 0` is permitted. The implementation now makes zero-length framing explicit, suppresses content, and sends no Transfer-Encoding. HTTP 204 continues to omit Content-Length. The raw TCP regressions assert actual serialized bytes rather than only the in-memory response object. See [RFC 9110 section 8.6](https://www.rfc-editor.org/rfc/rfc9110#section-8.6) and [section 15.3.6](https://www.rfc-editor.org/rfc/rfc9110#section-15.3.6).

## New Rust regression tests

Three diagnostics tests in `core/src/diagnostics.rs`:

- `blocked_writer_drops_at_capacity_without_blocking_producer`
- `broken_writer_counts_failure_and_closed_sends`
- `rendered_messages_are_bounded_and_valid_utf8`

Seven protocol tests in `core/src/proxy.rs`:

- `malformed_client_connection_headers_are_rejected_and_observed`
- `unsolicited_upstream_switch_is_a_backend_failure`
- `malformed_upstream_connection_headers_are_backend_failures`
- `connection_lists_validate_atomically_and_accept_empty_members_and_ows`
- `via_chains_host_and_duplicate_headers_survive_forwarding`
- `via_uses_received_protocol_and_a_non_sensitive_pseudonym`
- `reset_content_has_zero_length_framing_on_the_wire`

The earlier 47 Rust tests were retained. Existing `counts_full_and_closed_observer_drops` and `full_and_closed_observers_do_not_block_forwarding` passed again: completed HTTP forwarding does not wait for observation consumers or channel capacity. The latter exercised two full-channel drops and one closed-channel drop with one accepted record.

## Actual local results

Environment: Windows x86-64, Rust/Cargo 1.99.1, target `x86_64-pc-windows-gnu`, Python 3.12.14, FastAPI 0.159.1, Uvicorn 0.54.0. Local socket tests were executed outside the restrictive socket sandbox. Commands ran from the repository root with the installed Rust tools on PATH and `CARGO_INCREMENTAL=0` to avoid sandbox hard-link warnings.

| Required check | Actual result |
| --- | --- |
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy --workspace --all-targets -- -D warnings` | PASS, no warnings |
| `cargo test --workspace` | PASS: 54 core + 3 CLI = 57 tests; 0 failures; 0 doctests |
| `cargo build --release` | PASS |
| `python scripts/smoke_fastapi.py --include-timeouts` | PASS: 37 checked requests, 38 observations including readiness; 35.859 seconds |
| `python scripts/smoke_http_review.py` | PASS: protocol suite, three stderr modes, and shipped echo backend |

Additional checks passed: `cargo doc --workspace --no-deps` with `RUSTDOCFLAGS=-D warnings`, Python `pip check`, Python compilation of both smoke scripts and the FastAPI fixture, 49 relative documentation links across six Markdown files, and `git diff --check`. Git emitted line-ending normalization notices, not whitespace errors.

Initial development iterations found and corrected Clippy suggestions (`contains` and `next_back`) and formatting differences. One initial HTTP smoke run timed out in the Windows console-signal helper. Both isolated original/instrumented helper checks then passed; the unchanged helper subsequently passed every proxy shutdown in the complete HTTP and FastAPI reruns. Its initial timeout was not reproduced or assigned a proven root cause. The failed evidence is retained; it was not counted as a pass and no timeout assertion was relaxed.

### Protocol and real-backend evidence

Final HTTP evidence: `tmp/http-review/20261022-142627-16996/`.

- 16 explicitly checked protocol requests and 17 observations including readiness, with complete observation drain and zero queue drops.
- Four malformed client Connection forms returned 400/Rejected; three malformed upstream forms returned 502/BackendFailure.
- Unsolicited upstream 101 returned 502/BackendFailure.
- Existing comma-separated and repeated Via values remained in order, followed by the pseudonym. Host and duplicate Set-Cookie fields were retained; nominated request and response hop fields were removed.
- Two transactions on one frontend connection also reused the backend TCP connection.
- HEAD, 204, 205, and 304 contained no content. `http205-wire.txt` records HTTP/1.1 205, `content-length: 0`, no Transfer-Encoding, and no bytes after the header terminator.
- The shipped `examples/echo_server.py` handler passed GET/query, JSON POST, HEAD, binary echo, three sequential requests, five concurrent requests, backend outage returning 502, and recovery after backend restart.

Final FastAPI evidence: `tmp/fastapi-smoke/20261022-142701-31128/`.

- Route/query preservation, Authorization forwarding without recording it, encoded/unusual-name redaction, binary POST, duplicate cookies, HEAD, upstream 404/500/502, delayed/buffered streaming response, exact/over-limit bodies, 20 concurrent requests, backend unavailability, and both real 15-second timeout paths passed.
- Normal observation output: accepted 38, written 38, full/closed drops 0/0, unconfirmed 0, drained true, exit 0.
- Normal diagnostics: accepted 43, written 43, full/closed drops 0/0, unconfirmed 0.
- Separate broken-stdout proxy kept forwarding, reported three closed-consumer observation drops and one unconfirmed accepted record, and exited 1 with `drained=false`. This proves unsuccessful output is not silently reported as a successful drain.

### Reproduction of blocked and broken stderr

All modes used the compiled release CLI and real TCP requests, not a mocked logger. The stderr pipe was deliberately left unread during traffic; the finite pipe and queue capacities caused pressure well beyond the earlier 124-request failure point.

| Scenario | HTTP result | Output/shutdown result |
| --- | --- | --- |
| Leave stderr unread, then resume drainage | 600/600 requests returned 200, plus readiness | 386 diagnostics accepted and written; 219 full-queue drops; no closed drops; exit 0 |
| Leave stderr unread through shutdown | 600/600 returned 200, plus readiness | Exit 1 after 2.094 seconds from signal-helper invocation; diagnostic drain bounded; no claim of complete output |
| Close stderr reader before traffic | 10/10 returned 200, plus readiness | Exit 1 after 0.265 seconds; broken diagnostic sink did not stall forwarding |

The gate-writer unit regression separately proves bounded producer behavior deterministically. The real-pipe run demonstrates the operating-system failure scenario. These are functional smoke checks, not latency or throughput benchmarks.

### Can observations or diagnostics be dropped?

**Yes, both are intentionally best effort.** Observation sends use `try_send`; a full or closed queue loses the new record and increments the matching counter. Output failure, cancellation before emission, forced termination, or a bounded drain timeout can also lose accepted/unemitted data. Diagnostic sends have the equivalent full/closed drop policy, plus truncation and writer/spawn failure counters. Diagnostic output failures may abandon accepted messages. Neither channel blocks HTTP waiting for capacity or its destination.

Queue-drop counters distinguish failed sends from accepted-but-unconfirmed output; they are not a complete request census. Process-local snapshots are independently read atomics. Summaries themselves may be dropped or unwritable; no reliable external metrics endpoint or persistence is claimed.

## Remaining limitations and readiness boundary

- Only Windows GNU was executed in this local pass. Linux and Windows MSVC CI are configured to run all suites but remain unexecuted here; macOS was not tested. Build a native binary for each intended OS/architecture. The earlier isolated Windows helper timeout remains an intermittent test-infrastructure observation to monitor in CI.
- The practical Express recipe was documented but not executed. The real framework test was FastAPI; the additional shipped backend was Python's HTTP server.
- Plain HTTP/1.x, one numeric backend, no DNS targets, HTTPS, HTTP/2, CONNECT, WebSockets, streaming/SSE support, or service installation. No M4/fault injection behavior was implemented.
- Bodies are buffered to 4 MiB in each direction. Sixteen active frontend connections include idle keep-alive connections. These are resource controls, not a measured total memory ceiling or a fairness guarantee.
- Body timeouts apply to each frame; a progressing slow body can exceed 15 seconds overall. There is no total transaction deadline or universal downstream write deadline.
- Shutdown may cancel connections after two seconds. The connection, observation, and diagnostic phases can take about six seconds together plus process/scheduling overhead. Forced exit or blocked sinks can lose records.
- HTTP parser rejections and cancelled handlers may have no observation. `Success` covers completed upstream HTTP error responses. Duration excludes client response delivery and is not client end-to-end latency.
- Privacy handling is conservative but heuristic. Paths, original query-name spelling, and unrecognized ASCII query values may expose sensitive data. Existing Via content is preserved, not scrubbed.
- No authentication, public-edge hardening, durable telemetry, performance characterization, or cross-service inference is asserted. These remain outside the approved fixes.

There are no known unresolved high-severity findings among the five approved issues after these regressions. M3's reviewed observation and forwarding foundation is ready to support further development under the documented limits. The README retains the broader roadmap's in-progress status for capabilities such as planned anomaly detection; this verdict does not declare those capabilities implemented.

Before claiming verified Linux deployment, run the configured Linux CI job and inspect its retained smoke evidence. No repository changes were committed or published during this review.
