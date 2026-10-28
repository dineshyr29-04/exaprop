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

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::Method;
    use std::time::UNIX_EPOCH;

    fn observation(status: u16, outcome: RequestOutcome, duration: Duration) -> Observation {
        Observation {
            timestamp: UNIX_EPOCH + Duration::new(42, 123_456_789),
            service_name: "orders".into(),
            method: Method::GET,
            path: "/items".into(),
            query: Some("page=2".into()),
            status: StatusCode::from_u16(status).unwrap(),
            duration,
            outcome,
        }
    }

    fn assert_evidence(
        anomaly: Anomaly,
        source: &Observation,
        reasons: &[AnomalyKind],
        severity: Severity,
        origin: StatusOrigin,
        threshold: Option<Duration>,
    ) {
        assert_eq!(anomaly.timestamp(), source.timestamp);
        assert_eq!(anomaly.status(), source.status);
        assert_eq!(anomaly.outcome(), source.outcome);
        assert_eq!(anomaly.duration(), source.duration);
        assert_eq!(anomaly.origin(), origin);
        assert_eq!(anomaly.severity(), severity);
        assert_eq!(anomaly.duration_threshold(), threshold);
        assert!(!anomaly.matched_rules().is_empty());
        assert_eq!(anomaly.matched_rules().len(), reasons.len());
        assert_eq!(anomaly.matched_rules().iter().collect::<Vec<_>>(), reasons);
        for kind in [
            AnomalyKind::HttpServerError,
            AnomalyKind::ElevatedRequestDuration,
            AnomalyKind::UpstreamTimeout,
        ] {
            assert_eq!(
                anomaly.matched_rules().contains(kind),
                reasons.contains(&kind)
            );
        }
    }

    #[test]
    fn configuration_accepts_boundaries_and_defaults_to_one_second() {
        assert_eq!(
            DetectionConfig::default(),
            DetectionConfig::new(1_000).unwrap()
        );
        for threshold_ms in [1, 750, 1_000, 3_600_000] {
            let config = DetectionConfig::new(threshold_ms).unwrap();
            assert_eq!(
                config.latency_threshold(),
                Duration::from_millis(threshold_ms)
            );
            assert_eq!(Detector::new(config).config(), config);
        }
    }

    #[test]
    fn invalid_thresholds_return_typed_errors_without_clamping() {
        for threshold_ms in [0, 3_600_001, u64::MAX] {
            let error = DetectionConfig::new(threshold_ms).unwrap_err();
            assert_eq!(error.threshold_ms(), threshold_ms);
            assert_eq!(
                error.to_string(),
                format!("duration threshold must be 1..=3600000 ms; received {threshold_ms} ms")
            );
            let standard_error: &dyn std::error::Error = &error;
            assert!(standard_error.source().is_none());
        }
    }

    #[test]
    fn short_normal_and_client_error_responses_do_not_match() {
        let detector = Detector::new(DetectionConfig::default());
        for (status, outcome) in [
            (200, RequestOutcome::Success),
            (404, RequestOutcome::Success),
            (408, RequestOutcome::ClientTimeout),
            (400, RequestOutcome::ClientFailure),
            (413, RequestOutcome::Rejected),
            // A failure outcome alone is not one of the three predicates.
            (200, RequestOutcome::BackendFailure),
        ] {
            assert_eq!(
                detector.evaluate(&observation(status, outcome, Duration::from_millis(10))),
                None,
                "status={status}, outcome={outcome:?}"
            );
        }
    }

    #[test]
    fn http_server_error_uses_only_the_inclusive_5xx_range() {
        let detector = Detector::new(DetectionConfig::default());
        for status in 500..=599 {
            let source = observation(status, RequestOutcome::Success, Duration::ZERO);
            assert_evidence(
                detector.evaluate(&source).unwrap(),
                &source,
                &[AnomalyKind::HttpServerError],
                Severity::Error,
                StatusOrigin::BackendResponse,
                None,
            );
        }
        for status in [100, 199, 200, 404, 499, 600, 999] {
            assert_eq!(
                detector.evaluate(&observation(
                    status,
                    RequestOutcome::Success,
                    Duration::ZERO
                )),
                None,
                "status {status} is outside 500..=599"
            );
        }
    }

    #[test]
    fn duration_comparison_is_strict_to_the_nanosecond_for_custom_thresholds() {
        for threshold_ms in [1, 750, 1_000, 3_600_000] {
            let config = DetectionConfig::new(threshold_ms).unwrap();
            let threshold = config.latency_threshold();
            let detector = Detector::new(config);
            for duration in [
                Duration::ZERO,
                threshold - Duration::from_nanos(1),
                threshold,
            ] {
                assert_eq!(
                    detector.evaluate(&observation(200, RequestOutcome::Success, duration)),
                    None
                );
            }
            let source = observation(
                200,
                RequestOutcome::Success,
                threshold + Duration::from_nanos(1),
            );
            assert_evidence(
                detector.evaluate(&source).unwrap(),
                &source,
                &[AnomalyKind::ElevatedRequestDuration],
                Severity::Warning,
                StatusOrigin::BackendResponse,
                Some(threshold),
            );
        }
    }

    #[test]
    fn all_rule_combinations_form_one_canonical_reason_set() {
        use AnomalyKind::{
            ElevatedRequestDuration as D, HttpServerError as S, UpstreamTimeout as U,
        };
        use RequestOutcome::{BackendTimeout, Success};
        let detector = Detector::new(DetectionConfig::default());
        let threshold = Duration::from_secs(1);
        struct Case {
            status: u16,
            outcome: RequestOutcome,
            millis: u64,
            reasons: &'static [AnomalyKind],
            severity: Severity,
            origin: StatusOrigin,
        }
        let cases = [
            Case {
                status: 200,
                outcome: Success,
                millis: 10,
                reasons: &[],
                severity: Severity::Warning,
                origin: StatusOrigin::BackendResponse,
            },
            Case {
                status: 503,
                outcome: Success,
                millis: 10,
                reasons: &[S],
                severity: Severity::Error,
                origin: StatusOrigin::BackendResponse,
            },
            Case {
                status: 200,
                outcome: Success,
                millis: 1_001,
                reasons: &[D],
                severity: Severity::Warning,
                origin: StatusOrigin::BackendResponse,
            },
            Case {
                status: 200,
                outcome: BackendTimeout,
                millis: 10,
                reasons: &[U],
                severity: Severity::Error,
                origin: StatusOrigin::ProxyResponse,
            },
            Case {
                status: 503,
                outcome: Success,
                millis: 1_001,
                reasons: &[S, D],
                severity: Severity::Error,
                origin: StatusOrigin::BackendResponse,
            },
            Case {
                status: 503,
                outcome: BackendTimeout,
                millis: 10,
                reasons: &[S, U],
                severity: Severity::Error,
                origin: StatusOrigin::ProxyResponse,
            },
            Case {
                status: 200,
                outcome: BackendTimeout,
                millis: 1_001,
                reasons: &[D, U],
                severity: Severity::Error,
                origin: StatusOrigin::ProxyResponse,
            },
            Case {
                status: 504,
                outcome: BackendTimeout,
                millis: 15_000,
                reasons: &[S, D, U],
                severity: Severity::Error,
                origin: StatusOrigin::ProxyResponse,
            },
        ];
        for Case {
            status,
            outcome,
            millis,
            reasons,
            severity,
            origin,
        } in cases
        {
            let source = observation(status, outcome, Duration::from_millis(millis));
            let result = detector.evaluate(&source);
            if reasons.is_empty() {
                assert_eq!(result, None);
            } else {
                assert_evidence(
                    result.unwrap(),
                    &source,
                    reasons,
                    severity,
                    origin,
                    reasons.contains(&D).then_some(threshold),
                );
            }
        }
    }

    #[test]
    fn backend_error_status_does_not_infer_a_timeout() {
        let detector = Detector::new(DetectionConfig::default());
        for status in [502, 504] {
            for (outcome, origin) in [
                (RequestOutcome::Success, StatusOrigin::BackendResponse),
                (RequestOutcome::BackendFailure, StatusOrigin::ProxyResponse),
            ] {
                let source = observation(status, outcome, Duration::from_millis(10));
                assert_evidence(
                    detector.evaluate(&source).unwrap(),
                    &source,
                    &[AnomalyKind::HttpServerError],
                    Severity::Error,
                    origin,
                    None,
                );
            }
        }
    }

    #[test]
    fn upstream_timeout_matches_its_outcome_even_without_a_5xx_status() {
        let detector = Detector::new(DetectionConfig::default());
        for status in [200, 404, 408] {
            let source = observation(status, RequestOutcome::BackendTimeout, Duration::ZERO);
            assert_evidence(
                detector.evaluate(&source).unwrap(),
                &source,
                &[AnomalyKind::UpstreamTimeout],
                Severity::Error,
                StatusOrigin::ProxyResponse,
                None,
            );
        }
    }

    #[test]
    fn long_client_timeout_only_matches_elevated_duration() {
        let detector = Detector::new(DetectionConfig::default());
        let source = observation(408, RequestOutcome::ClientTimeout, Duration::from_secs(15));
        assert_evidence(
            detector.evaluate(&source).unwrap(),
            &source,
            &[AnomalyKind::ElevatedRequestDuration],
            Severity::Warning,
            StatusOrigin::ProxyResponse,
            Some(Duration::from_secs(1)),
        );
    }

    #[test]
    fn provenance_preserves_every_outcome_including_synthetic_combinations() {
        let detector = Detector::new(DetectionConfig::default());
        for (outcome, origin) in [
            (RequestOutcome::Success, StatusOrigin::BackendResponse),
            (RequestOutcome::BackendFailure, StatusOrigin::ProxyResponse),
            (RequestOutcome::BackendTimeout, StatusOrigin::ProxyResponse),
            (RequestOutcome::ClientFailure, StatusOrigin::ProxyResponse),
            (RequestOutcome::ClientTimeout, StatusOrigin::ProxyResponse),
            (RequestOutcome::Rejected, StatusOrigin::ProxyResponse),
        ] {
            let source = observation(501, outcome, Duration::ZERO);
            let reasons: &[AnomalyKind] = if outcome == RequestOutcome::BackendTimeout {
                &[AnomalyKind::HttpServerError, AnomalyKind::UpstreamTimeout]
            } else {
                &[AnomalyKind::HttpServerError]
            };
            assert_evidence(
                detector.evaluate(&source).unwrap(),
                &source,
                reasons,
                Severity::Error,
                origin,
                None,
            );
        }
    }

    #[test]
    fn extreme_duration_and_pre_epoch_timestamp_are_copied_without_conversion() {
        let detector = Detector::new(DetectionConfig::default());
        let mut source = observation(503, RequestOutcome::Success, Duration::MAX);
        source.timestamp = UNIX_EPOCH - Duration::from_nanos(1);
        assert_evidence(
            detector.evaluate(&source).unwrap(),
            &source,
            &[
                AnomalyKind::HttpServerError,
                AnomalyKind::ElevatedRequestDuration,
            ],
            Severity::Error,
            StatusOrigin::BackendResponse,
            Some(Duration::from_secs(1)),
        );
    }

    #[test]
    fn repeated_evaluation_has_no_history_or_observation_mutation() {
        let detector = Detector::new(DetectionConfig::new(750).unwrap());
        let source = observation(503, RequestOutcome::Success, Duration::from_millis(800));
        let original = source.clone();
        let expected = detector.evaluate(&source).unwrap();
        for _ in 0..32 {
            assert_eq!(detector.evaluate(&source), Some(expected));
            assert_eq!(source, original);
        }
        assert_eq!(
            detector.config().latency_threshold(),
            Duration::from_millis(750)
        );
    }

    #[test]
    fn event_is_independent_of_sensitive_metadata_and_outlives_its_source() {
        let detector = Detector::new(DetectionConfig::default());
        let baseline = observation(503, RequestOutcome::Success, Duration::from_secs(2));
        let expected = detector.evaluate(&baseline).unwrap();
        let event = {
            let mut source = baseline.clone();
            source.service_name = "SERVICE_SECRET\n\u{1b}[31m".repeat(1_024);
            source.method = Method::from_bytes(b"PRIVATE_METHOD_SENTINEL").unwrap();
            source.path = "/PATH_SECRET/BODY_SECRET/HEADER_SECRET".repeat(1_024);
            // External producers can populate unfiltered fields. The model must
            // ignore them rather than assume that the M3 constructor ran.
            source.query = Some("token=QUERY_SECRET&%2574oken=NESTED_SECRET".repeat(1_024));
            detector.evaluate(&source).unwrap()
        };
        assert_eq!(event, expected);
        let debug = format!("{event:?}");
        for sentinel in [
            "SERVICE_SECRET",
            "PRIVATE_METHOD_SENTINEL",
            "PATH_SECRET",
            "BODY_SECRET",
            "HEADER_SECRET",
            "QUERY_SECRET",
            "NESTED_SECRET",
        ] {
            assert!(!debug.contains(sentinel));
        }

        // Exhaustive destructuring makes future event fields require an
        // intentional update to this privacy/evidence test.
        let Anomaly {
            timestamp,
            status,
            outcome,
            duration,
            origin,
            matched_rules,
            severity,
            duration_threshold,
        } = event;
        assert_eq!(timestamp, baseline.timestamp);
        assert_eq!(status, baseline.status);
        assert_eq!(outcome, baseline.outcome);
        assert_eq!(duration, baseline.duration);
        assert_eq!(origin, StatusOrigin::BackendResponse);
        assert_eq!(
            matched_rules.iter().collect::<Vec<_>>(),
            vec![
                AnomalyKind::HttpServerError,
                AnomalyKind::ElevatedRequestDuration,
            ]
        );
        assert_eq!(severity, Severity::Error);
        assert_eq!(duration_threshold, Some(Duration::from_secs(1)));
    }

    #[test]
    fn model_types_are_copy_send_sync_and_severity_has_the_specified_order() {
        fn assert_value_type<T: Copy + Send + Sync>() {}
        assert_value_type::<DetectionConfig>();
        assert_value_type::<DetectionConfigError>();
        assert_value_type::<Detector>();
        assert_value_type::<Anomaly>();
        assert_value_type::<AnomalyKind>();
        assert_value_type::<MatchedRules>();
        assert_value_type::<Severity>();
        assert_value_type::<StatusOrigin>();
        assert!(Severity::Error > Severity::Warning);
        assert!(!std::mem::needs_drop::<Anomaly>());
    }

    #[test]
    fn shared_detector_evaluates_concurrently_without_mutable_state() {
        let detector = Detector::new(DetectionConfig::default());
        let sources = [
            observation(200, RequestOutcome::Success, Duration::ZERO),
            observation(500, RequestOutcome::Success, Duration::ZERO),
            observation(504, RequestOutcome::BackendTimeout, Duration::from_secs(15)),
            observation(408, RequestOutcome::ClientTimeout, Duration::from_secs(15)),
        ];
        let expected = sources.each_ref().map(|source| detector.evaluate(source));
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        for _ in 0..64 {
                            for (source, result) in sources.iter().zip(expected) {
                                assert_eq!(detector.evaluate(source), result);
                            }
                        }
                    })
                })
                .collect();
            for handle in handles {
                handle.join().unwrap();
            }
        });
    }
}
