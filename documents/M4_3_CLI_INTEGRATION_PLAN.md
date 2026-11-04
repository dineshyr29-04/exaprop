# M4.3 — CLI Integration & Anomaly Presentation Engineering Specification

## Document Metadata
- **Milestone:** M4.3 (CLI Integration and Anomaly Presentation)
- **Target Repository:** `exaprop` (Rust Workspace)
- **Status:** Proposed Implementation Specification (Reviewed & Hardened)
- **Author:** Lead Systems & Reverse-Proxy Architect
- **Baseline Commit:** `1803dd539b73b05782d67546e0691b34f7bcd543` (`m4-pipeline`)
- **Target Delivery Branch:** `m4-cli-integration`

---

## 1. Executive Summary & Objective

The objective of **Milestone M4.3** is to wire ExaProp's consumer-side telemetry pipeline (developed and hardened in M4.2) into the user-facing CLI binary (`exaprop attach`), exposing deterministic anomaly detection through robust, strictly validated CLI options while preserving the battle-tested HTTP/1.x reverse proxy core.

### Core Goals:
1. **Public CLI Enablement:** Expose `--detect` and `--latency-threshold-ms <INTEGER>` flags on `exaprop attach`.
2. **Legacy Compatibility:** Preserve M3 behavior, avoid detector instantiation, and retain existing output formatting when `--detect` is omitted; do not promise literally zero overhead.
3. **Decoupled Telemetry Execution:** Connect proxy observation producers to the `TelemetryPipeline` (`run_coordinator` + `run_writer`) with bounded queues, non-blocking dispatch, and zero backpressure on HTTP request forwarding.
4. **Machine-Readable Presentation:** Emit versioned, distinguishable, strictly bounded `OBSERVATION_V1` and `ANOMALY_V1` records on `stdout`.
5. **Security & Privacy Perimeters:** Exclude paths, queries, headers, and request/response payloads from anomaly lines; escape untrusted presentation fields. Treat service identifiers and methods as potentially sensitive and restrict access to telemetry.
6. **Graceful Supervised Lifecycle:** Enforce one 2-second telemetry shutdown *wait* deadline, verify counters only after successful settlement, and report diagnostics to `stderr`. A blocked synchronous writer may outlive the deadline.

---

## 2. CLI Interface & Flag Contract

### 2.1 Invocation Grammar

The existing `attach` subcommand is preserved and extended:

```bash
exaprop attach \
  --listen <SOCKET_ADDR> \
  --target <SOCKET_ADDR> \
  --name <SERVICE_NAME> \
  [--detect] \
  [--latency-threshold-ms <INTEGER>]
```

### 2.2 Flag Specification

| Flag | Type | Default | Permitted Values | Contract & Validation |
| :--- | :--- | :--- | :--- | :--- |
| `--listen` | `SocketAddr` | `127.0.0.1:8080` | Valid socket address | Address where proxy accepts incoming client connections. |
| `--target` | `SocketAddr` | `127.0.0.1:8081` | Valid socket address | Upstream HTTP/1.1 backend server address. |
| `--name` | `String` | `backend-service` | String | Logical service identifier for tagging observations. |
| `--detect` | `bool` | `false` | Presence flag (`--detect`) | Explicit opt-in for deterministic anomaly detection and V1 formatting. |
| `--latency-threshold-ms`| `u64` | `None` (`1000` if `--detect` set) | `1..=3_600_000` (1 ms to 1 hour) | Duration threshold for the `ElevatedRequestDuration` rule. **Forbidden if `--detect` is absent.** |

### 2.3 Strict Pre-Flight Validation Rules

Validation occurs immediately upon CLI parsing **before** any network sockets are bound or background workers are spawned. Any violation immediately aborts execution with exit code `2` (`EX_USAGE`) and emits a clear, actionable error message to `stderr`:

```
Validation Rules:
1. Threshold without detection:
   IF latency_threshold_ms.is_some() AND NOT detect:
     ABORT: "--latency-threshold-ms requires --detect to be enabled"

2. Zero threshold:
   IF latency_threshold_ms == Some(0):
     ABORT: "latency threshold must be at least 1 ms"

3. Upper bound violation:
   IF latency_threshold_ms > Some(3_600_000):
     ABORT: "latency threshold cannot exceed 3,600,000 ms (1 hour)"

4. Non-numeric / parse failure:
   Clap automatically rejects invalid integers with exit code 2.
```

### 2.4 Security-Sensitive CLI Policy

- Preserve the current loopback listen default. When the user explicitly selects a non-loopback listener, emit a clear bounded warning: ExaProp does not automatically add TLS, authentication, or access control. Do not silently change the bind address.
- Define and test duplicate-flag behavior using Clap (prefer rejection for duplicate `--latency-threshold-ms`); avoid ambiguous last-value-wins behavior.
- Validate `--name` as a nonempty, bounded service identifier before spawning workers. Choose and document an explicit encoded-byte length policy (max 128 bytes). Reject control characters and newlines in names by policy; **never claim validation can reliably detect every secret**.
- Preserve the existing `--target` socket-address contract rather than introducing URL parsing without a separate design decision.
- Validate before binding sockets or spawning telemetry workers. Keep diagnostics free of unescaped user-provided arguments.
- Ensure both M3 and detection-mode output remain appropriately access-controlled; the legacy path may expose raw request metadata.

---

## 3. Telemetry Pipeline Architecture & Concurrency

### 3.1 Dual-Mode Execution Architecture

```text
                                  +------------------------------------+
                                  |         exaprop attach             |
                                  +-----------------+------------------+
                                                    |
                                          [CLI Argument Parse]
                                                    |
                         +--------------------------+--------------------------+
                         |                                                     |
                 [--detect == false]                                   [--detect == true]
                         |                                                     |
             +-----------v------------+                             +----------v-----------+
             |    M3 Mode Pipeline    |                             |    M4.3 V1 Pipeline   |
             +-----------+------------+                             +----------+-----------+
                         |                                                     |
            +------------v-----------+                            +------------v------------+
            |  Proxy HTTP Engine     |                            |  Proxy HTTP Engine      |
            |  (Observation tx)      |                            |  (Observation tx)       |
            +------------+-----------+                            +------------+------------+
                         | (try_send)                                          | (try_send)
            +------------v-----------+                            +------------v------------+
            | Input Queue (1,024)    |                            | Input Queue (1,024)     |
            +------------+-----------+                            +------------+------------+
                         |                                                     |
            +------------v-----------+                            +------------v------------+
            | Single stdout Worker   |                            | Dedicated Coordinator   |
            | (legacy M3 formatting) |                            | (Detection & Rendering) |
            +------------+-----------+                            +------------+------------+
                         |                                                     | (nonblocking try_send)
            +------------v-----------+                            +------------v------------+
            | stdout                 |                            | Output Batch Queue (256)|
            +------------------------+                            +------------+------------+
                                                                               |
                                                                  +------------v------------+
                                                                  | Dedicated Writer Worker |
                                                                  | (complete line writes)  |
                                                                  +------------+------------+
                                                                               |
                                                                  +------------v------------+
                                                                  | stdout                  |
                                                                  +-------------------------+
```

### 3.2 Thread Isolation & Nonblocking Guarantees

1. **HTTP Proxy Core (`Tokio Worker Pool`)**:
   - Executes nonblocking HTTP reverse proxy forwarding (`hyper`).
   - Produces `Observation` values on request completion.
   - Enqueues via `mpsc::Sender::try_send`.
   - **Guaranteed Invariant:** An overloaded, stalled, or broken telemetry subsystem drops observation records at the queue boundary without introducing latency or backpressure to HTTP requests.

2. **Coordinator Worker (`Dedicated OS Thread`)**:
   - Owns the `mpsc::Receiver<Observation>`.
   - Assigns strictly monotonic sequence numbers (`seq`).
   - Invokes pure anomaly classification via `core_engine::detection::Detector`.
   - Renders pre-bounded byte slices directly into an `OutputBatch`.
   - Dispatches batches to the output queue using non-blocking `try_send`.
   - Updates `DetectionStats` using race-safe CAS atomic increments.

3. **Writer Worker (`Dedicated OS Thread`)**:
   - Owns the `mpsc::Receiver<OutputBatch>`.
   - Writes complete pre-rendered byte lines to `io::stdout()` using `write_all`.
   - Flushes output upon stream closure or termination.
   - Increments line completion counters only upon complete, error-free writes.

---

## 4. Output Presentation & Grammar Specification

When `--detect` is active, ExaProp outputs structured, deterministic, machine-readable lines to `stdout`.

### 4.1 Normal Observation Line (`OBSERVATION_V1`)

- **Byte Limit:** Strictly bounded to `NORMAL_LINE_CAP` (4,096 bytes).
- **Prefix:** `OBSERVATION_V1 `
- **Grammar:**
```text
OBSERVATION_V1 seq=<U64> timestamp=<S_SECS>.<NANOS> service="<ESC_STR>" method="<ESC_STR>" path="<ESC_STR>" <QUERY_FIELD> status=<U16> outcome=<OUTCOME> duration_ms=<F64_FIXED> truncated_fields=<TRUNC_FIELDS>\n
```

Where:
- `<QUERY_FIELD>`: `query="<ESC_STR>"` if query is present, or `query=none` if absent.
- `<OUTCOME>`: One of `Success`, `BackendFailure`, `BackendTimeout`, `ClientTimeout`, `ClientFailure`, `Rejected`.
- `<DURATION_MS>`: Formatted with microsecond precision (`{millis}.{micros:03}{nanos:03}`).
- `<TRUNC_FIELDS>`: `none` or comma-separated list (`service`, `method`, `path`, `query`, `path_query`).

### 4.2 Anomaly Line (`ANOMALY_V1`)

- **Byte Limit:** Strictly bounded to `ANOMALY_LINE_CAP` (1,024 bytes).
- **Prefix:** `ANOMALY_V1 `
- **Grammar:**
```text
ANOMALY_V1 seq=<U64> timestamp=<S_SECS>.<NANOS> service="<ESC_STR>" method="<ESC_STR>" status=<U16> outcome=<OUTCOME> origin=<ORIGIN> severity=<SEVERITY> duration_ms=<F64_FIXED> reasons=<REASONS> <THRESHOLD_FIELD> truncated_fields=<TRUNC_FIELDS>\n
```

Where:
- `<ORIGIN>`: `backend` (if outcome is `Success`) or `proxy` (all other outcomes).
- `<SEVERITY>`: `warning` (duration-only) or `error` (5xx or upstream timeout).
- `<REASONS>`: Comma-separated list in canonical order: `http_server_error,elevated_request_duration,upstream_timeout`.
- `<THRESHOLD_FIELD>`: `threshold_ms=<U64>` if duration rule matched; `threshold_ms=none` otherwise.
- `<TRUNC_FIELDS>`: `none` or comma-separated list (`service`, `method`).

---

## 5. Security & Privacy Architecture

### 5.1 Anomaly Line Privacy Isolation (Zero Payload Leakage)

`ANOMALY_V1` records minimize request-data exposure, but **must not be treated as public or safe for unrestricted distribution**. Service names and methods are included; configured identifiers can contain sensitive data. Apply access controls and retention limits to telemetry sinks.
- **Strictly Excluded Fields:**
  - Request Path (e.g. `/api/v1/users/12345`)
  - Query Strings & Parameters (e.g. `?token=secret`)
  - HTTP Request & Response Headers (e.g. `Authorization`, `Cookie`, `Set-Cookie`)
  - Request and Response Payloads / Bodies
- **Correlation Reference:** Correlation between an anomaly and its originating observation is performed exclusively via the ephemeral, run-local `seq` number.

### 5.2 Terminal Control Injection Defense

Untrusted strings provided by clients or upstream servers (such as custom HTTP methods, malformed paths, or service names) can contain ANSI escape sequences (e.g. `\x1b[2J`, `\x1b[31m`) or control characters that compromise terminal integrity.

- **Mitigation:**
  Every string field rendered in V1 passes through `escape_and_bound()`.
  - Double quotes (`"`) -> `\"`
  - Backslashes (`\`) -> `\\`
  - Newlines (`\n`) -> `\n`
  - Carriage returns (`\r`) -> `\r`
  - Tabs (`\t`) -> `\t`
  - Non-ASCII or ASCII control chars (< 0x20 or == 0x7F) -> `\xHH` hexadecimal escape byte sequence.
  - No raw ASCII `0x1B` (`ESC`) is ever emitted.

### 5.3 Bounded Allocation & Release-Mode Buffer Safety

- Rendering relies on `BoundedLine`, implementing `std::fmt::Write` with a hard remaining-byte counter.
- If a line would exceed its byte budget (`NORMAL_LINE_CAP` = 4,096; `ANOMALY_LINE_CAP` = 1,024), formatting immediately fails with `RenderLimit` rather than allocating unboundedly.
- `OutputBatch::new` validates line bounds in release mode. This limits serialized output size; it does **not** prove the absence of all memory-safety or resource-exhaustion issues.

---

## 6. Scaling & Performance Architecture

### 6.1 Predictable Memory Footprint

The telemetry subsystem bounds **queue item counts and serialized output line lengths**, not total process resident memory. In particular, `Observation` fields can allocate variable amounts of memory unless input limits are separately enforced.

| Component | Bound | Worst-Case In-Flight Memory |
| :--- | :--- | :--- |
| M3 Input Queue | 1,024 `Observation` records | Variable: depends on retained observation strings and allocation overhead |
| Output Batch Queue | 256 `OutputBatch` items | Max 1.25 MiB (256 * (4,096 + 1,024) bytes) |
| Active Rendering Buffer | 1 per coordinator thread | Serialized lines capped; additional intermediate strings and allocations exist |
| Telemetry Statistics | `Arc<DetectionStats>` | Small fixed structure; exact size and alignment are target-dependent |
| **Total Telemetry RSS Ceiling** | **Not established** | **Do not assert a fixed RSS ceiling without measured and enforced input bounds** |

### 6.2 Low-Contention Atomic Counters

- `DetectionStats` employs atomic `AtomicU64` and `AtomicBool` counters.
- Counter increments in `inc()` use weak compare-and-swap (`compare_exchange_weak`) loops that check for `u64::MAX`. If `u64::MAX` is reached, `counters_saturated` is marked `true` and the value remains saturated at `u64::MAX`, preventing arithmetic wrap-around.
- Snapshots (`DetectionStatsSnapshot`) use relaxed atomic loads without locks. They are not an atomic multi-counter transaction, and no sub-microsecond timing guarantee is made without benchmarks.

---

## 7. Lifecycle & Graceful Supervised Teardown

ExaProp enforces a strictly coordinated multi-stage shutdown process with an overall bounded budget.

```text
+-------------------------------------------------------------------------+
|                  Shutdown Initiation (Ctrl+C / SIGTERM)                 |
+------------------------------------+------------------------------------+
                                     |
                                     v
+-------------------------------------------------------------------------+
| 1. HTTP Proxy Listener Shutdown (Stop accepting new connections)        |
|    - 2-second in-flight connection grace period                         |
|    - Cancel remaining pending handlers                                  |
|    - Proxy drops Observation Sender (obs_tx)                            |
+------------------------------------+------------------------------------+
                                     |
                                     v
+-------------------------------------------------------------------------+
| 2. Telemetry Pipeline Supervised Teardown (Shared 2-second deadline)    |
|    - Pipeline drops its own clone of obs_tx                             |
|    - Coordinator receives until EOF, renders batches, drops output_tx   |
|    - Writer receives until EOF, writes complete lines, flushes stdout   |
|    - Wait deadline: if coordinator + writer exceed 2.0s, return timeout |
|      (blocked OS writes may remain blocked until process termination)   |
+------------------------------------+------------------------------------+
                                     |
                                     v
+-------------------------------------------------------------------------+
| 3. Operational Accounting & Diagnostic Reporting (stderr)               |
|    - Verify settled invariants only if coordinator and writer completed |
|    - Emit detection_stats diagnostic summary                            |
|    - Emit anomaly_output diagnostic summary with explicit state         |
+------------------------------------+------------------------------------+
                                     |
                                     v
+-------------------------------------------------------------------------+
| 4. Process Diagnostics Drain & Exit                                     |
|    - 2-second diagnostics queue drain to stderr                         |
|    - Return ExitCode::SUCCESS or ExitCode::FAILURE                      |
+-------------------------------------------------------------------------+
```

### 7.0 Ownership and Shutdown Ordering

- Explicitly track every `mpsc::Sender<Observation>` clone held by the listener, active handlers, and pipeline.
- Stop accepting requests and apply the existing connection grace period before closing producer ownership. Ensure no retained sender keeps the coordinator waiting indefinitely.
- Call `TelemetryPipeline::shutdown` only after producers are dropped or account for a bounded timeout and incomplete processing.
- If shutdown fails, report a non-clean state and avoid asserting settled counter invariants.
- Maintain the existing proxy shutdown budget separately from the telemetry wait budget; do not describe the whole process as subject to a single 2-second deadline unless verified.

### 7.1 Shared 2-Second Telemetry Deadline

The coordinator drain and writer flush are supervised under **one shared deadline** (`Instant::now() + Duration::from_secs(2)`):
- If the coordinator takes 700 ms to drain remaining records, the writer receives 1,300 ms.
- If either component hangs (e.g. stalled stdout pipe), the **shutdown wait** times out after 2 seconds total; this cannot forcibly cancel a blocked synchronous OS write.
- Do not synchronously join blocked worker threads on Tokio workers. Document that timed-out threads may continue until the process exits or their I/O unblocks.

### 7.2 Settled Counter Invariant Verification

Upon clean shutdown, the snapshot is verified against mathematical invariants:
```text
evaluated == normal + anomalous
matches_http_server_error <= anomalous
matches_elevated_request_duration <= anomalous
matches_upstream_timeout <= anomalous
anomalous == events_queued + events_dropped_full + events_dropped_closed
observations_written <= batches_queued
events_written <= events_queued
```

### 7.3 Diagnostic Summaries on stderr

Two bounded diagnostic lines (each at most 1,024 bytes) are emitted via `core_engine::diagnostic!`:

1. **`detection_stats` line**:
```text
detection_stats evaluated=N normal=N anomalous=N http_5xx=N duration=N timeout=N batches_queued=N events_queued=N written_obs=N written_events=N drops_obs=N drops_events=N errors=N saturated=false
```

2. **`anomaly_output` line**:
```text
anomaly_output written=N unconfirmed=N state=clean|failed|timed_out
```

Set `state=clean` only after confirmed coordinator completion, writer completion, and successful flush. On failure or timeout, do not label snapshots as settled. Include the relevant failure cause in a bounded, escaped diagnostic.

---

## 8. Implementation Steps (File-by-File)

### Step 1: Update CLI Parser & Argument Validation (`cli/src/main.rs`)
- Add `--detect` (bool) and `--latency-threshold-ms` (Option<u64>) to `Command::Attach`.
- Implement validation helper `validate_attach_args(detect: bool, threshold: Option<u64>, name: &str, listen: &SocketAddr) -> Result<Option<u64>, String>`.
  - Validate `--latency-threshold-ms` requires `--detect`.
  - Validate range `1..=3_600_000`.
  - Validate `--name`: non-empty, byte length ≤ 128, no ASCII control characters or newlines.
  - Check non-loopback listen address and emit warning to diagnostics without mutating the address.
- In `main()` / `run()`:
  - If validation fails, log diagnostic error, print error to stderr, and exit with code `2`.
  - If `!detect`, execute existing `print_observations` legacy path.
  - If `detect`, construct `DetectionConfig`, spawn `TelemetryPipeline`, forward observer sender, await completion, execute supervised shutdown, emit diagnostic lines, and assert invariants.

### Step 2: Diagnostic Summary Formatting Helpers (`cli/src/reporting.rs`)
- Add public functions to format the settled `detection_stats` and `anomaly_output` diagnostic lines within 1,024-byte guarantees:
  - `pub fn format_detection_stats_diagnostic(snap: &DetectionStatsSnapshot) -> String`
  - `pub fn format_anomaly_output_diagnostic(snap: &DetectionStatsSnapshot, state: &str) -> String`

### Step 2a: Security and Failure-Mode Integration
- Make detection-mode startup transactional: if telemetry startup fails, do not leave a bound listener or partially active proxy; return an actionable error.
- Confirm that an output queue overflow never blocks HTTP forwarding. Account separately for input-queue losses and downstream output losses; anomaly counts cover only observations consumed by the detector.
- Ensure detection-mode output never includes raw path/query/header/body data in `ANOMALY_V1`.
- Preserve existing exit-code and error semantics outside argument parsing; do not force all runtime failures into CLI usage code `2`.

### Step 3: Comprehensive Unit & Integration Tests
- Add CLI unit tests in `cli/src/main.rs` for argument validation and flag combinations.
- Add end-to-end integration tests exercising real HTTP requests, detection classification, formatting, and shutdown summaries.

### Step 4: Documentation Alignment
- Update `documents/M4_ANOMALY_DETECTION_DESIGN.md` recording M4.3 completion.
- Update `documents/EXAPROP_USAGE.md` with M4.3 `--detect` and `--latency-threshold-ms` usage instructions and examples.

---

## 9. Verification & Acceptance Criteria

### 9.1 Required Cargo Toolchain Commands
All of the following must complete with exit code 0:
```bash
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --release --locked
cargo doc --workspace --no-deps --locked
cargo audit
git diff --check
```

### 9.2 Functional Test Suite Matrix
1. **CLI Flag Verification:**
   - Default attach (no detection): passes, M3 output format preserved.
   - Attach with `--detect`: default 1,000 ms threshold used.
   - Attach with `--detect --latency-threshold-ms 500`: custom 500 ms threshold used.
   - Attach with `--latency-threshold-ms 500` (no `--detect`): rejected with exit code 2.
   - Attach with `--detect --latency-threshold-ms 0`: rejected with exit code 2.
   - Attach with `--detect --latency-threshold-ms 3600001`: rejected with exit code 2.
2. **Deterministic Anomaly Scenarios:**
   - HTTP 200 Fast -> `OBSERVATION_V1`, no `ANOMALY_V1`, normal incremented.
   - HTTP 404 Fast -> `OBSERVATION_V1`, no `ANOMALY_V1`, normal incremented.
   - HTTP 500 Fast -> `OBSERVATION_V1` + `ANOMALY_V1` (`http_server_error`).
   - HTTP 200 Slow (> threshold) -> `OBSERVATION_V1` + `ANOMALY_V1` (`elevated_request_duration`, `threshold_ms`).
   - Upstream Timeout -> `OBSERVATION_V1` + `ANOMALY_V1` (`upstream_timeout`); verify actual proxy-generated status and origin.
   - Multi-match -> one `ANOMALY_V1` containing exactly the matched reasons. Use a synthetic observation with HTTP 504, `BackendTimeout`, and duration above threshold to test all three; separately test the real proxy timeout status.
3. **Resilience & Backpressure Scenarios:**
   - Full output batch queue: coordinator drops batch without blocking proxy handlers; drops accurately tracked.
   - Stalled / broken stdout: report writer failure or timeout without blocking the async caller; a synchronous blocked writer thread may survive the wait deadline.
   - Clean shutdown: settled counter invariants hold.
4. **Regression Smoke Tests:**
   - `python scripts/smoke_fastapi.py` passes cleanly against M3 default mode.
