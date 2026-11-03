//! Pipeline integration, bounded batch formatting, and telemetry accounting.
//!
//! Implements M4.2 consumer-side telemetry:
//! `Observation` receiver -> Coordinator -> Bounded `OutputBatch` queue -> Dedicated Writer.

use std::{
    io::{self, Write},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use core_engine::{
    detection::{Anomaly, AnomalyKind, Detector, Severity, StatusOrigin},
    Method, Observation, RequestOutcome,
};
use tokio::sync::{mpsc, oneshot};

/// Maximum pre-rendered byte capacity for one normal observation line, including trailing newline.
pub const NORMAL_LINE_CAP: usize = 4096;

/// Maximum pre-rendered byte capacity for one anomaly event line, including trailing newline.
pub const ANOMALY_LINE_CAP: usize = 1024;

/// Capacity of the bounded queue between the coordinator and the stdout writer.
pub const OUTPUT_BATCH_CAPACITY: usize = 256;

/// Default telemetry shutdown timeout shared between coordinator and writer.
pub const TELEMETRY_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// Maximum encoded byte budget for string field contents before escaping.
pub const SERVICE_ENCODED_BUDGET: usize = 128;
pub const METHOD_ENCODED_BUDGET: usize = 64;
pub const PATH_ENCODED_BUDGET: usize = 1536;
pub const QUERY_ENCODED_BUDGET: usize = 1024;

/// Pre-rendered, bounded byte buffers for one observed HTTP exchange.
///
/// Contains at most one normal observation line and optionally one derived anomaly line.
/// Memory for both lines is bounded at allocation time by `NORMAL_LINE_CAP` and `ANOMALY_LINE_CAP`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputBatch {
    pub normal_line: Box<[u8]>,
    pub anomaly_line: Option<Box<[u8]>>,
}

impl OutputBatch {
    /// Construct a new output batch, asserting byte limits.
    pub fn new(normal_line: Box<[u8]>, anomaly_line: Option<Box<[u8]>>) -> Self {
        assert!(
            normal_line.len() <= NORMAL_LINE_CAP,
            "normal_line exceeds NORMAL_LINE_CAP: {} > {}",
            normal_line.len(),
            NORMAL_LINE_CAP
        );
        if let Some(ref anomaly) = anomaly_line {
            assert!(
                anomaly.len() <= ANOMALY_LINE_CAP,
                "anomaly_line exceeds ANOMALY_LINE_CAP: {} > {}",
                anomaly.len(),
                ANOMALY_LINE_CAP
            );
        }
        Self {
            normal_line,
            anomaly_line,
        }
    }
}

/// Thread-safe atomic counters tracking the complete telemetry lifecycle.
#[derive(Debug, Default)]
pub struct DetectionStats {
    pub evaluated: AtomicU64,
    pub normal: AtomicU64,
    pub anomalous: AtomicU64,
    pub matches_http_server_error: AtomicU64,
    pub matches_elevated_request_duration: AtomicU64,
    pub matches_upstream_timeout: AtomicU64,
    pub batches_queued: AtomicU64,
    pub observations_output_dropped_full: AtomicU64,
    pub observations_output_dropped_closed: AtomicU64,
    pub events_queued: AtomicU64,
    pub events_dropped_full: AtomicU64,
    pub events_dropped_closed: AtomicU64,
    pub observations_written: AtomicU64,
    pub events_written: AtomicU64,
    pub write_errors: AtomicU64,
    pub truncated_observations: AtomicU64,
    pub truncated_events: AtomicU64,
    pub counters_saturated: AtomicBool,
}

impl DetectionStats {
    /// Construct new zeroed stats wrapped in an `Arc`.
    pub fn new_shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Atomically increment a counter, setting the saturation flag if overflowing `u64::MAX`.
    ///
    /// Uses race-safe compare-and-swap loop to prevent arithmetic wrapping under concurrent increments.
    #[inline]
    pub fn inc(&self, counter: &AtomicU64) {
        let mut cur = counter.load(Ordering::Relaxed);
        loop {
            if cur == u64::MAX {
                self.counters_saturated.store(true, Ordering::Relaxed);
                break;
            }
            match counter.compare_exchange_weak(cur, cur + 1, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(actual) => cur = actual,
            }
        }
    }

    /// Capture a point-in-time snapshot of all counters.
    pub fn snapshot(&self) -> DetectionStatsSnapshot {
        DetectionStatsSnapshot {
            evaluated: self.evaluated.load(Ordering::Relaxed),
            normal: self.normal.load(Ordering::Relaxed),
            anomalous: self.anomalous.load(Ordering::Relaxed),
            matches_http_server_error: self.matches_http_server_error.load(Ordering::Relaxed),
            matches_elevated_request_duration: self
                .matches_elevated_request_duration
                .load(Ordering::Relaxed),
            matches_upstream_timeout: self.matches_upstream_timeout.load(Ordering::Relaxed),
            batches_queued: self.batches_queued.load(Ordering::Relaxed),
            observations_output_dropped_full: self
                .observations_output_dropped_full
                .load(Ordering::Relaxed),
            observations_output_dropped_closed: self
                .observations_output_dropped_closed
                .load(Ordering::Relaxed),
            events_queued: self.events_queued.load(Ordering::Relaxed),
            events_dropped_full: self.events_dropped_full.load(Ordering::Relaxed),
            events_dropped_closed: self.events_dropped_closed.load(Ordering::Relaxed),
            observations_written: self.observations_written.load(Ordering::Relaxed),
            events_written: self.events_written.load(Ordering::Relaxed),
            write_errors: self.write_errors.load(Ordering::Relaxed),
            truncated_observations: self.truncated_observations.load(Ordering::Relaxed),
            truncated_events: self.truncated_events.load(Ordering::Relaxed),
            counters_saturated: self.counters_saturated.load(Ordering::Relaxed),
        }
    }
}

/// Value snapshot of telemetry counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DetectionStatsSnapshot {
    pub evaluated: u64,
    pub normal: u64,
    pub anomalous: u64,
    pub matches_http_server_error: u64,
    pub matches_elevated_request_duration: u64,
    pub matches_upstream_timeout: u64,
    pub batches_queued: u64,
    pub observations_output_dropped_full: u64,
    pub observations_output_dropped_closed: u64,
    pub events_queued: u64,
    pub events_dropped_full: u64,
    pub events_dropped_closed: u64,
    pub observations_written: u64,
    pub events_written: u64,
    pub write_errors: u64,
    pub truncated_observations: u64,
    pub truncated_events: u64,
    pub counters_saturated: bool,
}

impl DetectionStatsSnapshot {
    /// Total normal observations dropped at the output queue boundary.
    pub const fn observations_output_dropped(&self) -> u64 {
        self.observations_output_dropped_full
            .saturating_add(self.observations_output_dropped_closed)
    }

    /// Total anomaly events dropped at the output queue boundary.
    pub const fn events_dropped(&self) -> u64 {
        self.events_dropped_full
            .saturating_add(self.events_dropped_closed)
    }

    /// Normal batches queued but not confirmed written.
    pub const fn observations_unconfirmed(&self) -> u64 {
        self.batches_queued
            .saturating_sub(self.observations_written)
    }

    /// Anomaly events queued but not confirmed written.
    pub const fn events_unconfirmed(&self) -> u64 {
        self.events_queued.saturating_sub(self.events_written)
    }

    /// Validate settled mathematical invariants after clean shutdown.
    pub fn check_settled_invariants(&self) -> Result<(), &'static str> {
        if self.counters_saturated {
            return Ok(()); // Saturated counters cannot guarantee linear identity arithmetic
        }

        if self.evaluated != self.normal.saturating_add(self.anomalous) {
            return Err("evaluated != normal + anomalous");
        }
        if self.matches_http_server_error > self.anomalous {
            return Err("matches_http_server_error > anomalous");
        }
        if self.matches_elevated_request_duration > self.anomalous {
            return Err("matches_elevated_request_duration > anomalous");
        }
        if self.matches_upstream_timeout > self.anomalous {
            return Err("matches_upstream_timeout > anomalous");
        }
        let total_matches = self
            .matches_http_server_error
            .saturating_add(self.matches_elevated_request_duration)
            .saturating_add(self.matches_upstream_timeout);
        if total_matches < self.anomalous {
            return Err("total rule matches < anomalous");
        }
        if self.anomalous
            != self
                .events_queued
                .saturating_add(self.events_dropped_full)
                .saturating_add(self.events_dropped_closed)
        {
            return Err("anomalous != events_queued + events_dropped_full + events_dropped_closed");
        }
        if self.observations_written > self.batches_queued {
            return Err("observations_written > batches_queued");
        }
        if self.events_written > self.events_queued {
            return Err("events_written > events_queued");
        }

        Ok(())
    }
}

/// Format timestamp as signed Unix seconds with 9 decimal places (nanoseconds).
pub fn format_timestamp(ts: SystemTime) -> String {
    match ts.duration_since(UNIX_EPOCH) {
        Ok(dur) => format!("{}.{:09}", dur.as_secs(), dur.subsec_nanos()),
        Err(err) => {
            let dur = err.duration();
            let secs = dur.as_secs();
            let nanos = dur.subsec_nanos();
            if nanos == 0 {
                format!("-{secs}.000000000")
            } else {
                format!("-{}.{:09}", secs + 1, 1_000_000_000 - nanos)
            }
        }
    }
}

/// Format duration in milliseconds with 6 decimal places (microsecond resolution).
pub fn format_duration_ms(dur: Duration) -> String {
    let total_ms = dur.as_millis();
    let micros_frac = (dur.subsec_nanos() % 1_000_000) / 1000;
    let nanos_frac = dur.subsec_nanos() % 1000;
    format!("{total_ms}.{micros_frac:03}{nanos_frac:03}")
}

/// Canonical string token for outcome variants.
pub fn outcome_token(outcome: RequestOutcome) -> &'static str {
    match outcome {
        RequestOutcome::Success => "Success",
        RequestOutcome::BackendFailure => "BackendFailure",
        RequestOutcome::BackendTimeout => "BackendTimeout",
        RequestOutcome::ClientTimeout => "ClientTimeout",
        RequestOutcome::ClientFailure => "ClientFailure",
        RequestOutcome::Rejected => "Rejected",
    }
}

/// Canonical string token for anomaly rules.
pub fn reason_token(kind: AnomalyKind) -> &'static str {
    match kind {
        AnomalyKind::HttpServerError => "http_server_error",
        AnomalyKind::ElevatedRequestDuration => "elevated_request_duration",
        AnomalyKind::UpstreamTimeout => "upstream_timeout",
    }
}

/// Canonical string token for status origins.
pub fn origin_token(origin: StatusOrigin) -> &'static str {
    match origin {
        StatusOrigin::BackendResponse => "backend",
        StatusOrigin::ProxyResponse => "proxy",
    }
}

/// Canonical string token for severity levels.
pub fn severity_token(sev: Severity) -> &'static str {
    match sev {
        Severity::Warning => "warning",
        Severity::Error => "error",
    }
}

/// Escape quotes, backslashes, controls, and non-ASCII characters; bound by byte length.
pub fn escape_and_bound(s: &str, byte_limit: usize) -> (String, bool) {
    let mut out = String::new();
    let mut truncated = false;

    for ch in s.chars() {
        let esc = match ch {
            '"' => "\\\"".to_string(),
            '\\' => "\\\\".to_string(),
            '\n' => "\\n".to_string(),
            '\r' => "\\r".to_string(),
            '\t' => "\\t".to_string(),
            c if c.is_ascii_control() || !c.is_ascii() => {
                let mut buf = [0u8; 4];
                let encoded = c.encode_utf8(&mut buf);
                let mut s = String::new();
                for b in encoded.as_bytes() {
                    use std::fmt::Write;
                    let _ = write!(s, "\\x{:02x}", b);
                }
                s
            }
            c => c.to_string(),
        };

        if out.len() + esc.len() > byte_limit {
            truncated = true;
            break;
        }
        out.push_str(&esc);
    }

    (out, truncated)
}

/// Render one normal observation line strictly bounded to `NORMAL_LINE_CAP`.
pub fn render_normal_line(seq: u64, obs: &Observation) -> (Box<[u8]>, bool) {
    let (service, trunc_service) = escape_and_bound(&obs.service_name, SERVICE_ENCODED_BUDGET);
    let (method, trunc_method) = escape_and_bound(obs.method.as_str(), METHOD_ENCODED_BUDGET);
    let (path, trunc_path) = escape_and_bound(&obs.path, PATH_ENCODED_BUDGET);
    let (query, trunc_query) = match &obs.query {
        Some(q) => {
            let (eq, tq) = escape_and_bound(q, QUERY_ENCODED_BUDGET);
            (format!("query=\"{eq}\""), tq)
        }
        None => ("query=none".to_string(), false),
    };

    let mut trunc_fields = Vec::new();
    if trunc_service {
        trunc_fields.push("service");
    }
    if trunc_method {
        trunc_fields.push("method");
    }
    if trunc_path && trunc_query {
        trunc_fields.push("path_query");
    } else if trunc_path {
        trunc_fields.push("path");
    } else if trunc_query {
        trunc_fields.push("query");
    }

    let trunc_str = if trunc_fields.is_empty() {
        "none".to_string()
    } else {
        trunc_fields.join(",")
    };

    let ts = format_timestamp(obs.timestamp);
    let duration_ms = format_duration_ms(obs.duration);
    let outcome = outcome_token(obs.outcome);

    let line = format!(
        "OBSERVATION_V1 seq={seq} timestamp={ts} service=\"{service}\" method=\"{method}\" path=\"{path}\" {query} status={} outcome={outcome} duration_ms={duration_ms} truncated_fields={trunc_str}\n",
        obs.status.as_u16()
    );

    let is_truncated = !trunc_fields.is_empty();
    debug_assert!(
        line.len() <= NORMAL_LINE_CAP,
        "normal line length {} exceeds NORMAL_LINE_CAP {}",
        line.len(),
        NORMAL_LINE_CAP
    );

    (line.into_bytes().into_boxed_slice(), is_truncated)
}

/// Render one anomaly line strictly bounded to `ANOMALY_LINE_CAP`.
///
/// Contains ONLY scalar evidence fields. URLs, paths, queries, headers, and bodies are excluded.
pub fn render_anomaly_line(
    seq: u64,
    service_name: &str,
    method: &Method,
    anomaly: &Anomaly,
) -> (Box<[u8]>, bool) {
    let (service, trunc_service) = escape_and_bound(service_name, SERVICE_ENCODED_BUDGET);
    let (method_str, trunc_method) = escape_and_bound(method.as_str(), METHOD_ENCODED_BUDGET);
    let mut trunc_fields = Vec::new();
    if trunc_service {
        trunc_fields.push("service");
    }
    if trunc_method {
        trunc_fields.push("method");
    }

    let reasons: Vec<&'static str> = anomaly.matched_rules().iter().map(reason_token).collect();
    let reasons_str = reasons.join(",");

    let threshold_str = match anomaly.duration_threshold() {
        Some(t) => format!("threshold_ms={}", t.as_millis()),
        None => "threshold_ms=none".to_string(),
    };

    let trunc_str = if trunc_fields.is_empty() {
        "none".to_string()
    } else {
        trunc_fields.join(",")
    };

    let ts = format_timestamp(anomaly.timestamp());
    let duration_ms = format_duration_ms(anomaly.duration());
    let outcome = outcome_token(anomaly.outcome());
    let origin = origin_token(anomaly.origin());
    let severity = severity_token(anomaly.severity());

    let line = format!(
        "ANOMALY_V1 seq={seq} timestamp={ts} service=\"{service}\" method=\"{method_str}\" status={} outcome={outcome} origin={origin} severity={severity} duration_ms={duration_ms} reasons={reasons_str} {threshold_str} truncated_fields={trunc_str}\n",
        anomaly.status().as_u16()
    );

    let is_truncated = !trunc_fields.is_empty();
    debug_assert!(
        line.len() <= ANOMALY_LINE_CAP,
        "anomaly line length {} exceeds ANOMALY_LINE_CAP {}",
        line.len(),
        ANOMALY_LINE_CAP
    );

    (line.into_bytes().into_boxed_slice(), is_truncated)
}

/// Terminal coordinator error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordinatorError {
    /// Sequence counter reached `u64::MAX`; telemetry terminated to prevent identifier reuse.
    SequenceOverflow,
}

impl std::fmt::Display for CoordinatorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SequenceOverflow => write!(f, "sequence overflow"),
        }
    }
}

impl std::error::Error for CoordinatorError {}

/// Dedicated coordinator worker consuming observations, classifying anomalies,
/// and dispatching pre-rendered batches to the output queue non-blockingly.
pub fn run_coordinator(
    observation_rx: mpsc::Receiver<Observation>,
    output_tx: mpsc::Sender<OutputBatch>,
    detector: Detector,
    stats: Arc<DetectionStats>,
    done_tx: Option<oneshot::Sender<Result<(), CoordinatorError>>>,
) {
    run_coordinator_with_initial_seq(observation_rx, output_tx, detector, stats, done_tx, 1);
}

/// Dedicated coordinator worker with injectable initial sequence for testing boundary behavior.
pub fn run_coordinator_with_initial_seq(
    mut observation_rx: mpsc::Receiver<Observation>,
    output_tx: mpsc::Sender<OutputBatch>,
    detector: Detector,
    stats: Arc<DetectionStats>,
    done_tx: Option<oneshot::Sender<Result<(), CoordinatorError>>>,
    initial_seq: u64,
) {
    let mut next_seq: Option<u64> = Some(initial_seq);
    let mut terminal_error: Option<CoordinatorError> = None;

    while let Some(obs) = observation_rx.blocking_recv() {
        let current_seq = match next_seq {
            Some(s) => s,
            None => {
                // Sequence counter reached capacity. Terminate telemetry processing
                // immediately to prevent identifier reuse.
                stats.counters_saturated.store(true, Ordering::Relaxed);
                terminal_error = Some(CoordinatorError::SequenceOverflow);
                break;
            }
        };

        // Advance sequence. If current_seq was u64::MAX, checked_add returns None.
        next_seq = current_seq.checked_add(1);
        if next_seq.is_none() {
            stats.counters_saturated.store(true, Ordering::Relaxed);
        }

        stats.inc(&stats.evaluated);

        let maybe_anomaly = detector.evaluate(&obs);
        let is_anomalous = if let Some(ref anomaly) = maybe_anomaly {
            stats.inc(&stats.anomalous);
            for rule in anomaly.matched_rules().iter() {
                match rule {
                    AnomalyKind::HttpServerError => stats.inc(&stats.matches_http_server_error),
                    AnomalyKind::ElevatedRequestDuration => {
                        stats.inc(&stats.matches_elevated_request_duration)
                    }
                    AnomalyKind::UpstreamTimeout => stats.inc(&stats.matches_upstream_timeout),
                }
            }
            true
        } else {
            stats.inc(&stats.normal);
            false
        };

        let (normal_line, normal_trunc) = render_normal_line(current_seq, &obs);
        if normal_trunc {
            stats.inc(&stats.truncated_observations);
        }

        let anomaly_line = maybe_anomaly.map(|anomaly| {
            let (line, trunc) =
                render_anomaly_line(current_seq, &obs.service_name, &obs.method, &anomaly);
            if trunc {
                stats.inc(&stats.truncated_events);
            }
            line
        });

        // Drop observation immediately to release variable-sized strings
        drop(obs);

        let batch = OutputBatch::new(normal_line, anomaly_line);

        match output_tx.try_send(batch) {
            Ok(()) => {
                stats.inc(&stats.batches_queued);
                if is_anomalous {
                    stats.inc(&stats.events_queued);
                }
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                stats.inc(&stats.observations_output_dropped_full);
                if is_anomalous {
                    stats.inc(&stats.events_dropped_full);
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                stats.inc(&stats.observations_output_dropped_closed);
                if is_anomalous {
                    stats.inc(&stats.events_dropped_closed);
                }
            }
        }
    }

    // Drop sole output sender to signal downstream writer
    drop(output_tx);

    if let Some(tx) = done_tx {
        let res = match terminal_error {
            Some(err) => Err(err),
            None => Ok(()),
        };
        let _ = tx.send(res);
    }
}

/// Dedicated stdout writer worker consuming pre-rendered batches from the output queue,
/// writing complete lines to the sink, and accounting for completed lines and I/O errors.
pub fn run_writer(
    mut output_rx: mpsc::Receiver<OutputBatch>,
    mut writer: impl Write,
    stats: Arc<DetectionStats>,
    done_tx: Option<oneshot::Sender<io::Result<()>>>,
) -> io::Result<()> {
    let mut write_res = Ok(());

    while let Some(batch) = output_rx.blocking_recv() {
        if let Err(e) = writer.write_all(&batch.normal_line) {
            stats.inc(&stats.write_errors);
            write_res = Err(e);
            break;
        }
        stats.inc(&stats.observations_written);

        if let Some(ref anomaly) = batch.anomaly_line {
            if let Err(e) = writer.write_all(anomaly) {
                stats.inc(&stats.write_errors);
                write_res = Err(e);
                break;
            }
            stats.inc(&stats.events_written);
        }
    }

    // Drop receiver to close channel if we broke out on error
    drop(output_rx);

    let final_res = match write_res {
        Ok(()) => writer.flush().inspect_err(|_| {
            stats.inc(&stats.write_errors);
        }),
        Err(e) => Err(e),
    };

    if let Some(tx) = done_tx {
        let res = match &final_res {
            Ok(()) => Ok(()),
            Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
        };
        let _ = tx.send(res);
    }

    final_res
}

/// Supervised telemetry pipeline managing coordinator and writer workers,
/// bounded channels, and a shared shutdown deadline.
pub struct TelemetryPipeline {
    observation_tx: mpsc::Sender<Observation>,
    stats: Arc<DetectionStats>,
    coordinator_done_rx: oneshot::Receiver<Result<(), CoordinatorError>>,
    writer_done_rx: oneshot::Receiver<io::Result<()>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StartupState {
    Pending,
    Started,
    Cancelled,
}

/// Dropping an uncommitted startup cancels both workers without joining them.
/// Cancelled workers only release their captured resources; they never read input or write output.
struct StartupGate(Arc<(Mutex<StartupState>, Condvar)>);

impl StartupGate {
    fn new() -> Self {
        Self(Arc::new((
            Mutex::new(StartupState::Pending),
            Condvar::new(),
        )))
    }

    fn set(&self, state: StartupState) {
        let (lock, ready) = &*self.0;
        *lock.lock().unwrap_or_else(|e| e.into_inner()) = state;
        ready.notify_all();
    }

    fn wait(gate: &Arc<(Mutex<StartupState>, Condvar)>) -> bool {
        let (lock, ready) = &**gate;
        let state = lock.lock().unwrap_or_else(|e| e.into_inner());
        let state = ready
            .wait_while(state, |s| *s == StartupState::Pending)
            .unwrap_or_else(|e| e.into_inner());
        *state == StartupState::Started
    }
}

impl Drop for StartupGate {
    fn drop(&mut self) {
        let (lock, ready) = &*self.0;
        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
        if *state == StartupState::Pending {
            *state = StartupState::Cancelled;
            ready.notify_all();
        }
    }
}

impl TelemetryPipeline {
    /// Spawn dedicated coordinator and writer threads with bounded queues.
    pub fn spawn<W>(
        observation_capacity: usize,
        detector: Detector,
        writer: W,
        stats: Arc<DetectionStats>,
    ) -> io::Result<(Self, mpsc::Sender<Observation>)>
    where
        W: Write + Send + 'static,
    {
        Self::spawn_with_initial_seq(observation_capacity, detector, writer, stats, 1)
    }

    /// Spawn dedicated coordinator and writer threads with injectable initial sequence for testing.
    pub fn spawn_with_initial_seq<W>(
        observation_capacity: usize,
        detector: Detector,
        writer: W,
        stats: Arc<DetectionStats>,
        initial_seq: u64,
    ) -> io::Result<(Self, mpsc::Sender<Observation>)>
    where
        W: Write + Send + 'static,
    {
        Self::spawn_with_workers(
            observation_capacity,
            detector,
            writer,
            stats,
            initial_seq,
            |name, worker| {
                std::thread::Builder::new()
                    .name(name.into())
                    .spawn(worker)
                    .map(drop)
            },
        )
    }

    fn spawn_with_workers<W>(
        observation_capacity: usize,
        detector: Detector,
        writer: W,
        stats: Arc<DetectionStats>,
        initial_seq: u64,
        mut spawn: impl FnMut(&str, Box<dyn FnOnce() + Send>) -> io::Result<()>,
    ) -> io::Result<(Self, mpsc::Sender<Observation>)>
    where
        W: Write + Send + 'static,
    {
        let (obs_tx, obs_rx) = mpsc::channel(observation_capacity);
        let (out_tx, out_rx) = mpsc::channel(OUTPUT_BATCH_CAPACITY);
        let (coord_done_tx, coord_done_rx) = oneshot::channel();
        let (writer_done_tx, writer_done_rx) = oneshot::channel();

        let startup = StartupGate::new();
        let coordinator_gate = Arc::clone(&startup.0);
        let coord_stats = Arc::clone(&stats);
        spawn(
            "exaprop-coordinator",
            Box::new(move || {
                if !StartupGate::wait(&coordinator_gate) {
                    return;
                }
                run_coordinator_with_initial_seq(
                    obs_rx,
                    out_tx,
                    detector,
                    coord_stats,
                    Some(coord_done_tx),
                    initial_seq,
                );
            }),
        )?;

        let writer_stats = Arc::clone(&stats);
        let writer_gate = Arc::clone(&startup.0);
        spawn(
            "exaprop-writer",
            Box::new(move || {
                if !StartupGate::wait(&writer_gate) {
                    return;
                }
                let _ = run_writer(out_rx, writer, writer_stats, Some(writer_done_tx));
            }),
        )?;

        // Only release workers once BOTH spawns succeeded. Any early return cancels them.
        startup.set(StartupState::Started);

        let pipeline = Self {
            observation_tx: obs_tx.clone(),
            stats,
            coordinator_done_rx: coord_done_rx,
            writer_done_rx,
        };

        Ok((pipeline, obs_tx))
    }

    /// Supervised shutdown under a single shared telemetry deadline.
    ///
    /// Drains the coordinator first, then the writer. If the timeout expires,
    /// returns `TimedOut` without synchronously blocking Tokio workers on detached threads.
    pub async fn shutdown(self, timeout: Duration) -> io::Result<DetectionStatsSnapshot> {
        let deadline = tokio::time::Instant::now() + timeout;

        // Drop the pipeline's own clone so input closes once all external producers drop
        drop(self.observation_tx);

        // 1. Await coordinator drain within the shared deadline
        let coord_res = match tokio::time::timeout_at(deadline, self.coordinator_done_rx).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(err))) => Err(io::Error::other(format!(
                "Telemetry coordinator failed: {err}"
            ))),
            Ok(Err(_)) => Err(io::Error::other(
                "Telemetry coordinator terminated prematurely without acknowledging completion",
            )),
            Err(_) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Telemetry coordinator did not finish within shutdown timeout",
            )),
        };

        // 2. Await writer drain and flush within the remaining shared deadline
        let writer_res = match tokio::time::timeout_at(deadline, self.writer_done_rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => Err(io::Error::other(
                "Telemetry writer thread panicked or closed without acknowledgment",
            )),
            Err(_) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Telemetry writer did not finish within shutdown timeout",
            )),
        };

        let snapshot = self.stats.snapshot();
        // Propagate coordinator error first if failed, otherwise writer error, otherwise snapshot
        coord_res?;
        writer_res?;
        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_engine::{
        detection::DetectionConfig, Method, Observation, RequestOutcome, StatusCode,
    };

    #[test]
    fn startup_gate_requires_commit_and_drop_cancels() {
        for commit in [false, true] {
            let gate = StartupGate::new();
            let worker_gate = Arc::clone(&gate.0);
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                entered_tx.send(()).unwrap();
                done_tx.send(StartupGate::wait(&worker_gate)).unwrap();
            });
            entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(done_rx.try_recv().is_err());
            if commit {
                gate.set(StartupState::Started);
            }
            drop(gate);
            assert_eq!(
                done_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
                commit
            );
            worker.join().unwrap();
        }
    }

    #[test]
    fn startup_spawn_failures_cancel_created_workers_without_io() {
        struct NoIo;
        impl Write for NoIo {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                panic!("uncommitted writer ran")
            }
            fn flush(&mut self) -> io::Result<()> {
                panic!("uncommitted writer flushed")
            }
        }
        for failing_worker in ["exaprop-coordinator", "exaprop-writer"] {
            let stats = DetectionStats::new_shared();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let mut handles = Vec::new();
            let result = TelemetryPipeline::spawn_with_workers(
                1,
                Detector::new(DetectionConfig::default()),
                NoIo,
                Arc::clone(&stats),
                1,
                |name, worker| {
                    if name == failing_worker {
                        return Err(io::Error::other("injected spawn failure"));
                    }
                    let done = done_tx.clone();
                    handles.push(std::thread::spawn(move || {
                        worker();
                        done.send(()).unwrap();
                    }));
                    Ok(())
                },
            );
            assert!(result.is_err());
            for handle in handles {
                done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                handle.join().unwrap();
            }
            assert_eq!(stats.snapshot(), DetectionStatsSnapshot::default());
        }
    }

    #[test]
    fn batch_enforces_byte_limits() {
        let normal = vec![b'a'; NORMAL_LINE_CAP].into_boxed_slice();
        let anomaly = Some(vec![b'b'; ANOMALY_LINE_CAP].into_boxed_slice());
        let batch = OutputBatch::new(normal, anomaly);
        assert_eq!(batch.normal_line.len(), NORMAL_LINE_CAP);
        assert_eq!(batch.anomaly_line.as_ref().unwrap().len(), ANOMALY_LINE_CAP);
    }

    #[test]
    #[should_panic(expected = "normal_line exceeds NORMAL_LINE_CAP")]
    fn batch_panics_on_normal_line_overflow() {
        let normal = vec![b'a'; NORMAL_LINE_CAP + 1].into_boxed_slice();
        let _ = OutputBatch::new(normal, None);
    }

    #[test]
    #[should_panic(expected = "anomaly_line exceeds ANOMALY_LINE_CAP")]
    fn batch_panics_on_anomaly_line_overflow() {
        let normal = vec![b'a'; 10].into_boxed_slice();
        let anomaly = Some(vec![b'b'; ANOMALY_LINE_CAP + 1].into_boxed_slice());
        let _ = OutputBatch::new(normal, anomaly);
    }

    #[test]
    fn stats_settled_invariants_hold_on_consistent_counts() {
        let stats = DetectionStatsSnapshot {
            evaluated: 10,
            normal: 6,
            anomalous: 4,
            matches_http_server_error: 2,
            matches_elevated_request_duration: 3,
            matches_upstream_timeout: 1,
            batches_queued: 10,
            observations_output_dropped_full: 0,
            observations_output_dropped_closed: 0,
            events_queued: 3,
            events_dropped_full: 1,
            events_dropped_closed: 0,
            observations_written: 10,
            events_written: 3,
            write_errors: 0,
            truncated_observations: 0,
            truncated_events: 0,
            counters_saturated: false,
        };

        assert!(stats.check_settled_invariants().is_ok());
        assert_eq!(stats.observations_unconfirmed(), 0);
        assert_eq!(stats.events_unconfirmed(), 0);
        assert_eq!(stats.events_dropped(), 1);
    }

    #[test]
    fn stats_detects_broken_invariants() {
        let mut stats = DetectionStatsSnapshot {
            evaluated: 10,
            normal: 5,
            anomalous: 4, // 5 + 4 != 10
            ..Default::default()
        };
        assert_eq!(
            stats.check_settled_invariants().unwrap_err(),
            "evaluated != normal + anomalous"
        );

        stats.normal = 6;
        stats.matches_http_server_error = 5; // > anomalous (4)
        assert_eq!(
            stats.check_settled_invariants().unwrap_err(),
            "matches_http_server_error > anomalous"
        );
    }

    #[test]
    fn coordinator_processes_observations_and_updates_counters() {
        let (obs_tx, obs_rx) = mpsc::channel(10);
        let (out_tx, mut out_rx) = mpsc::channel(10);
        let config = DetectionConfig::new(500).unwrap();
        let detector = Detector::new(config);
        let stats = DetectionStats::new_shared();

        let obs = Observation::new(
            UNIX_EPOCH + Duration::from_secs(10),
            "orders-service".into(),
            Method::GET,
            "/items",
            StatusCode::INTERNAL_SERVER_ERROR,
            Duration::from_millis(600),
            RequestOutcome::Success,
        );

        obs_tx.try_send(obs).unwrap();
        drop(obs_tx);

        run_coordinator(obs_rx, out_tx, detector, Arc::clone(&stats), None);

        let snap = stats.snapshot();
        assert_eq!(snap.evaluated, 1);
        assert_eq!(snap.normal, 0);
        assert_eq!(snap.anomalous, 1);
        assert_eq!(snap.matches_http_server_error, 1);
        assert_eq!(snap.matches_elevated_request_duration, 1);
        assert_eq!(snap.batches_queued, 1);
        assert_eq!(snap.events_queued, 1);

        let batch = out_rx.try_recv().unwrap();
        let normal_str = std::str::from_utf8(&batch.normal_line).unwrap();
        assert!(normal_str.starts_with("OBSERVATION_V1 seq=1 "));
        assert!(normal_str.contains("status=500"));

        let anomaly_str = std::str::from_utf8(batch.anomaly_line.as_ref().unwrap()).unwrap();
        assert!(anomaly_str.starts_with("ANOMALY_V1 seq=1 "));
        assert!(anomaly_str.contains("reasons=http_server_error,elevated_request_duration"));
        assert!(anomaly_str.contains("threshold_ms=500"));
        // URL path is strictly absent from anomaly line
        assert!(!anomaly_str.contains("/items"));
    }

    #[test]
    fn coordinator_drops_batches_on_full_output_queue_without_blocking() {
        let (obs_tx, obs_rx) = mpsc::channel(10);
        let (out_tx, _out_rx) = mpsc::channel(1); // Capacity 1
        let config = DetectionConfig::new(1000).unwrap();
        let detector = Detector::new(config);
        let stats = DetectionStats::new_shared();

        // Send 3 observations: first fills the queue, next 2 must be dropped
        for _ in 0..3 {
            let obs = Observation::new(
                UNIX_EPOCH,
                "svc".into(),
                Method::GET,
                "/item",
                StatusCode::INTERNAL_SERVER_ERROR,
                Duration::from_millis(10),
                RequestOutcome::Success,
            );
            obs_tx.try_send(obs).unwrap();
        }
        drop(obs_tx);

        run_coordinator(obs_rx, out_tx, detector, Arc::clone(&stats), None);

        let snap = stats.snapshot();
        assert_eq!(snap.evaluated, 3);
        assert_eq!(snap.anomalous, 3);
        assert_eq!(snap.batches_queued, 1);
        assert_eq!(snap.events_queued, 1);
        assert_eq!(snap.observations_output_dropped_full, 2);
        assert_eq!(snap.events_dropped_full, 2);
    }

    #[test]
    fn coordinator_handles_closed_output_queue_and_drains_input() {
        let (obs_tx, obs_rx) = mpsc::channel(10);
        let (out_tx, out_rx) = mpsc::channel(10);
        drop(out_rx); // Close output channel before coordinator runs

        let config = DetectionConfig::new(1000).unwrap();
        let detector = Detector::new(config);
        let stats = DetectionStats::new_shared();

        let obs = Observation::new(
            UNIX_EPOCH,
            "svc".into(),
            Method::GET,
            "/closed",
            StatusCode::OK,
            Duration::from_millis(10),
            RequestOutcome::Success,
        );
        obs_tx.try_send(obs).unwrap();
        drop(obs_tx);

        run_coordinator(obs_rx, out_tx, detector, Arc::clone(&stats), None);

        let snap = stats.snapshot();
        assert_eq!(snap.evaluated, 1);
        assert_eq!(snap.normal, 1);
        assert_eq!(snap.batches_queued, 0);
        assert_eq!(snap.observations_output_dropped_closed, 1);
    }

    #[test]
    fn writer_writes_all_batches_and_flushes() {
        let (out_tx, out_rx) = mpsc::channel(10);
        let stats = DetectionStats::new_shared();

        let batch1 = OutputBatch::new(b"line1\n".to_vec().into_boxed_slice(), None);
        let batch2 = OutputBatch::new(
            b"line2\n".to_vec().into_boxed_slice(),
            Some(b"anomaly2\n".to_vec().into_boxed_slice()),
        );

        out_tx.try_send(batch1).unwrap();
        out_tx.try_send(batch2).unwrap();
        drop(out_tx);

        let mut sink = Vec::new();
        let res = run_writer(out_rx, &mut sink, Arc::clone(&stats), None);
        assert!(res.is_ok());

        let sink_str = String::from_utf8(sink).unwrap();
        assert_eq!(sink_str, "line1\nline2\nanomaly2\n");

        let snap = stats.snapshot();
        assert_eq!(snap.observations_written, 2);
        assert_eq!(snap.events_written, 1);
        assert_eq!(snap.write_errors, 0);
    }

    #[test]
    fn writer_stops_on_broken_pipe_and_records_write_error() {
        struct BrokenPipeWriter;
        impl Write for BrokenPipeWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::BrokenPipe))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let (out_tx, out_rx) = mpsc::channel(10);
        let stats = DetectionStats::new_shared();

        let batch = OutputBatch::new(b"normal\n".to_vec().into_boxed_slice(), None);
        out_tx.try_send(batch).unwrap();
        drop(out_tx);

        let (done_tx, mut done_rx) = oneshot::channel();
        let res = run_writer(out_rx, BrokenPipeWriter, Arc::clone(&stats), Some(done_tx));
        assert_eq!(res.unwrap_err().kind(), io::ErrorKind::BrokenPipe);

        let done_res = done_rx.try_recv().unwrap();
        assert_eq!(done_res.unwrap_err().kind(), io::ErrorKind::BrokenPipe);

        let snap = stats.snapshot();
        assert_eq!(snap.observations_written, 0);
        assert_eq!(snap.events_written, 0);
        assert_eq!(snap.write_errors, 1);
    }

    #[tokio::test]
    async fn pipeline_clean_shutdown_drains_all_records() {
        let config = DetectionConfig::new(500).unwrap();
        let detector = Detector::new(config);
        let stats = DetectionStats::new_shared();

        struct SharedBuffer(Arc<std::sync::Mutex<Vec<u8>>>);
        impl Write for SharedBuffer {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let output_buf = Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer = SharedBuffer(Arc::clone(&output_buf));

        let (pipeline, tx) =
            TelemetryPipeline::spawn(10, detector, writer, Arc::clone(&stats)).unwrap();

        let obs = Observation::new(
            UNIX_EPOCH,
            "orders".into(),
            Method::POST,
            "/checkout",
            StatusCode::OK,
            Duration::from_millis(50),
            RequestOutcome::Success,
        );
        tx.try_send(obs).unwrap();
        drop(tx);

        let snap = pipeline.shutdown(Duration::from_secs(2)).await.unwrap();
        assert_eq!(snap.evaluated, 1);
        assert_eq!(snap.normal, 1);
        assert_eq!(snap.batches_queued, 1);
        assert_eq!(snap.observations_written, 1);
        assert!(snap.check_settled_invariants().is_ok());

        let output = String::from_utf8(output_buf.lock().unwrap().clone()).unwrap();
        assert!(output.starts_with("OBSERVATION_V1 seq=1 "));
        assert!(output.contains("/checkout"));
    }

    #[tokio::test]
    async fn pipeline_shutdown_aborts_on_blocked_writer_at_deadline() {
        let config = DetectionConfig::new(500).unwrap();
        let detector = Detector::new(config);
        let stats = DetectionStats::new_shared();

        struct BlockedWriter;
        impl Write for BlockedWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                // Emulate permanently blocked write (e.g. stalled pipe)
                std::thread::park();
                Ok(0)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let (pipeline, tx) =
            TelemetryPipeline::spawn(10, detector, BlockedWriter, Arc::clone(&stats)).unwrap();

        let obs = Observation::new(
            UNIX_EPOCH,
            "orders".into(),
            Method::GET,
            "/stalled",
            StatusCode::OK,
            Duration::from_millis(10),
            RequestOutcome::Success,
        );
        tx.try_send(obs).unwrap();
        drop(tx);

        let start = std::time::Instant::now();
        let res = pipeline.shutdown(Duration::from_millis(50)).await;
        let elapsed = start.elapsed();

        assert!(res.is_err());
        assert_eq!(res.unwrap_err().kind(), io::ErrorKind::TimedOut);
        // Shared deadline enforced: aborted around 50ms, did not hang indefinitely
        assert!(elapsed < Duration::from_millis(500));
    }

    #[tokio::test]
    async fn clean_pipeline_drain_settles_all_counter_invariants() {
        let config = DetectionConfig::new(500).unwrap();
        let detector = Detector::new(config);
        let stats = DetectionStats::new_shared();

        struct DevNull;
        impl Write for DevNull {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let (pipeline, tx) =
            TelemetryPipeline::spawn(20, detector, DevNull, Arc::clone(&stats)).unwrap();

        // 1. Normal fast 200
        tx.try_send(Observation::new(
            UNIX_EPOCH,
            "orders".into(),
            Method::GET,
            "/health",
            StatusCode::OK,
            Duration::from_millis(20),
            RequestOutcome::Success,
        ))
        .unwrap();

        // 2. Normal 404 (not an anomaly)
        tx.try_send(Observation::new(
            UNIX_EPOCH,
            "orders".into(),
            Method::GET,
            "/not_found",
            StatusCode::NOT_FOUND,
            Duration::from_millis(15),
            RequestOutcome::Success,
        ))
        .unwrap();

        // 3. Anomaly: 500 Internal Server Error
        tx.try_send(Observation::new(
            UNIX_EPOCH,
            "orders".into(),
            Method::POST,
            "/charge",
            StatusCode::INTERNAL_SERVER_ERROR,
            Duration::from_millis(50),
            RequestOutcome::Success,
        ))
        .unwrap();

        // 4. Anomaly: Elevated duration (800ms > 500ms)
        tx.try_send(Observation::new(
            UNIX_EPOCH,
            "orders".into(),
            Method::GET,
            "/slow",
            StatusCode::OK,
            Duration::from_millis(800),
            RequestOutcome::Success,
        ))
        .unwrap();

        // 5. Anomaly: Upstream Timeout
        tx.try_send(Observation::new(
            UNIX_EPOCH,
            "orders".into(),
            Method::GET,
            "/timeout",
            StatusCode::GATEWAY_TIMEOUT,
            Duration::from_millis(5000),
            RequestOutcome::BackendTimeout,
        ))
        .unwrap();

        drop(tx);

        let snap = pipeline.shutdown(Duration::from_secs(2)).await.unwrap();

        assert_eq!(snap.evaluated, 5);
        assert_eq!(snap.normal, 2);
        assert_eq!(snap.anomalous, 3);
        assert_eq!(snap.matches_http_server_error, 2); // 500 and 504 are both in 500..=599
        assert_eq!(snap.matches_elevated_request_duration, 2); // 800ms and 5000ms
        assert_eq!(snap.matches_upstream_timeout, 1);
        assert_eq!(snap.batches_queued, 5);
        assert_eq!(snap.events_queued, 3);
        assert_eq!(snap.observations_written, 5);
        assert_eq!(snap.events_written, 3);
        assert_eq!(snap.observations_unconfirmed(), 0);
        assert_eq!(snap.events_unconfirmed(), 0);
        assert!(snap.check_settled_invariants().is_ok());
    }

    #[test]
    fn byte_limits_and_truncation_accounting() {
        let long_path = "/path/".repeat(1000); // 6,000 bytes
        let obs = Observation::new(
            UNIX_EPOCH,
            "svc".into(),
            Method::GET,
            &long_path,
            StatusCode::OK,
            Duration::from_millis(10),
            RequestOutcome::Success,
        );

        let (line, truncated) = render_normal_line(1, &obs);
        assert!(truncated);
        assert!(line.len() <= NORMAL_LINE_CAP);
        assert!(line.ends_with(b"\n"));
        let line_str = std::str::from_utf8(&line).unwrap();
        assert!(line_str.contains("truncated_fields=path"));
    }

    #[test]
    fn anomaly_line_contains_zero_url_or_header_payloads() {
        let (obs_tx, obs_rx) = mpsc::channel(10);
        let (out_tx, mut out_rx) = mpsc::channel(10);
        let config = DetectionConfig::new(500).unwrap();
        let detector = Detector::new(config);
        let stats = DetectionStats::new_shared();

        let mut obs = Observation::new(
            UNIX_EPOCH,
            "sensitive-service".into(),
            Method::POST,
            "/internal/admin/secret_account",
            StatusCode::INTERNAL_SERVER_ERROR,
            Duration::from_millis(600),
            RequestOutcome::Success,
        );
        obs.query = Some("session_token=super_secret_token_12345".into());

        obs_tx.try_send(obs).unwrap();
        drop(obs_tx);

        run_coordinator(obs_rx, out_tx, detector, stats, None);

        let batch = out_rx.try_recv().unwrap();
        let normal_str = std::str::from_utf8(&batch.normal_line).unwrap();
        assert!(normal_str.contains("/internal/admin/secret_account"));

        let anomaly_str = std::str::from_utf8(batch.anomaly_line.as_ref().unwrap()).unwrap();
        // Crucial security invariant: URLs, query parameters, and paths are completely excluded
        assert!(!anomaly_str.contains("/internal"));
        assert!(!anomaly_str.contains("admin"));
        assert!(!anomaly_str.contains("secret_account"));
        assert!(!anomaly_str.contains("session_token"));
        assert!(!anomaly_str.contains("super_secret_token_12345"));

        // Scalar facts are preserved accurately
        assert!(anomaly_str.contains("service=\"sensitive-service\""));
        assert!(anomaly_str.contains("method=\"POST\""));
        assert!(anomaly_str.contains("status=500"));
        assert!(anomaly_str.contains("reasons=http_server_error,elevated_request_duration"));
    }

    #[tokio::test]
    async fn concurrent_counter_snapshots_are_monotonic() {
        let config = DetectionConfig::new(500).unwrap();
        let detector = Detector::new(config);
        let stats = DetectionStats::new_shared();

        struct DevNull;
        impl Write for DevNull {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let (pipeline, tx) =
            TelemetryPipeline::spawn(100, detector, DevNull, Arc::clone(&stats)).unwrap();

        let snapshot_stats = Arc::clone(&stats);
        let monitor = tokio::spawn(async move {
            let mut last_written = 0;
            for _ in 0..50 {
                let snap = snapshot_stats.snapshot();
                assert!(snap.observations_written >= last_written);
                last_written = snap.observations_written;
                assert!(snap.observations_unconfirmed() <= snap.batches_queued);
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        });

        for i in 0..50 {
            let obs = Observation::new(
                UNIX_EPOCH,
                "bench".into(),
                Method::GET,
                "/ping",
                if i % 2 == 0 {
                    StatusCode::OK
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                },
                Duration::from_millis(10),
                RequestOutcome::Success,
            );
            tx.try_send(obs).unwrap();
        }
        drop(tx);

        let (snap, _) = tokio::join!(pipeline.shutdown(Duration::from_secs(2)), monitor);
        let final_snap = snap.unwrap();
        assert_eq!(final_snap.evaluated, 50);
        assert_eq!(final_snap.observations_written, 50);
        assert!(final_snap.check_settled_invariants().is_ok());
    }

    #[test]
    fn stats_saturating_atomic_inc_at_max_and_max_minus_one_with_concurrency() {
        let stats = DetectionStats::new_shared();
        let counter = Arc::new(AtomicU64::new(u64::MAX - 1));

        let mut handles = Vec::new();
        for _ in 0..10 {
            let s = Arc::clone(&stats);
            let c = Arc::clone(&counter);
            handles.push(std::thread::spawn(move || {
                for _ in 0..100 {
                    s.inc(&c);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        // Must saturate at u64::MAX, NEVER wrap around to 0 or low values
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
        assert!(stats.counters_saturated.load(Ordering::Relaxed));

        // Incrementing again when already at u64::MAX
        stats.inc(&counter);
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn coordinator_terminates_on_sequence_overflow_without_identifier_reuse() {
        let (obs_tx, obs_rx) = mpsc::channel(10);
        let (out_tx, mut out_rx) = mpsc::channel(10);
        let config = DetectionConfig::new(500).unwrap();
        let detector = Detector::new(config);
        let stats = DetectionStats::new_shared();
        let (done_tx, mut done_rx) = oneshot::channel();

        // Start coordinator with initial_seq = u64::MAX
        let coordinator_thread = std::thread::spawn(move || {
            run_coordinator_with_initial_seq(
                obs_rx,
                out_tx,
                detector,
                stats,
                Some(done_tx),
                u64::MAX,
            );
        });

        // 1st request gets seq = u64::MAX
        obs_tx
            .try_send(Observation::new(
                UNIX_EPOCH,
                "svc".into(),
                Method::GET,
                "/first",
                StatusCode::OK,
                Duration::from_millis(10),
                RequestOutcome::Success,
            ))
            .unwrap();

        // 2nd request triggers sequence overflow
        obs_tx
            .try_send(Observation::new(
                UNIX_EPOCH,
                "svc".into(),
                Method::GET,
                "/second",
                StatusCode::OK,
                Duration::from_millis(10),
                RequestOutcome::Success,
            ))
            .unwrap();

        coordinator_thread.join().unwrap();

        // The coordinator must have terminated with explicit SequenceOverflow error
        let err = done_rx.try_recv().unwrap().unwrap_err();
        assert_eq!(err, CoordinatorError::SequenceOverflow);

        // Exactly one batch emitted (seq = u64::MAX). No identifier reuse!
        let batch1 = out_rx.try_recv().unwrap();
        let line1 = std::str::from_utf8(&batch1.normal_line).unwrap();
        assert!(line1.starts_with(&format!("OBSERVATION_V1 seq={} ", u64::MAX)));

        // Output channel was dropped, no second batch
        assert!(out_rx.try_recv().is_err());
    }

    #[test]
    fn extreme_durations_timestamps_and_malicious_terminals_preserve_caps_and_utf8() {
        // Pathological display inputs: ANSI controls, quotes, backslashes, multi-byte emojis
        let malicious_service = "\x1b[31;1m\"orders\"\n\r\t\\service\x00\x1b[0m".repeat(100);
        let malicious_path = "/api/v1/🦀/users/\x1b[2J/secret\"\\'\n\0".repeat(500);
        let malicious_query =
            "token=\"super\\secret\"&user=\x1b[32madmin\x1b[0m&data=🦀".repeat(200);

        // Extreme timestamps and durations
        let pre_epoch_timestamp = UNIX_EPOCH - Duration::from_secs(1_500_000_000);
        let extreme_duration = Duration::MAX;

        let mut obs = Observation::new(
            pre_epoch_timestamp,
            malicious_service,
            Method::POST,
            &malicious_path,
            StatusCode::INTERNAL_SERVER_ERROR,
            extreme_duration,
            RequestOutcome::Success,
        );
        obs.query = Some(malicious_query);

        let config = DetectionConfig::new(500).unwrap();
        let detector = Detector::new(config);
        let anomaly = detector.evaluate(&obs).unwrap();

        let (normal_line, normal_trunc) = render_normal_line(42, &obs);
        assert!(normal_trunc);
        assert!(normal_line.len() <= NORMAL_LINE_CAP);
        assert!(normal_line.ends_with(b"\n"));

        // Must be 100% valid UTF-8
        let normal_str = std::str::from_utf8(&normal_line).unwrap();
        assert!(normal_str.starts_with("OBSERVATION_V1 seq=42 "));
        // ANSI escape \x1b must be safely escaped, not raw
        assert!(!normal_str.contains('\x1b'));
        assert!(normal_str.contains("\\x1b"));
        assert!(normal_str.contains("truncated_fields="));

        let (anomaly_line, anomaly_trunc) =
            render_anomaly_line(42, &obs.service_name, &obs.method, &anomaly);
        assert!(anomaly_trunc); // Service name exceeded budget
        assert!(anomaly_line.len() <= ANOMALY_LINE_CAP);
        assert!(anomaly_line.ends_with(b"\n"));

        let anomaly_str = std::str::from_utf8(&anomaly_line).unwrap();
        assert!(anomaly_str.starts_with("ANOMALY_V1 seq=42 "));
        assert!(!anomaly_str.contains('\x1b'));
        assert!(anomaly_str.contains("\\x1b"));
        // Required evidence is preserved intact
        assert!(anomaly_str.contains("reasons=http_server_error,elevated_request_duration"));
        assert!(anomaly_str.contains("threshold_ms=500"));
        assert!(anomaly_str.contains("status=500"));
        // Path and query strictly excluded
        assert!(!anomaly_str.contains("super\\secret"));
        assert!(!anomaly_str.contains("/api/v1"));
    }

    #[test]
    fn writer_handles_write_zero_and_flush_failure() {
        struct WriteZeroWriter;
        impl Write for WriteZeroWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Ok(0) // write-zero simulation
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let (out_tx, out_rx) = mpsc::channel(10);
        let stats = DetectionStats::new_shared();

        let batch = OutputBatch::new(b"line\n".to_vec().into_boxed_slice(), None);
        out_tx.try_send(batch).unwrap();
        drop(out_tx);

        let res = run_writer(out_rx, WriteZeroWriter, Arc::clone(&stats), None);
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().kind(), io::ErrorKind::WriteZero);

        let snap = stats.snapshot();
        assert_eq!(snap.observations_written, 0);
        assert_eq!(snap.write_errors, 1);

        // Test flush failure
        struct FlushErrorWriter;
        impl Write for FlushErrorWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Err(io::Error::from(io::ErrorKind::ConnectionReset))
            }
        }

        let (out_tx, out_rx) = mpsc::channel(10);
        let stats = DetectionStats::new_shared();
        out_tx
            .try_send(OutputBatch::new(
                b"flush_line\n".to_vec().into_boxed_slice(),
                None,
            ))
            .unwrap();
        drop(out_tx);

        let res = run_writer(out_rx, FlushErrorWriter, Arc::clone(&stats), None);
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().kind(), io::ErrorKind::ConnectionReset);

        let snap = stats.snapshot();
        assert_eq!(snap.observations_written, 1); // Line was completely written
        assert_eq!(snap.write_errors, 1); // Flush error counted
    }

    #[test]
    fn writer_handles_partial_anomaly_line_write() {
        struct FailOnAnomalyWriter {
            normal_written: bool,
        }
        impl Write for FailOnAnomalyWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                if !self.normal_written {
                    self.normal_written = true;
                    Ok(buf.len())
                } else {
                    Err(io::Error::from(io::ErrorKind::BrokenPipe))
                }
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let (out_tx, out_rx) = mpsc::channel(10);
        let stats = DetectionStats::new_shared();

        let batch = OutputBatch::new(
            b"normal_line\n".to_vec().into_boxed_slice(),
            Some(b"anomaly_line\n".to_vec().into_boxed_slice()),
        );
        out_tx.try_send(batch).unwrap();
        drop(out_tx);

        let writer = FailOnAnomalyWriter {
            normal_written: false,
        };
        let res = run_writer(out_rx, writer, Arc::clone(&stats), None);
        assert!(res.is_err());

        let snap = stats.snapshot();
        // Normal line succeeded
        assert_eq!(snap.observations_written, 1);
        // Anomaly line failed midway: never counted as written
        assert_eq!(snap.events_written, 0);
        assert_eq!(snap.write_errors, 1);
    }

    #[test]
    fn stdout_failure_allows_coordinator_to_continue_and_count_closed_drops() {
        let (obs_tx, obs_rx) = mpsc::channel(10);
        let (out_tx, out_rx) = mpsc::channel(10);
        let config = DetectionConfig::new(500).unwrap();
        let detector = Detector::new(config);
        let stats = DetectionStats::new_shared();

        // Simulate writer dying / closing receiver early
        drop(out_rx);

        // Send 3 observations after writer died
        for _ in 0..3 {
            obs_tx
                .try_send(Observation::new(
                    UNIX_EPOCH,
                    "svc".into(),
                    Method::GET,
                    "/after_death",
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Duration::from_millis(10),
                    RequestOutcome::Success,
                ))
                .unwrap();
        }
        drop(obs_tx);

        run_coordinator(obs_rx, out_tx, detector, Arc::clone(&stats), None);

        let snap = stats.snapshot();
        // Coordinator continued classifying despite dead stdout
        assert_eq!(snap.evaluated, 3);
        assert_eq!(snap.anomalous, 3);
        assert_eq!(snap.batches_queued, 0);
        assert_eq!(snap.observations_output_dropped_closed, 3);
        assert_eq!(snap.events_dropped_closed, 3);
    }

    #[tokio::test]
    async fn shutdown_distinguishes_coordinator_failure_from_clean_drain() {
        let config = DetectionConfig::new(500).unwrap();
        let detector = Detector::new(config);
        let stats = DetectionStats::new_shared();

        struct DevNull;
        impl Write for DevNull {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        // Spawn pipeline near MAX sequence to test coordinator failure in shutdown
        let (pipeline, tx) = TelemetryPipeline::spawn_with_initial_seq(
            10,
            detector,
            DevNull,
            Arc::clone(&stats),
            u64::MAX,
        )
        .unwrap();

        // 1st request: seq = u64::MAX
        tx.try_send(Observation::new(
            UNIX_EPOCH,
            "orders".into(),
            Method::GET,
            "/req1",
            StatusCode::OK,
            Duration::from_millis(10),
            RequestOutcome::Success,
        ))
        .unwrap();

        // 2nd request: triggers sequence overflow in coordinator
        tx.try_send(Observation::new(
            UNIX_EPOCH,
            "orders".into(),
            Method::GET,
            "/req2",
            StatusCode::OK,
            Duration::from_millis(10),
            RequestOutcome::Success,
        ))
        .unwrap();

        drop(tx);

        let res = pipeline.shutdown(Duration::from_secs(2)).await;
        assert!(res.is_err());
        let err_msg = res.unwrap_err().to_string();
        assert!(err_msg.contains("sequence overflow"));
    }
}
