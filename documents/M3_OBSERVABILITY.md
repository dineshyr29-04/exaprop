# M3 observation contract

This document defines the implemented observation, privacy, latency, and shutdown behavior. It supersedes descriptions in the historical [verification report](../docs/m3-observability-review.md). The broader [external test guide](M3_OBSERVABILITY_TESTING.md) remains available for manual experiments.

## Queue accounting

Observation delivery never waits for capacity. Each completed emission attempt increments exactly one process-local counter:

| Counter | Meaning |
| --- | --- |
| `enqueued` | The bounded channel accepted the record; not proof of output delivery |
| `dropped_full` | The channel had no capacity at the attempted send |
| `dropped_closed` | The observation consumer was closed |
| `dropped()` | Saturating sum of the two dropped counters |

The CLI uses a queue of 1,024 records. At proxy shutdown, stderr reports all counters, including zeros:

```text
observation_queue enqueued=25 dropped=0 full=0 closed=0
observation_output written=25 unconfirmed=0 drained=true
```

`written` counts successful complete line writes. `unconfirmed` is enqueued minus written, floored at zero. A partial write or output failure may leave a record unconfirmed. A successful flush is not durable storage, and these counters do not account for a request cancelled before emission or for records lost when the process is forcibly terminated.

Library callers can read live counters without parsing stderr:

```rust,no_run
use core_engine::{Observation, ObservationStats};
use tokio::sync::mpsc;

let (sender, mut receiver) = mpsc::channel::<Observation>(1024);
let stats = ObservationStats::default();
let live_stats = stats.clone();
// Pass `stats` to proxy::run_with_observer_and_stats(..., Some(sender), stats).
// Consume `receiver` concurrently, and read live_stats.snapshot() as needed.
```

`run_with_observer` remains available for existing callers. A fresh stats handle gives per-run totals; intentionally sharing one aggregates runs. Individual snapshot fields are read independently with relaxed atomics, so a live snapshot is not a transactional view of simultaneous updates. After producers stop, totals settle. There is no externally exposed metrics listener or persistent counter store.

## Exact latency semantics

`Observation::duration` measures proxy handler processing, using `std::time::Instant`.

1. Start: the first `Instant::now()` inside `handle_request`, after Hyper has parsed request headers and invoked the service.
2. End: `start_instant.elapsed()` when constructing the observation arguments at an emission point. This follows successful response buffering/reframing or creation of a proxy error response, and precedes query sanitization and channel submission.

The duration includes request-body buffering, request preparation, upstream connection establishment when needed, upstream request sending/response-header waiting, response-body buffering, and response construction. For a rejection or failure, it measures only the work completed up to that error decision and response construction. Small metadata preparation costs before the elapsed sample are included.

It excludes connection admission/backlog waiting, client TCP establishment, HTTP request-header reading/parsing, observation sanitization, channel enqueueing, stdout queueing/output, and downstream response delivery. It is neither backend-only latency nor client end-to-end latency. Streaming responses are buffered, so a pause between upstream body frames is included even when upstream headers arrived promptly.

Wall-clock `timestamp` is captured separately at handler entry. Duration remains monotonic if the system wall clock changes. Text output rounds milliseconds to three decimal places; the typed record retains `Duration` precision.

Timeouts are stage-specific: body reads wait up to 15 seconds for each next frame. They do not impose a 15-second total transaction deadline.

## Privacy policy

Observation filtering does not modify the request sent to the backend. Bodies and headers are excluded from the record schema. Paths remain visible.

For query filtering:

- Recognize ampersand and semicolon as potential separators while preserving their spelling and order in output.
- Decode parameter names once, including form-style `+` spaces, solely for classification.
- Normalize ASCII punctuation and spaces before matching sensitive names. For example, `a.p.i-k+e+y` is classified as an API key.
- Redact malformed escapes, invalid UTF-8, remaining percent escapes (including nested encoding), non-ASCII/confusable names, control characters, embedded query syntax, and empty/unclassifiable names conservatively.
- Match token, key, secret, auth, pass, session, sig, bearer, cred, pwd, jwt, and assertion substrings, plus exact `code`, `otp`, and `pin` names.
- Redact every occurrence of a matching name, including duplicates and valueless sensitive parameters. Keep empty separators unchanged.

Examples:

```text
%74oken=private          -> %74oken=[REDACTED]
%2574oken=private        -> %2574oken=[REDACTED]
a.p.i-k+e+y=private      -> a.p.i-k+e+y=[REDACTED]
p%D0%B0ssword=private    -> p%D0%B0ssword=[REDACTED]
page=2;pwd=private       -> page=2;pwd=[REDACTED]
```

This intentionally permits false positives. It is not comprehensive secret detection: unrecognized ASCII names, query values under those names, original parameter-name spelling, and paths may still reveal sensitive data. Values embedded in parameter names are not protected by value redaction. Do not treat observation output as anonymized data. The CLI separately escapes terminal control characters when printing text.

## Graceful shutdown and draining

The CLI handles Ctrl+C, Unix SIGTERM, and Windows Ctrl+Break. It stops accepting connections, allows up to two seconds for active connections, cancels remaining connection tasks, and drops producer handles. The output thread then consumes the channel through closure and flushes its writer. It has a two-second completion wait so a blocked output pipe cannot indefinitely hold the runtime open.

A normal writable observation sink produces `drained=true` and `unconfirmed=0`. Output failure or drain timeout reports `drained=false` and returns a nonzero exit status. That summary is itself a best-effort diagnostic and may be lost if stderr is blocked or closed. Forced termination, a permanently blocked sink, or cancelled handlers can still lose records. Windows service registration is not provided by the CLI.

## Bounded operational diagnostics

HTTP tasks and the accept loop submit diagnostics through `try_send`; they never write stderr or wait for diagnostic capacity. A separate OS thread owns stderr writes. The process-wide queue holds at most 256 messages, each limited to 1,024 UTF-8 bytes, plus one message currently being written. Truncation preserves UTF-8 boundaries. This bounds queued diagnostic payloads, not total process memory or arbitrary custom formatter execution time.

`core_engine::diagnostics::snapshot()` exposes `enqueued`, `written`, `dropped_full`, `dropped_closed`, `truncated`, `write_errors`, and `spawn_errors`. A full queue discards the new message; a closed queue discards later sends. A writer failure closes the receiver and may abandon accepted messages. `enqueued - written`, floored at zero, is unconfirmed output, including a potentially partial line. Counters are process-local independent atomic reads and settle after producers stop. No fallback writes to stdout are made.

At process exit, the CLI waits up to two seconds for queued diagnostics, the summary, and flush. A permanently blocked destination times out and returns exit code 1; the detached writer cannot hold the Tokio runtime open. The CLI returns an explicit exit code so Rust's implicit error reporting cannot make a synchronous stderr write. Normal shutdown prints, for example:

```text
diagnostics_queue enqueued=20 written=20 dropped_full=3 dropped_closed=0 truncated=0 write_errors=0 spawn_errors=0 unconfirmed=0
```

The summary excludes itself from message counters. A blocked or broken destination can prevent all summaries from being delivered; live library snapshots remain available without I/O. Drops alone are best-effort loss and do not make the CLI exit unsuccessfully; writer/drain failure does. Library callers should call `diagnostics::finish` only at process exit after all producers stop: successful completion permanently closes this process-wide sink.

Connection grace, observation-output wait, and diagnostic-output wait can add up to roughly six seconds plus overhead. Neither observation delivery nor diagnostic delivery is durable or guaranteed; both may drop messages, while HTTP processing continues.

## Real FastAPI smoke suite

The executable harness starts a real FastAPI application under Uvicorn, starts the compiled proxy, sends requests over TCP, shuts down the backend, requests graceful proxy shutdown, and validates the captured records and queue/output summaries. It owns and cleans up only its own child processes. Windows uses an isolated hidden console to deliver Ctrl+Break without signalling the user's terminal; Unix uses SIGTERM.

Create a Python 3.12 virtual environment and install the locked smoke dependencies:

```text
python -m venv .venv
```

Windows:

```powershell
.\.venv\Scripts\python.exe -m pip install -r examples/requirements-smoke.txt
cargo build --workspace --release --locked
.\.venv\Scripts\python.exe scripts/smoke_fastapi.py --include-timeouts
```

Linux/macOS:

```bash
.venv/bin/python -m pip install -r examples/requirements-smoke.txt
cargo build --workspace --release --locked
.venv/bin/python scripts/smoke_fastapi.py --include-timeouts
```

Omit `--include-timeouts` for the short run. The full run deliberately exercises both 15-second timeout paths. The fixture covers route parameters, query preservation, Authorization forwarding without recording it, binary POST echo, duplicate Set-Cookie headers, HEAD, upstream HTTP error statuses, delayed and streamed responses, request/response size boundaries, concurrent requests, and backend unavailability. Each case is matched to exactly one expected observation. The normal-output process checks redaction, latency lower bounds, zero queue drops, and complete graceful drain. A second proxy deliberately loses its stdout reader: HTTP forwarding must continue, closed-consumer drops must become measurable, and shutdown must return exit code 1 with `drained=false`.

Evidence is written to a unique directory under `tmp/fastapi-smoke/`: `result.json`, `observations.log`, `proxy-stderr.log`, and `backend.log`. `PASS` is printed only after all assertions succeed. Existing evidence directories are not overwritten. Linux and Windows CI run the same full suite and retain the evidence as job artifacts. Local Windows results do not imply that hosted Linux CI has already run.

## Earlier verification before final review fixes

On 22 October 2026, the Windows GNU build passed all 47 Rust tests (the previous 46 were retained), strict Clippy, formatting, compilation, debug/release builds, documentation with warnings denied, and a Python dependency consistency check. The final FastAPI 0.159.1 / Uvicorn 0.54.0 smoke campaign ran under Python 3.12.14 with `--include-timeouts` and passed in 35.672 seconds. The normal-output proxy reported:

| Result | Value |
| --- | --- |
| Explicitly checked HTTP requests | 37 |
| Delivered observations, including readiness | 38 |
| Accepted queue records | 38 |
| Full/closed queue drops | 0 / 0 |
| Complete output line writes | 38 |
| Unconfirmed output records | 0 |
| Graceful output drain | true |

Final combined-run evidence is retained locally at `tmp/fastapi-smoke/20261022-124412-16576`. This is a smoke result, not a throughput benchmark. Hosted Linux and Windows MSVC results remain pending a CI run. The redundant repository-local Rust toolchain was removed after the normal user installation was verified; test fixtures, existing Rust regressions, and requested documentation were preserved.

The same full run verified a separate proxy's real broken-pipe case: three closed-consumer drops were reported, one enqueued record remained unconfirmed, forwarding continued, and the affected proxy exited with code 1 rather than claiming a successful drain. Its diagnostics are in `broken-sink-stderr.log` alongside the normal-output evidence.

The [final engineering report](M3_FINAL_ENGINEERING_REPORT.md) supersedes these earlier counts with the five focused fixes and their new regression evidence. Run `python scripts/smoke_http_review.py` after building release to exercise malformed Connection fields, unsolicited upstream 101, preserved Via chains, HTTP 205 wire framing, and blocked/closed diagnostic destinations. It also runs the shipped echo backend through sequential, concurrent, binary, outage, and restart checks.
