# M4 — Deterministic HTTP anomaly detection

Status: **design proposal for independent engineering review; not implemented**.

Design date: 23 October 2026 (Asia/Calcutta). Source baseline: `origin/main` at `6c862edb86e88abd9ef1d6ede80d14a6821743d8`. Design branch: `m4-anomaly-detection`. Both the M3 merge and repository-standards merge are ancestors of this baseline. The starting tree was clean. This change adds only this document; it does not add detector code, flags, dependencies, or new runtime behavior.

## 1. Executive summary

Add an explicitly enabled, deterministic classifier over existing `Observation` values. Evaluate three independent rules: HTTP status 500–599, handler duration strictly above a configured threshold, and `RequestOutcome::BackendTimeout`. Produce at most one derived anomaly per evaluated observation, carrying every matching reason. Preserve status and outcome so a backend HTTP 504 is distinguishable from a proxy timeout response.

Recommend **consumer-side detection**, outside HTTP tasks, using the existing M3 observation channel. In detection mode, a dedicated coordinator consumes observations, evaluates rules, and attempts nonblocking submission of a bounded output batch. A separate OS thread writes stdout. One batch contains one normal observation line and zero or one anomaly line. Diagnostics retain their existing independent bounded stderr writer. No detector or output queue is added to `handle_request`.

The trade-off is explicit: observations lost before consumption cannot be classified. Detection is best effort over received observations, not a complete request census. A slow output sink can lose output batches but must not block the coordinator on queue capacity or any HTTP task. Existing M3 behavior remains the default when detection is disabled.

Core decisions:

| Concern | Proposed decision |
| --- | --- |
| Enablement | Opt in with `attach --detect`; default off |
| Duration threshold | Default 1,000 ms; integer CLI range 1–3,600,000 ms; match uses exact `Duration` comparison `>` |
| Event cardinality | One event per consumed observation with at least one reason; no cross-request deduplication |
| Severity | Duration-only: Warning; HTTP 5xx or upstream timeout: Error; no Critical severity |
| Identity | Consumer-local sequence, not a UUID, trace ID, or globally unique request ID |
| Integration | Existing 1,024-record M3 input queue, dedicated coordinator, shared 256-batch output queue, dedicated stdout writer |
| Output | Versioned observation/anomaly lines on stdout in detection mode; diagnostics/summaries on existing best-effort stderr |
| Privacy | Anomaly lines omit path and query entirely; use local sequence as the safe reference |
| Compatibility | Existing proxy APIs and `Observation`/`RequestOutcome` remain unchanged; detection-off CLI output/lifecycle retained |

## 2. Verified M3 architecture

The following are source observations, not assumptions about planned functionality:

| Source | Verified behavior and implication |
| --- | --- |
| [observation.rs](../core/src/observation.rs) | Owns timestamp, service `String`, `Method`, visible path, sanitized query, status, duration, and outcome. Fields are public, so external producers can construct synthetic combinations. `ObservationStats` tracks input-channel accepted/full/closed sends. |
| [proxy.rs](../core/src/proxy.rs) | Creates an observation only when an observer is configured. `Observer::emit` calls `try_send`; full/closed queues do not wait. No receiver means no automatic detection. |
| [proxy handler](../core/src/proxy.rs) | Starts `Instant` and wall-clock timestamp at parsed-handler entry. Samples elapsed time at emission after response buffering/reframing or proxy error construction, before sanitization and enqueueing. |
| [cli/main.rs](../cli/src/main.rs) | Owns the 1,024-record observation channel and a dedicated stdout thread. The current thread calls `blocking_recv`, formats, writes each record, and flushes after closure. A blocked stdout writer therefore stops that consumer, although HTTP sends remain nonblocking. |
| [diagnostics.rs](../core/src/diagnostics.rs) | Uses a process-wide queue of 256 messages capped at 1,024 bytes and a dedicated stderr thread. Counts drops and write failures; process-exit drain is bounded. It is not a reliable anomaly transport. |
| [lib.rs](../core/src/lib.rs) | Exports the existing modules and observation/stat types. Proxy entry points are `run`, `run_with_observer`, and `run_with_observer_and_stats`. |
| [workspace](../Cargo.toml), [core manifest](../core/Cargo.toml), [CLI manifest](../cli/Cargo.toml) | Rust workspace, both packages 0.1.0, standard library/Tokio/Hyper/Clap already available. No new dependency is needed for this design. |

M3's outcome values are `Success`, `BackendFailure`, `BackendTimeout`, `ClientFailure`, `ClientTimeout`, and `Rejected`. A completed backend response, including backend 5xx, has `Success`. Proxy-generated upstream errors use `BackendFailure` or `BackendTimeout`. `BackendFailure` includes connect/body failures **and** unsupported 101, invalid upstream Connection, trailers, oversized responses, or other constraint violations. It is not proof of a specific transport failure.

Duration includes client-body buffering and upstream work, including upstream body-frame gaps. It excludes admission/backlog delay, header parsing, observation work/output, and client response delivery. It is neither backend-only latency nor end-to-end client latency. Fifteen-second I/O deadlines apply to stages or each next body frame; they are not a universal request-duration deadline. See the [M3 contract](M3_OBSERVABILITY.md#exact-latency-semantics).

Shutdown allows two seconds for active connections, cancels remaining handlers, then gives observation output two seconds and diagnostics two seconds. Cancelled handlers and parser rejections may have no record. Counts do not cover these missing observations. The [M3 engineering report](M3_FINAL_ENGINEERING_REPORT.md) records historical tests, limits, and a transient Windows signal-helper timeout; it is not evidence that M4 exists or has passed tests.

## 3. Problem definition

Users currently receive factual observations and must interpret each status/duration themselves. M4 should classify a small set of conditions consistently, retain the evidence used, and expose uncertainty caused by loss. An anomaly means “a configured rule matched this observation,” not “the service is broken,” “the backend caused the delay,” or “a root cause was found.”

“Normal” below means no M4 rule matched. It does not mean a successful application request: a short HTTP 404 or client failure can be normal under these three rules.

## 4. Functional requirements

1. Evaluate the three rules independently against status, exact duration, and outcome.
2. Return zero or one typed anomaly; include all reasons in a fixed order.
3. Preserve factual evidence and identify backend-versus-proxy status provenance using M3 outcome semantics.
4. Keep `attach` and its current flags working; detection must be explicitly enabled.
5. Count classification separately from input loss, output acceptance, output loss, and complete line writes.
6. Do not make HTTP processing wait for detection, stdout, stderr, or downstream queue capacity.
7. Make the pure classifier testable with synthetic observations and no network, clock reads, sleeps, or OS output.
8. Keep default M3 behavior and tests, and add explicit tests for detection-enabled behavior.

## 5. Non-functional requirements

- Three fixed predicates, no history, windows, route maps, per-service caches, network calls, or unbounded collections.
- Immutable configuration during one run; no dynamic reload or shared configuration lock.
- Fixed worker count, fixed queue capacities, bounded new output payloads, bounded rendering work per consumed record.
- Linux/Windows behavior using existing portable Rust/Tokio/standard-library facilities and the current CI matrix.
- No changes to HTTP statuses, headers, framing, body limits, deadlines, routing, or request admission.
- No new persistent storage, global event-ID service, metric listener, or external infrastructure.
- Safe, bounded shutdown when writers block, fail, or panic. Loss is counted where observable, never described as lossless monitoring.

These are bounded-work engineering goals, not hard real-time scheduling or zero-allocation promises. M3 still creates variable-sized observation metadata; this design does not claim a fixed total process-memory ceiling.

## 6. Detection rules and edge cases

Let `T` be the validated threshold and `d` the exact `Observation::duration`.

| Rule / stable output name | Exact predicate | Severity contribution | Evidence |
| --- | --- | --- | --- |
| `HttpServerError` / `http_server_error` | `500 <= status.as_u16() <= 599` | Error | Original status, outcome, and derived status origin |
| `ElevatedRequestDuration` / `elevated_request_duration` | `d > T` | Warning | Original duration, configured threshold, strict comparison |
| `UpstreamTimeout` / `upstream_timeout` | `outcome == BackendTimeout` | Error | Original outcome; no inference about the particular timed-out stage |

The default is exactly `Duration::from_millis(1000)`. At 999 ms: no duration match. At 1,000 ms: no match. At 1,000 ms + 1 ns: match. Compare `Duration` directly, never rounded milliseconds or floating-point output. HTTP 499 and 600 are outside the 5xx rule; 500 and 599 are inside.

Status provenance follows the M3 producer contract:

| Status/outcome | Interpretation |
| --- | --- |
| 500/503/502/504 + `Success` | Backend returned that status. Match HTTP 5xx; do not infer a proxy timeout or transport error. |
| 502 + `BackendFailure` | Proxy-generated response after an upstream failure/constraint violation. Match HTTP 5xx; specific transport-versus-protocol cause is unavailable. |
| 504 + `BackendTimeout` | Proxy-generated response; HTTP 5xx and upstream-timeout rules match; duration is evaluated independently. |
| 504 + `BackendFailure` in a synthetic record | HTTP 5xx only unless slow; a 504 alone never creates a timeout match. |
| 408 + `ClientTimeout` | Not an upstream timeout or HTTP 5xx. Can match elevated duration. |
| 501 + `Rejected` | Proxy unsupported-request response; still matches HTTP 5xx, not “backend returned an error.” |
| 200 + `BackendTimeout` in a synthetic record | Timeout predicate matches literally, without rewriting status or inventing a 5xx. External producers own record correctness. |

For M3-produced observations, `Success` maps to `StatusOrigin::BackendResponse`; other outcomes map to `StatusOrigin::ProxyResponse`. This derives provenance from the outcome contract; it does not independently verify external producers. Preserve the original outcome in the event.

**Evidence limitation:** M3 cannot distinguish a true TCP/connect failure from an upstream framing/size/upgrade violation inside `BackendFailure`. M4 must retain `BackendFailure` as unspecified upstream failure evidence and must not label every such event `transport_failure`. Exact stage/cause classification would require a separately reviewed additive observation-evidence change; it is not needed or proposed for initial M4.

## 7. Event schema and invariants

### 7.1 One event with multiple reasons

| Representation | Benefit | Cost |
| --- | --- | --- |
| Event per matching rule | Small individual rule messages | One slow timeout can produce three apparent incidents; alert/event counts inflate and output triples |
| One event with all matching reasons | One anomalous observation counted once, complete evidence in one message | Requires a tiny fixed set of reasons |

Recommend the second. Reason order is always HTTP server error, elevated request duration, upstream timeout. There is no suppression precedence. Severity is the maximum contribution: duration-only Warning, otherwise Error. It is an attention level, not an impact estimate or paging policy. No Critical classification is justified by this evidence alone.

An identical observation evaluated twice yields the same classification twice. There is no semantic request deduplication, retry grouping, or aggregation across services. Each actual channel receive is one evaluation and at most one emitted event.

### 7.2 Proposed public core API

Add a small `core_engine::detection` module in `core/src/detection.rs`, exported through [lib.rs](../core/src/lib.rs). These are proposed signatures and invariants, **not implemented Rust code**:

| Type / operation | Proposed contract |
| --- | --- |
| `DetectionConfig::new(threshold_ms: u64) -> Result<Self, DetectionConfigError>` | Private immutable fields; range 1–3,600,000; stores validated `Duration`; Default is 1,000 ms. No enable flag: constructing/using a detector is the library opt-in. |
| `DetectionConfigError` | Typed invalid-threshold error; Display includes allowed range and supplied numeric value, no request data. |
| `Detector::new(config: DetectionConfig) -> Self` | Owns only the validated fixed-size configuration; no sender, writer, clock, mutable history, or process global. |
| `Detector::evaluate(&self, observation: &Observation) -> Option<Anomaly>` | Pure classification. Borrows the record, copies scalar evidence, does not clone its strings/Method, does no I/O, and changes no counters. |
| `AnomalyKind` | Three closed variants listed in section 6. |
| `MatchedRules` | Private fixed three-bit representation; `contains`, `len`, and canonical-order iteration; no heap collection. Only valid nonempty sets are exposed through an `Anomaly`. |
| `Severity` | Warning or Error; fixed ordering and output spelling. |
| `StatusOrigin` | BackendResponse or ProxyResponse, according to M3 outcome semantics. |
| `Anomaly` | Private fields with read-only accessors: original timestamp, status, outcome, duration; origin, nonempty matches, severity; threshold evidence present exactly when duration matches. No service/path/query/body/header or arbitrary message strings. |

Threshold evidence contains the exact `Duration` used. The anomaly owns its scalar evidence and borrows nothing after evaluation. Service/method for presentation come from the still-owned observation in the CLI coordinator, through bounded renderers; they are not cloned into the core anomaly.

The classifier is `Send + Sync` through immutable ordinary fields and can be shared without a mutex. Concurrent callers receive deterministic classifications independent of scheduling. `Anomaly` and configuration can be fixed-size value types; no premature trait, plugin, registry, generic detector framework, or serde dependency is needed.

Existing `Observation`, `RequestOutcome`, proxy functions, and channel signatures remain unchanged. Public additions are the module and types above only. Keep `DetectionStats` an internal CLI reporting handle initially: applications embedding the pure core detector can maintain their own counters and output. Do not expose CLI queue/writer lifecycle as a new core public API without an actual consumer requirement.

### 7.3 Identity

The CLI coordinator assigns a sequence starting at 1 to **each consumed observation**, including normal ones. It is a receive-order ordinal scoped to one `attach` process invocation. Normal and anomaly lines for the same observation share it. Concurrent HTTP completion ordering is not deterministic; classification of each record is. A restart resets the sequence.

No UUID, random run token, timestamp uniqueness, source-request ID, or global uniqueness is claimed. `seq` is a safe local reference, not a distributed correlation key. Use checked increment: on exhaustion, stop the coordinator as a telemetry error, close its receiver, report through bounded diagnostics, and return unsuccessful shutdown; never wrap or silently duplicate ordinals. HTTP forwarding continues while its now-closed observation channel drops sends.

## 8. Integration options and trade-offs

| Dimension | A: classify before M3 queue submission | B: classify after M3 queue receive |
| --- | --- | --- |
| Detection coverage | Can classify every emitted observation even if the M3 queue later drops it; still misses unobserved/cancelled handlers | Misses records lost before consumption; input loss must accompany counts |
| HTTP cost | New rule evaluation/counters per emission; event handoff and metadata work may enter request tasks | No added detector/output work in request tasks |
| Output pressure | Requires an independent nonblocking event path; full queues still lose output | Existing input plus bounded output handoff; both losses explicit |
| Failure isolation | A detector bug/panic is closer to handler execution and may affect responses unless carefully isolated | Worker failure closes observation receiver; HTTP still uses existing `try_send` |
| Shutdown | Adds producer handles to connection lifecycle and another path to drain | Input closes through existing producer teardown; coordinator/output drain afterward |
| Testability | Pure model is easy; request-path wiring and producer accounting add integration cases | Pure model plus synthetic receiver/renderer/writer tests; network path unchanged |
| API compatibility | Likely new observer/hook options or internal coupling in proxy.rs | Existing observer APIs remain sufficient; CLI owns composition |

Recommend **B with a decoupled coordinator**, not simply inserting evaluation into the current blocking stdout loop. The latter is simpler but a blocked stdout write would stop further evaluation; the proposed coordinator continues classification of received observations and counts output drops without waiting for stdout.

Option A is rejected for the first release because stronger pre-queue detection coverage does not justify adding detection/output coupling to HTTP handlers. Its coverage is still incomplete and would not make output durable.

Also reject two independent raw-observation/anomaly stdout writers: they can interleave and complicate ordering. Sending anomaly events through operational diagnostics is rejected because they would compete with logging, inherit its truncation/drop policy, and couple anomaly visibility to stderr. Direct OS writes from any HTTP task are prohibited.

## 9. Recommended architecture

```text
HTTP handler (unchanged M3)
  Observation::new -> try_send
                         |
             M3 observation input: 1,024 records
                         |
            coordinator OS thread (--detect only)
              receive -> seq -> pure evaluate -> counters
              bounded normal/anomaly rendering
                         |
                     try_send (never send/await capacity)
                         |
              shared output queue: 256 batches
                         |
                stdout writer OS thread
                observation line, then optional anomaly line

operational diagnostics -> existing bounded queue -> stderr OS thread
async main: lifecycle supervision and bounded completion waits only
```

Only the coordinator owns the M3 receiver and output sender. The writer owns the output receiver and stdout. The async main task owns completion receivers and cloneable stats snapshots, not extra output senders. Once all proxy producers close, the coordinator drains input to closure and drops its output sender; the writer then drains and flushes.

Each internal `OutputBatch` owns pre-rendered bounded bytes: at most 4,096 bytes for the normal observation line and optionally 1,024 bytes for one anomaly line, including their newlines. It retains no `Observation` or separate owned path/query strings. Target data exists only inside the bounded normal line, never the anomaly line. After rendering, the coordinator drops the input record. No event-specific heap queue or unbounded backlog is added.

The output queue holds 256 batches, hence at most 256 anomaly events and 1,310,720 bytes (1.25 MiB) of queued line payload. Allow one in-progress batch per worker plus fixed descriptors, stats, and channel/thread overhead. Rendering must use capped buffers from the outset, not allocate a full formatted string and truncate afterward. M3's existing input observations remain variable-sized; the additional output payload bound is not a bound on their memory or total RSS.

One try-send accepts or drops the entire batch. Anomalies do not obtain extra queue slots at the expense of normal observations. A full/closed queue loses both lines together before acceptance and increments the appropriate normal-output and event-drop counters. Writer failure after acceptance can still leave only one line written; account separately as unconfirmed output.

**Opt-in compatibility cost:** detection mode introduces this additional output-drop boundary and a bounded, versioned stdout format described below. These changes must be documented and tested, not hidden as M3-equivalent delivery. Without `--detect`, retain the existing one-channel M3 printer, output format, drops, and exit behavior. No new workers or queues are constructed in that path.

## 10. Configuration and CLI contract

The following flags are proposals; they do not exist in the current CLI:

```text
exaprop attach --name orders --detect
exaprop attach --name orders --detect --latency-threshold-ms 750
```

| Setting | Proposed behavior |
| --- | --- |
| `--detect` | Boolean opt-in, default false; enables all three initial rules |
| `--latency-threshold-ms` | Unsigned base-10 integer, default 1000 when detection enabled; inclusive range 1–3,600,000 |
| Threshold supplied without `--detect` | CLI validation error, exit 2, before binding listener or starting workers |
| Zero, negative, fractional, overflow, nonnumeric, or above-range threshold | Error, exit 2; no silent clamping, default fallback, or disabled-rule interpretation |
| Per-rule disablement | Not provided in initial M4; zero does not disable duration detection |
| `--name` | Existing default/service semantics retained; same bounded escaped label on derived output |
| Output selection | No `--anomaly-output` flag initially; stdout carries tagged observations and anomalies, stderr carries operational messages and summaries |

Prefer these two flags over an output destination option: arbitrary file sinks add lifecycle and security decisions without a demonstrated initial need. Shell redirection applies to the combined stdout stream. A closed/blocked stdout sink never triggers fallback to stderr and never disables detection silently. No config file, environment framework, database, or remote management is proposed.

CLI parsing uses existing Clap facilities and explicit threshold validation. Parse the optional threshold as `Option<u64>` and resolve its default only when detection is enabled, so an implicit default cannot accidentally require `--detect` on existing commands. Parse errors occur before HTTP processing; this is distinct from runtime output failures. Library configuration uses the same range and units. Rules do not read live configuration or environment variables per observation.

## 11. Event output, privacy, and security

### 11.1 Proposed text contract

With detection enabled, stdout uses distinct `OBSERVATION_V1` and `ANOMALY_V1` prefixes, fixed key ordering, and one newline-terminated line per record. Observation lines carry M3 factual fields plus local sequence and truncation metadata. Derived lines carry classification plus its evidence. With detection disabled, the current M3 text stays unchanged.

Example of one slow backend response, not an asserted root cause:

```text
OBSERVATION_V1 seq=17 timestamp=42.000000000 service="orders" method="GET" path="/items" query=none status=503 outcome=Success duration_ms=1500.001000 truncated_fields=none
ANOMALY_V1 seq=17 timestamp=42.000000000 service="orders" method="GET" status=503 outcome=Success origin=backend severity=error duration_ms=1500.001000 reasons=http_server_error,elevated_request_duration threshold_ms=1000 truncated_fields=none
```

Every rendered anomaly line contains service, source timestamp, method, local reference, status, outcome, origin, duration, reasons, and severity. Threshold evidence is present exactly when the duration rule matches; otherwise emit `threshold_ms=none`. Canonical reason ordering does not imply precedence. Render durations using integer arithmetic with six fractional milliseconds so a threshold + 1 ns remains visible; classification never depends on presentation rounding. Timestamp is signed Unix seconds with nine fractional digits, including well-defined pre-epoch rendering; an unusual synthetic timestamp must not panic the worker.

Use lowercase severity/origin/rule tokens as shown, and existing outcome variant names. No free-form “backend is slow,” “transport failed,” or “root cause” explanation is generated. The fixed predicates are the explanation. Later incompatible text changes require a new format version; M4.1 does not promise a JSON schema or parser crate.

### 11.2 Privacy and field bounds

- Anomaly records omit path and query completely, including supposedly sanitized query keys. Local sequence is their safe reference. Do not add URL hashes: low-entropy sensitive values remain guessable and hashes imply correlation properties not needed here.
- Service and method are untrusted display inputs. Quote string values and escape quotes, backslashes, controls, ANSI/OSC, line separators, and non-ASCII using an ASCII escape representation. Bound scanning as well as output; never scan an arbitrarily long suffix to calculate a truncation length.
- Budget encoded string contents at 128 bytes for service and 64 for method. Normal-record path and query contents have budgets of 1,536 and 1,024 bytes. These are encoded-byte budgets, not Unicode character counts. Reserve room for quotes, `...`, mandatory numeric/evidence fields, `truncated_fields`, and newline before filling fields.
- Never split an escape token; include the shortened field name in `truncated_fields` in stable field order. Do not truncate away anomaly reasons, status, outcome, sequence, severity, or threshold. Assert total line limits at maximum scalar sizes, including `Duration`/timestamp extremes.
- Normal records still follow M3's privacy policy: paths, original parameter-name spelling, and unrecognized query values can expose secrets. Length caps and escaping are not anonymization. Detection does not worsen this by copying the target into a second derived event. A sensitive `--name` label is also not made secret by escaping.
- Core `Anomaly` deliberately has no raw-observation field or unbounded explanatory strings. Do not derive an output formatter that dumps the whole borrowed `Observation` by Debug. Tests use synthetic sentinels to prove derived records omit paths, queries, headers, and bodies.

In detection mode normal lines can be truncated and use the new prefix/key-value format. This is an explicit opt-in presentation difference, not a change to HTTP or the core Observation. Keep old automation on default M3 mode until it opts into and understands V1.

## 12. Backpressure and failure handling

| Boundary/failure | Required behavior |
| --- | --- |
| M3 input full | Existing new-record drop/full count; observation never evaluated; no inferred anomaly-drop increment |
| M3 input closed | Existing closed count; HTTP unaffected; no fabricated classification |
| Output batch queue full | Coordinator drops new batch immediately, counts normal-output drop and, if anomalous, event drop/full; continues receiving/evaluating |
| Output batch queue closed | Count corresponding closed drops; continue classification until input closes, even if stdout has failed |
| Slow/blocked stdout | Only writer waits on OS I/O; batch queue fills and coordinator drops without waiting; source queue can still overflow if arrival rate exceeds coordinator capacity |
| Broken stdout/partial write | Writer returns error, drops receiver, notifies main. Count complete lines only; accepted-but-incomplete output remains unconfirmed. Do not retry a partial line into a new destination. |
| Blocked/broken stderr | Existing diagnostics may drop or fail. No dependency from coordinator or HTTP tasks on actual stderr writes. Anomalies never fall back there. |
| Worker startup failure | Fail startup before binding/serving HTTP; close any created channels, use bounded diagnostics, return failure |
| Coordinator panic/failure | Receiver closes, HTTP observation sends drop; output drains what was already accepted; report telemetry failure at shutdown without pretending classification completed |
| Writer panic/failure | Same closed-output behavior; coordinator may continue classification; completion receiver error is not success |

No per-drop diagnostic message is emitted: it could become a log storm and would obscure loss summaries. Use atomic counters and a small number of final diagnostic lines. Do not join a potentially blocked OS writer synchronously on a Tokio worker or use `spawn_blocking` in a way that holds runtime shutdown indefinitely.

Detection mode adds an output loss boundary for normal records too. Existing `observation_queue enqueued` continues to mean input acceptance, not delivery. Its accepted-minus-normal-written `unconfirmed` includes output-batch drops, failed writes, and incomplete processing. Additional output-drop counters explain those losses rather than changing what the existing counters mean.

## 13. Counter semantics

Keep an internal cloneable `DetectionStats` snapshot handle using fixed atomics. The coordinator owns classification and submission updates; the stdout writer owns completed-line/write-error updates. Public library users can count their own calls to the pure detector. No externally exposed metrics endpoint is proposed.

| Counter | Increment point / meaning |
| --- | --- |
| `evaluated` | One classification completed for one received observation with detection enabled |
| `normal` | Completed evaluation returned no match; not a health claim |
| `anomalous` | Completed evaluation returned one nonempty anomaly, irrespective of reason count or delivery |
| `matches_http_server_error` | HTTP 5xx reason present; once per evaluated observation |
| `matches_elevated_request_duration` | Strict duration reason present; once per evaluated observation |
| `matches_upstream_timeout` | BackendTimeout reason present; once per evaluated observation |
| `events_queued` | Output batch containing the anomaly was accepted; each anomalous observation has one attempt |
| `events_dropped_full` / `events_dropped_closed` | Failed output batch submission for a detected event, by cause |
| `events_dropped` | Derived saturating sum of full and closed event-send drops; excludes accepted-but-unconfirmed output and unknown input loss |
| `events_written` | Complete anomaly line write, including newline, succeeded |
| `events_unconfirmed` | Snapshot-derived `events_queued.saturating_sub(events_written)`; not another drop counter |
| `batches_queued` / `observations_output_dropped_full` / `observations_output_dropped_closed` | All normal-record batch submission decisions, including batches with anomalies |
| `observations_written` | Complete normal line writes; used in existing observation-output accounting |
| `write_errors`, `truncated_observations`, `truncated_events` | Sink errors and bounded formatting, counted separately from classification/loss |

After producers and the coordinator stop cleanly, without counter saturation:

```text
evaluated = normal + anomalous
each rule-match count <= anomalous
sum(rule-match counts) may exceed anomalous
anomalous = events_queued + events_dropped_full + events_dropped_closed
events_written <= events_queued
events_unconfirmed = events_queued - events_written
input accepted - evaluated = accepted input not evaluated (if processing stopped early)
```

Detection off: all M4 counters remain zero and no M4 summary is required. Input full/closed drops are **unknown classifications**, not normal observations and not detected-event drops. Parser rejection, cancellation before emission, and hard process termination cannot be counted as known anomalies. Even all-zero measured drops never establish complete request coverage.

Use saturating u64 updates with an explicit `counters_saturated` flag; do not wrap and report misleading identities. Only the designated owner updates each field. Read-only clones can snapshot with relaxed atomics, as M3 does; fields are not a transactional live view. Queue acceptance can race completed writes in a live snapshot, so temporary relationships need not hold; use floored differences and label incomplete snapshots. Exact identities apply only after clean phase completion, no saturation, and no lifecycle failure. A failed coordinator can leave classified-but-not-submitted work; do not force residuals into full/closed drop counters to make totals balance. Rule results are deterministic for each input/configuration; concurrent receive ordering and capacity-dependent delivery are not claimed to be deterministic in production.

“Detected” is a classification decision. “Queued” is memory-channel acceptance. “Written” is complete line-write success. Flush success is separate and still not durable persistence. No storage/delivery acknowledgement exists beyond process memory and the writer result.

## 14. Shutdown behavior

1. Reuse current Ctrl+C, Unix SIGTERM, and Windows Ctrl+Break handling. Stop admission; allow the existing two-second connection grace, then cancel remaining handlers and release all observation producers.
2. Drain the input receiver to closure. The coordinator classifies and attempts batch submissions without waiting for output capacity, then drops the sole output sender and acknowledges completion.
3. The stdout writer drains accepted batches in order, writes normal then optional anomaly lines, flushes once, and acknowledges completion or error.
4. Supervise steps 2–3 under **one shared absolute two-second telemetry deadline** starting after proxy shutdown, not two sequential two-second waits. If the coordinator finishes late, the writer receives only the remaining time. Neither blocking thread is synchronously joined after the deadline.
5. Snapshot stats; indicate whether classification/output completed or timed out. Enqueue compact `detection_stats` and `anomaly_output` summaries through existing diagnostics. Keep each summary within 1,024 bytes even at maximum counter values; use two fixed lines rather than silent tail truncation.
6. Reuse the final two-second diagnostics drain and explicit ExitCode. Clean workers/flushes return zero; writer error, worker failure, or telemetry/diagnostic deadline returns nonzero. Full queues alone are best-effort drops, not an automatic process failure. Exit zero does not promise complete anomaly coverage.

Total intended shutdown budget remains approximately two seconds connection grace + two seconds shared telemetry completion + two seconds diagnostics, plus scheduling/process overhead. A permanently blocked writer remains detached until process exit. `unconfirmed` at timeout is an incomplete snapshot, not a promise that no later write can finish before exit. Forced termination offers no drain/counter guarantee. Embedders of the pure detector have no worker lifecycle to shut down; they own their existing observation consumer.

## 15. Testing strategy

The table specifies **future implementation tests**, not tests executed by this design change. Assert typed `Anomaly` fields and exact settled counters, not just an `ANOMALY` substring. Use synthetic durations/SystemTime values for rule tests, barriers/channels for races, injected short lifecycle deadlines for unit tests, and no long unit-test sleeps.

Abbreviations: S = HTTP server error; D = elevated request duration; U = upstream timeout. Unless noted, threshold is 1,000 ms and duration 10 ms. For an isolated accepted/written anomalous record, expect `evaluated=1, anomalous=1, normal=0, events_queued=1, events_written=1`; only its matched per-rule counters increment. For a normal one, expect `evaluated=1, normal=1, anomalous=0` and all event counters zero.

| Case | Input / deterministic setup | Required assertions |
| --- | --- | --- |
| 1 | 200, Success | No anomaly; normal counter vector |
| 2 | 404, Success | No anomaly; no assertion of application health |
| 3 | 500, Success | S, Error, BackendResponse; exact status/outcome preserved |
| 4 | 503, Success | Same classification shape as case 3, status 503 retained |
| 5 | 502, BackendFailure | S, ProxyResponse; no U, no invented transport cause |
| 6 | 504, Success | S, BackendResponse; no U |
| 7 | 504, BackendTimeout, 15 seconds | One event with S,D,U in fixed order, Error; each rule counter one; anomalous/event count one, not three |
| 8 | 408, ClientTimeout, 15 seconds | D only, Warning, ProxyResponse; no U or S |
| 9 | 200 at T − 1 ns | No D |
| 10 | 200 at exactly T | No D |
| 11 | 200 at T + 1 ns | D, Warning; exact threshold/duration and visible sub-ms evidence |
| 12 | 503, Success, T + 1 ns | One S,D event, Error; threshold included; no duplicate alert |
| 13 | Detection disabled | Existing M3 output/queue path and CLI parsing still work; no new worker/queue, evaluation, anomaly, or M4 stats |
| 14 | Threshold 0, −1, fraction, overflow, text, >3,600,000; supplied without --detect | Typed config error or CLI exit 2 before listener starts; valid boundaries/default accepted |
| 15 | N concurrent producers, deterministic records | Every accepted/received record independently classified; aggregate typed reasons/counters correct; ordinals unique but receive order not prescribed |
| 16 | Hold M3 receiver; fill capacity and send another | Existing input full drop; no evaluation/event-drop/normal classification fabricated for lost record; HTTP still completes |
| 17 | Close M3 receiver | Existing input closed drop; HTTP completes; no derived event from that record |
| 18 | Gate stdout writer, run coordinator past output capacity | Gate only writer; coordinator continues; exact whole-batch full drops and per-event drops; no blocking send |
| 19 | stdout fails immediately or midway through a line | Controlled write error, closed-output sends counted; accepted incomplete lines unconfirmed; no fallback; HTTP and coordinator continue |
| 20 | Fill unread stderr and close it in another case | No request/coordinator dependency on stderr; diagnostics may drop; existing bounded diagnostic shutdown retained |
| 21 | Close producers with accepted work pending | Coordinator drains to closure, sender closes, writer flushes; settled counter identities and correct exit zero |
| 22 | Cancel handler before emission / force process stop | Do not invent observation or anomaly; no completeness promise; test force-stop cleanup separately from graceful drain |
| 23 | Control-bearing service/path/query, long valid extension method; hostile strings injected into renderer helpers | One safe line, complete escapes, bounded fields; no injected newline or second record; do not construct impossible control-bearing Hyper Method values |
| 24 | Sensitive path/query/body/header sentinels | Anomaly has no target fields/sentinels; normal line remains explicitly subject to M3 policy; no forwarding mutation |
| 25 | Existing Linux/Windows CI runners | Both unchanged M3 smoke suites plus new M4 cases pass; no Unix-only signal/write assumptions |
| 26 | Status 499/500/599/600, 501 Rejected | Inclusive 5xx boundary; 501 source is proxy; no inferred backend failure |
| 27 | Synthetic BackendTimeout with 200, BackendFailure with 504 | U from outcome only; status does not create/suppress U |
| 28 | Writer fails after normal line but before anomaly line | Normal written one; anomaly queued one/written zero/unconfirmed one; failure is not complete batch delivery |
| 29 | Queue full for a normal versus anomalous batch | Both increment normal-output loss; only anomalous batch increments event loss; queued counts exclude failed sends |
| 30 | Maximum field/scalar sizes, pre-epoch timestamp | Total 4,096/1,024-byte limits include newline; bounded scan, complete escapes, truncation names, mandatory evidence retained; no timestamp panic |
| 31 | Blocked writer through shared shutdown deadline | Nonzero result within injected budget; no sequential reset, synchronous join, or runtime-held blocking task |
| 32 | Coordinator/writer spawn failure or panic | Startup rollback or failed completion observed; no false drained state; input/output loss accounting remains honest |
| 33 | Repeated same Observation, seq near exhaustion, counters near maximum | Same reason set; repeated evaluations count independently; no global dedup; sequence fails instead of wrapping; counter saturation explicitly flagged |
| 34 | Inspect typed model with Debug and safe renderer | Core anomaly contains no raw record/target; rendering uses only the whitelisted fields and does not resample a clock |

Reuse [smoke_fastapi.py](../scripts/smoke_fastapi.py) startup, requests, exact observation checks, 15-second header/body timeout cases, privacy sentinels, broken stdout, and isolated Windows Ctrl+Break helper. Reuse [smoke_http_review.py](../scripts/smoke_http_review.py) malformed headers, 101, Via, 205, connection reuse, echo/restart, and blocked/closed stderr. Keep both existing default-mode suites unchanged as regression gates; add detection-enabled invocations or a narrowly scoped M4 harness rather than replacing their assertions.

Future integration evidence should include typed event fixtures/parsed fields, all counter summaries, exit codes, raw logs, and actual platform/toolchain versions. Preserve readiness-request accounting: harness health probes are real observations and can affect expected counts. Do not shorten the real timeout campaign merely because pure classifier tests use synthetic durations.

## 16. Implementation phases

Every phase requires separate implementation authorization after independent design review. No phase is executed by this document.

| Phase | Expected files | Responsibilities | Tests / acceptance | Dependencies |
| --- | --- | --- | --- | --- |
| M4.1 — Detection model | New `core/src/detection.rs`; `core/src/lib.rs` export | Validated immutable config, fixed rules/reason set, severity, provenance, scalar-only event, pure API | Typed cases 1–12, 14, 26–27, 33–34; threshold nanosecond boundary; Send/Sync assertions; no source strings cloned or output performed | Approved design; no CLI/proxy wiring |
| M4.2 — Pipeline integration | New `cli/src/reporting.rs`; minimal composition in `cli/src/main.rs` | Coordinator and shared bounded batch queue, counters, worker ownership, completion signals, no HTTP hook | Synthetic channels/gated writer exercise cases 15–18, 28–29, 31–33; all settled identities; default M3 path preserved | M4.1 |
| M4.3 — CLI reporting | `cli/src/main.rs`, `cli/src/reporting.rs`; README and M3 contract opt-in compatibility notes | Add two flags, V1 bounded format, privacy allowlist, complete-line accounting, shared deadline, summaries/exit behavior | Cases 13–14, 19–24, 28, 30–32; byte-bound golden/parser tests; no arbitrary output destinations | M4.1–M4.2 |
| M4.4 — Integration verification | Existing fixtures only if necessary; extend smoke scripts with opt-in runs or new `scripts/smoke_m4.py`; `.github/workflows/ci.yml` | Real backend 500/503/504, proxy failure/timeout, duration and multi-match, shutdown/backpressure, default-mode regression retention | Cases 5–8 and 15–25 over real TCP; typed/counter assertions; Linux/Windows runs; existing 101/Via/205 checks retained | M4.3 |
| M4.5 — Final hardening | Design/usage/README/testing docs and only files with demonstrated defects | Review privacy, bounded allocations/rendering, producer ownership, queue loss, deadline math, output compatibility, evidence and CI | All acceptance criteria, strict Rust checks, both complete M3 suites and M4 suite, independent code review | M4.4 |

Expected runtime additions are one pure core module and one small CLI reporting module, not a core proxy redesign. `proxy.rs`, `diagnostics.rs`, existing observation types, manifests, and dependencies should not need functional changes. Any demonstrated exception needs an explanation and review before broadening scope. Phase filenames are plans, not files created by this design change.

## 17. Acceptance criteria

- The three exact predicates, strict threshold edge, outcome-driven timeout rule, nonempty fixed reason set, origin limits, and severity maximum are verified through typed tests.
- One anomalous consumed observation creates one event attempt; multi-rule counts are not mislabeled as request/incident counts.
- Default mode preserves current M3 APIs, HTTP behavior, observation output and regression tests. Detection mode's new text format, caps, and extra output-loss boundary are explicit.
- Source inspection finds no detection, blocking output, blocking channel send, queue-capacity await, or new configuration lock in HTTP handlers.
- Every new queue has fixed capacity; every new rendered line is bounded before allocation/scanning; no target is stored in derived events; no new thread/task per request.
- Slow/broken stdout and blocked stderr cannot stall HTTP; coordinator output submission remains nonblocking; writer failure, partial writes, and shutdown deadlines are observable without false success.
- Loss/coverage language distinguishes unknown input loss, detected events, queued events, complete writes, flushes, and absent durable storage. Counters settle with the documented identities when lifecycle completion permits them.
- Shared telemetry deadline preserves the approximately six-second aggregate shutdown budget; detached writer behavior is exercised on Linux and Windows.
- Future code passes `cargo fmt --all -- --check`, `cargo check --workspace --locked`, `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`, `cargo test --workspace --locked`, release build, and strict rustdoc, plus full existing M3 and new M4 smoke campaigns. Proposed tests are not reported as executed by this design document.
- Design review explicitly accepts the opt-in V1 presentation/truncation and shared-batch loss trade-off before implementation starts.

## 18. Risks and open engineering questions

| Issue | Decision / review question |
| --- | --- |
| Blindness before consumption | Accepted first-release trade-off. Always show input loss and incomplete-lifecycle context; no “all requests monitored” claim. Would stronger pre-queue coverage later justify Option A? |
| Transport-versus-protocol provenance | Keep unspecified BackendFailure evidence. If maintainers require detailed causes, review additive M3 evidence separately; do not guess from status. |
| Opt-in output compatibility | Reviewers must approve V1 formatting, field caps, and the additional normal-output drop stage. Default output remains unchanged. This is the largest CLI compatibility decision. |
| Shared stdout ordering | One writer and whole-batch acceptance prevent interleaving; partial OS writes still permit incomplete pairs and are accounted honestly. |
| Coordinator throughput | Fixed work/queue bounds reduce risk but do not prove negligible overhead. Measure under representative load during implementation; retain the conservative capacities until evidence justifies changes. |
| Queue/threshold defaults | 256 output batches and 1,000 ms are explicit initial policies, not universally optimal settings. Threshold is CLI-configurable; capacities stay fixed initially. |
| Metadata size inherited from M3 | Output frames add bounded memory, but M3 input metadata remains variable-sized. Tightening input metadata is a separate compatibility review, not a hidden part of M4. |
| Parser/cancellation gaps | Existing unobserved work remains unobserved; event counts are lower bounds over delivered observations, not service-wide totals. |
| Operational summaries can be lost | stderr remains best effort. Tests/library snapshots can inspect counters; no durable monitoring assurance follows from a missing/printed summary. |
| Severity and false positives | Static rules flag expected long operations and intentional backend errors too. Severity is rule-derived; no automatic remediation or causal diagnosis. |
| Windows shutdown helper | Preserve isolated-console safety and monitor the previously recorded transient timeout. Do not weaken assertions or introduce a platform-specific signal workaround without reproduction. |
| Review gate | Resolve objections to architecture/API/format before M4.1. Do not treat unresolved reviewer preferences as implemented features. |

## 19. Explicit non-goals

M4 does not include machine learning, adaptive statistical baselines, anomaly prediction, root-cause analysis, cross-service tracing, distributed correlation, fault injection, circuit breakers, automatic retries, anomaly-based request blocking, dashboards, persistent storage, cloud infrastructure, or OpenTelemetry exporters. It does not add HTTPS, streaming transport, new HTTP behavior, arbitrary output sinks, or a remote management service.

Future proposals can revisit these topics with evidence and their own scope. This design ends at explainable, best-effort classification of existing HTTP observations. **Stop here for independent review; no detector runtime implementation is authorized by this document.**
