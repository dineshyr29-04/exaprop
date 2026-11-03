//! Pipeline integration, bounded batch formatting, and telemetry accounting.
//!
//! Implements M4.2 consumer-side telemetry:
//! `Observation` receiver -> Coordinator -> Bounded `OutputBatch` queue -> Dedicated Writer.

use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

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
    #[inline]
    pub fn inc(&self, counter: &AtomicU64) {
        let prev = counter.fetch_add(1, Ordering::Relaxed);
        if prev == u64::MAX {
            counter.store(u64::MAX, Ordering::Relaxed);
            self.counters_saturated.store(true, Ordering::Relaxed);
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
