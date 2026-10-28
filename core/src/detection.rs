//! Pure, deterministic classification of completed HTTP observations.
//!
//! A match describes evidence, not a root cause. No clock is sampled, no request
//! metadata is cloned, and no I/O, counters, queues, or proxy hooks are involved.
//! Callers opt in by constructing a [`Detector`] and evaluating observations.
//!
//! ```
//! use core_engine::{Observation, RequestOutcome};
//! use core_engine::detection::{AnomalyKind, DetectionConfig, Detector, Severity};
//! use std::time::{Duration, UNIX_EPOCH};
//!
//! let detector = Detector::new(DetectionConfig::new(750)?);
//! let observation = Observation::new(
//!     UNIX_EPOCH, "orders".into(), "GET".parse()?, "/items?token=secret",
//!     503u16.try_into()?, Duration::from_millis(800), RequestOutcome::Success,
//! );
//! let anomaly = detector.evaluate(&observation).expect("two rules match");
//! assert_eq!(anomaly.severity(), Severity::Error);
//! assert_eq!(anomaly.matched_rules().iter().collect::<Vec<_>>(), vec![
//!     AnomalyKind::HttpServerError, AnomalyKind::ElevatedRequestDuration,
//! ]);
//! assert_eq!(anomaly.duration_threshold(), Some(Duration::from_millis(750)));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::{
    fmt,
    time::{Duration, SystemTime},
};

use hyper::StatusCode;

use crate::observation::{Observation, RequestOutcome};

/// Immutable duration-rule configuration, expressed in whole milliseconds.
///
/// Zero is invalid; it does not disable a rule. CLI parsing and enablement are
/// deliberately outside this model. Invalid configurations cannot be constructed
/// by setting fields directly:
///
/// ```compile_fail
/// use core_engine::detection::DetectionConfig;
/// use std::time::Duration;
/// let mut config = DetectionConfig::default();
/// config.latency_threshold = Duration::ZERO;
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetectionConfig {
    latency_threshold: Duration,
}

impl DetectionConfig {
    /// Smallest accepted threshold, in milliseconds.
    pub const MIN_THRESHOLD_MS: u64 = 1;
    /// Largest accepted threshold, in milliseconds (one hour).
    pub const MAX_THRESHOLD_MS: u64 = 3_600_000;
    /// Default threshold, in milliseconds (one second).
    pub const DEFAULT_THRESHOLD_MS: u64 = 1_000;

    /// Validate a threshold without clamping it or silently choosing a default.
    ///
    /// # Errors
    /// Returns [`DetectionConfigError`] outside the inclusive range
    /// [`Self::MIN_THRESHOLD_MS`]–[`Self::MAX_THRESHOLD_MS`].
    pub fn new(threshold_ms: u64) -> Result<Self, DetectionConfigError> {
        if !(Self::MIN_THRESHOLD_MS..=Self::MAX_THRESHOLD_MS).contains(&threshold_ms) {
            return Err(DetectionConfigError { threshold_ms });
        }
        Ok(Self {
            latency_threshold: Duration::from_millis(threshold_ms),
        })
    }

    /// Threshold compared directly with the observation's exact duration.
    pub const fn latency_threshold(&self) -> Duration {
        self.latency_threshold
    }
}

impl Default for DetectionConfig {
    fn default() -> Self {
        Self {
            latency_threshold: Duration::from_millis(Self::DEFAULT_THRESHOLD_MS),
        }
    }
}

/// An invalid numeric threshold. Carries no request or service metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetectionConfigError {
    threshold_ms: u64,
}

impl DetectionConfigError {
    /// The rejected value, in milliseconds.
    pub const fn threshold_ms(&self) -> u64 {
        self.threshold_ms
    }
}

impl fmt::Display for DetectionConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "duration threshold must be {}..={} ms; received {} ms",
            DetectionConfig::MIN_THRESHOLD_MS,
            DetectionConfig::MAX_THRESHOLD_MS,
            self.threshold_ms,
        )
    }
}

impl std::error::Error for DetectionConfigError {}

/// The three independent predicates; none implies a cause or timed-out stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnomalyKind {
    /// The resulting HTTP status is within 500–599, regardless of its origin.
    HttpServerError,
    /// Exact handler-processing duration is strictly above the threshold.
    ElevatedRequestDuration,
    /// The observation explicitly has [`RequestOutcome::BackendTimeout`].
    UpstreamTimeout,
}

impl AnomalyKind {
    const fn bit(self) -> u8 {
        match self {
            Self::HttpServerError => 1,
            Self::ElevatedRequestDuration => 2,
            Self::UpstreamTimeout => 4,
        }
    }
}

/// Fixed-size matching reasons in canonical order, with no heap collection.
///
/// Only a detector can construct this type; externally obtainable sets are
/// nonempty and have no unknown bits. Empty or arbitrary sets cannot be made:
///
/// ```compile_fail
/// use core_engine::detection::MatchedRules;
/// let invalid = MatchedRules(0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchedRules(u8);

impl MatchedRules {
    /// Whether the given reason matched.
    pub const fn contains(self, kind: AnomalyKind) -> bool {
        self.0 & kind.bit() != 0
    }

    /// Number of matching reasons, from one to three for returned anomalies.
    pub const fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    /// Always false for sets returned in a detector-produced anomaly.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Iterate once per reason: HTTP 5xx, elevated duration, upstream timeout.
    pub fn iter(self) -> impl Iterator<Item = AnomalyKind> + Clone {
        [
            AnomalyKind::HttpServerError,
            AnomalyKind::ElevatedRequestDuration,
            AnomalyKind::UpstreamTimeout,
        ]
        .into_iter()
        .filter(move |kind| self.contains(*kind))
    }
}

/// Rule-derived attention level, not an impact estimate or paging policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Elevated duration without an HTTP 5xx or upstream-timeout match.
    Warning,
    /// At least one HTTP 5xx or upstream-timeout match.
    Error,
}

/// Response provenance derived from the M3 outcome contract.
///
/// This does not independently validate external producers. A proxy response
/// with `BackendFailure` does not distinguish transport from protocol failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusOrigin {
    /// [`RequestOutcome::Success`], including backend-returned HTTP errors.
    BackendResponse,
    /// Any of the other M3 outcomes, describing a proxy-generated response.
    ProxyResponse,
}

/// One classification with all matching reasons and owned scalar evidence.
///
/// Retains no observation reference, service, method, target, body, header, or
/// explanatory string. There is no global event identity or delivery state.
/// Fields cannot be changed into inconsistent evidence after classification:
///
/// ```compile_fail
/// use core_engine::detection::Anomaly;
/// use std::time::Duration;
/// fn rewrite_evidence(anomaly: &mut Anomaly) {
///     anomaly.duration = Duration::ZERO;
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anomaly {
    timestamp: SystemTime,
    status: StatusCode,
    outcome: RequestOutcome,
    duration: Duration,
    origin: StatusOrigin,
    matched_rules: MatchedRules,
    severity: Severity,
    duration_threshold: Option<Duration>,
}

impl Anomaly {
    /// Original handler-entry timestamp, without sampling a new clock.
    pub const fn timestamp(&self) -> SystemTime {
        self.timestamp
    }

    /// Original backend or proxy HTTP status, without normalization.
    pub const fn status(&self) -> StatusCode {
        self.status
    }

    /// Original outcome; preserves the available evidence without inferring cause.
    pub const fn outcome(&self) -> RequestOutcome {
        self.outcome
    }

    /// Original exact handler-processing duration, not backend-only latency.
    pub const fn duration(&self) -> Duration {
        self.duration
    }

    /// Origin derived exclusively from the supplied M3 outcome.
    pub const fn origin(&self) -> StatusOrigin {
        self.origin
    }

    /// All matching reasons, in a fixed-size nonempty set.
    pub const fn matched_rules(&self) -> MatchedRules {
        self.matched_rules
    }

    /// Maximum severity contributed by any matching reason.
    pub const fn severity(&self) -> Severity {
        self.severity
    }

    /// Exact configured threshold, present only when duration strictly exceeded it.
    pub const fn duration_threshold(&self) -> Option<Duration> {
        self.duration_threshold
    }
}

/// Immutable, stateless classifier. Evaluation has no allocation, I/O, or history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detector {
    config: DetectionConfig,
}

impl Detector {
    /// Construct a detector with an already validated configuration.
    pub const fn new(config: DetectionConfig) -> Self {
        Self { config }
    }

    /// Return a copy of the immutable configuration.
    pub const fn config(&self) -> DetectionConfig {
        self.config
    }

    /// Evaluate independent predicates and return at most one anomaly.
    ///
    /// No match does not imply application success. Unusual status/outcome
    /// combinations are evaluated literally: status alone never implies a
    /// timeout, and the original evidence is never rewritten. Duration uses
    /// strict `>` without conversion to rounded milliseconds.
    pub fn evaluate(&self, observation: &Observation) -> Option<Anomaly> {
        let mut matched_rules = MatchedRules(0);
        if (500..=599).contains(&observation.status.as_u16()) {
            matched_rules.0 |= AnomalyKind::HttpServerError.bit();
        }
        if observation.duration > self.config.latency_threshold {
            matched_rules.0 |= AnomalyKind::ElevatedRequestDuration.bit();
        }
        if observation.outcome == RequestOutcome::BackendTimeout {
            matched_rules.0 |= AnomalyKind::UpstreamTimeout.bit();
        }
        if matched_rules.is_empty() {
            return None;
        }

        let severity = if matched_rules.contains(AnomalyKind::HttpServerError)
            || matched_rules.contains(AnomalyKind::UpstreamTimeout)
        {
            Severity::Error
        } else {
            Severity::Warning
        };
        let origin = match observation.outcome {
            RequestOutcome::Success => StatusOrigin::BackendResponse,
            RequestOutcome::BackendFailure
            | RequestOutcome::BackendTimeout
            | RequestOutcome::ClientFailure
            | RequestOutcome::ClientTimeout
            | RequestOutcome::Rejected => StatusOrigin::ProxyResponse,
        };

        Some(Anomaly {
            timestamp: observation.timestamp,
            status: observation.status,
            outcome: observation.outcome,
            duration: observation.duration,
            origin,
            matched_rules,
            severity,
            duration_threshold: matched_rules
                .contains(AnomalyKind::ElevatedRequestDuration)
                .then_some(self.config.latency_threshold),
        })
    }
}
