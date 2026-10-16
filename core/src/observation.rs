//! Structured observation model and privacy sanitization for HTTP requests.

use hyper::{Method, StatusCode};
use std::time::{Duration, SystemTime};

/// The final outcome of an observed HTTP transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestOutcome {
    /// Upstream exchange completed normally (even if status is 4xx or 5xx).
    Success,
    /// Upstream backend connection refused, reset, or broken before response headers.
    BackendFailure,
    /// Upstream backend connect or read exceeded deadline.
    BackendTimeout,
    /// Client disconnected prematurely, timed out, or sent malformed bytes.
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
    /// Request URI path.
    pub path: String,
    /// Sanitized query string with sensitive values redacted.
    pub query: Option<String>,
    /// Resulting HTTP status code (from upstream backend or proxy error response).
    pub status: StatusCode,
    /// Monotonic processing duration from proxy entry to buffer completion.
    pub duration: Duration,
    /// Structured classification of the transaction outcome.
    pub outcome: RequestOutcome,
}

impl Observation {
    /// Create a new observation record, automatically sanitizing the path and query.
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

/// Redact values for sensitive query keys (tokens, secrets, passwords, api keys, auth).
pub fn sanitize_query(query: &str) -> String {
    if query.is_empty() {
        return String::new();
    }

    query
        .split('&')
        .map(|pair| {
            if let Some((key, _value)) = pair.split_once('=') {
                if is_sensitive_key(key) {
                    format!("{key}=[REDACTED]")
                } else {
                    pair.to_string()
                }
            } else if is_sensitive_key(pair) {
                format!("{pair}=[REDACTED]")
            } else {
                pair.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
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
}
