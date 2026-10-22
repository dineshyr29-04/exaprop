//! Structured observation model and privacy sanitization for HTTP requests.

use hyper::{Method, StatusCode};
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, SystemTime},
};

/// Shared, process-local queue counters. Clone to read them while a proxy runs.
#[derive(Debug, Clone, Default)]
pub struct ObservationStats {
    inner: Arc<QueueCounters>,
}

#[derive(Debug, Default)]
struct QueueCounters {
    enqueued: AtomicU64,
    full: AtomicU64,
    closed: AtomicU64,
}

/// A queue-counter snapshot. Concurrent updates may span the individual reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservationStatsSnapshot {
    pub enqueued: u64,
    pub dropped_full: u64,
    pub dropped_closed: u64,
}

impl ObservationStatsSnapshot {
    /// Failed queue sends, excluding cancelled requests and output-sink errors.
    pub fn dropped(self) -> u64 {
        self.dropped_full.saturating_add(self.dropped_closed)
    }
}

impl ObservationStats {
    pub fn snapshot(&self) -> ObservationStatsSnapshot {
        ObservationStatsSnapshot {
            enqueued: self.inner.enqueued.load(Ordering::Relaxed),
            dropped_full: self.inner.full.load(Ordering::Relaxed),
            dropped_closed: self.inner.closed.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn record_enqueued(&self) {
        self.inner.enqueued.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_full(&self) {
        self.inner.full.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_closed(&self) {
        self.inner.closed.fetch_add(1, Ordering::Relaxed);
    }
}

/// The final outcome of an observed HTTP transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestOutcome {
    /// Upstream exchange completed normally (even if status is 4xx or 5xx).
    Success,
    /// Upstream connection or response body failed, or violated proxy constraints.
    BackendFailure,
    /// Upstream backend connect or read exceeded deadline.
    BackendTimeout,
    /// Client request body failed after HTTP headers were parsed.
    ClientFailure,
    /// Client body read exceeded timeout.
    ClientTimeout,
    /// Request was rejected by the proxy due to protocol constraints or payload limits.
    Rejected,
}

/// A structured, factual record of an observed HTTP request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    /// System timestamp when request entered the proxy.
    pub timestamp: SystemTime,
    /// Service identifier configured for this proxy instance.
    pub service_name: String,
    /// HTTP method (GET, POST, etc.).
    pub method: Method,
    /// Request URI path, preserved verbatim. May contain sensitive identifiers.
    pub path: String,
    /// Sanitized query string with sensitive values redacted.
    pub query: Option<String>,
    /// Resulting HTTP status code (from upstream backend or proxy error response).
    pub status: StatusCode,
    /// Monotonic time from parsed-request handler entry to the emission point,
    /// after response buffering or a proxy error decision. Includes request-body
    /// reads and upstream work; excludes parsing headers, admission waiting,
    /// observation sanitization/enqueue/output, and downstream response delivery.
    pub duration: Duration,
    /// Structured classification of the transaction outcome.
    pub outcome: RequestOutcome,
}

impl Observation {
    /// Create a record, preserving the path and redacting recognized query values.
    pub fn new(
        timestamp: SystemTime,
        service_name: String,
        method: Method,
        raw_path_and_query: &str,
        status: StatusCode,
        duration: Duration,
        outcome: RequestOutcome,
    ) -> Self {
        let (path, query) = split_and_sanitize_path_and_query(raw_path_and_query);
        Self {
            timestamp,
            service_name,
            method,
            path,
            query,
            status,
            duration,
            outcome,
        }
    }
}

/// Split a raw path_and_query string and redact sensitive query parameter values.
pub fn split_and_sanitize_path_and_query(raw: &str) -> (String, Option<String>) {
    if let Some((path, query)) = raw.split_once('?') {
        let sanitized_query = sanitize_query(query);
        (path.to_string(), Some(sanitized_query))
    } else {
        (raw.to_string(), None)
    }
}

/// Redact recognized sensitive and ambiguous query names without changing forwarding.
/// Both ampersands and semicolons are treated as potential telemetry separators.
pub fn sanitize_query(query: &str) -> String {
    if query.is_empty() {
        return String::new();
    }

    query
        .split_inclusive(['&', ';'])
        .map(|segment| {
            let (pair, separator) = if segment.ends_with(['&', ';']) {
                segment.split_at(segment.len() - 1)
            } else {
                (segment, "")
            };
            if pair.is_empty() {
                return segment.to_string();
            }
            let key = pair.split_once('=').map_or(pair, |(key, _)| key);
            if is_sensitive_key(key) {
                format!("{key}=[REDACTED]{separator}")
            } else {
                segment.to_string()
            }
        })
        .collect()
}

fn is_sensitive_key(key: &str) -> bool {
    // Match form-encoded names as the backend would see them. Preserve the
    // original spelling in the observation; only decode for classification.
    // Malformed escapes are redacted conservatively rather than risking a leak.
    let mut decoded = Vec::with_capacity(key.len());
    let mut bytes = key.bytes();
    while let Some(byte) = bytes.next() {
        match byte {
            b'%' => {
                let Some(high) = bytes.next().and_then(hex_digit) else {
                    return true;
                };
                let Some(low) = bytes.next().and_then(hex_digit) else {
                    return true;
                };
                decoded.push(high * 16 + low);
            }
            b'+' => decoded.push(b' '),
            other => decoded.push(other),
        }
    }
    let Ok(decoded) = String::from_utf8(decoded) else {
        return true;
    };
    // Nested escapes, Unicode/confusable names, controls, and embedded query
    // syntax are ambiguous across frameworks. Redact rather than guess.
    if !decoded.is_ascii()
        || decoded
            .bytes()
            .any(|byte| byte.is_ascii_control() || b"%&=;#".contains(&byte))
    {
        return true;
    }
    let lower: String = decoded
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect();
    if lower.is_empty() {
        return true;
    }
    lower.contains("token")
        || lower.contains("key")
        || lower.contains("secret")
        || lower.contains("auth")
        || lower.contains("password")
        || lower.contains("pass")
        || lower.contains("session")
        || lower.contains("sig")
        || lower.contains("bearer")
        || lower.contains("cred")
        || lower.contains("pwd")
        || lower.contains("jwt")
        || lower.contains("assertion")
        || lower == "code"
        || lower == "otp"
        || lower == "pin"
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_sensitive_query_parameters() {
        let raw = "/api/v1/user?token=secret123&page=2&apiKey=xyz987&sort=asc&user_password=hidden";
        let (path, query) = split_and_sanitize_path_and_query(raw);
        assert_eq!(path, "/api/v1/user");
        assert_eq!(
            query.as_deref(),
            Some("token=[REDACTED]&page=2&apiKey=[REDACTED]&sort=asc&user_password=[REDACTED]")
        );
    }

    #[test]
    fn handles_path_without_query() {
        let raw = "/health";
        let (path, query) = split_and_sanitize_path_and_query(raw);
        assert_eq!(path, "/health");
        assert_eq!(query, None);
    }

    #[test]
    fn redacts_encoded_case_insensitive_and_duplicate_keys() {
        assert_eq!(
            sanitize_query("%74oken=secret&API%5fKEY=hidden&token=again&token&page=2"),
            "%74oken=[REDACTED]&API%5fKEY=[REDACTED]&token=[REDACTED]&token=[REDACTED]&page=2"
        );
    }

    #[test]
    fn conservatively_redacts_malformed_parameter_names() {
        for query in ["%=secret", "%7=secret", "%GG=secret", "%FF=secret"] {
            assert!(sanitize_query(query).ends_with("=[REDACTED]"));
        }
    }

    #[test]
    fn preserves_non_sensitive_values_and_empty_query_structure() {
        assert_eq!(sanitize_query(""), "");
        assert_eq!(
            sanitize_query("page=1&&sort=a%26b&flag&"),
            "page=1&&sort=a%26b&flag&"
        );
        assert_eq!(
            split_and_sanitize_path_and_query("/items?"),
            ("/items".into(), Some(String::new()))
        );
    }

    #[test]
    fn redacts_ambiguous_and_unusually_named_parameters() {
        for key in [
            "%2574oken",
            "a.p.i-k+e+y",
            "p%61%00ss",
            "p%D0%B0ssword",
            "pwd",
            "JWT",
            "client_assertion",
            "code",
            "otp",
            "pin",
            "",
            "%26page",
            "---",
        ] {
            assert_eq!(
                sanitize_query(&format!("{key}=private")),
                format!("{key}=[REDACTED]"),
                "key: {key}"
            );
        }
        assert_eq!(
            sanitize_query("page=1;token=private&sort=asc"),
            "page=1;token=[REDACTED]&sort=asc"
        );
        assert_eq!(sanitize_query("page=1&&;sort=asc;"), "page=1&&;sort=asc;");
    }
}
