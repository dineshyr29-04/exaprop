//! M5.1: pure, opt-in planning for controlled faults on authorized test systems.
//!
//! This module does not delay requests, generate responses, change observations,
//! or perform I/O. A [`FaultPlan`] is intent, **not evidence of an applied fault**.
//! Existing M3/M4 proxy behavior and telemetry schemas are unaffected.
//!
//! # Safety contract
//!
//! - [`FaultPolicy::default`] is disabled. An enabled policy contains exactly one
//!   validated rule with an exact, case-sensitive method and raw encoded path.
//!   Queries are excluded from matching; no decoding or normalization is done.
//! - An effect is a 1..=5,000 ms delay or an HTTP 500..=599 synthetic error.
//!   Each selected request receives one effect, never a combination.
//! - Select every Nth eligible matching request, starting at N, for at most the
//!   configured number of selection slots. There is no randomness or clock input.
//! - A future executor must assign each eligible matching request a unique,
//!   increasing, nonzero ordinal for that policy/run, including under concurrency.
//!   Stop on ordinal exhaustion; never wrap, reuse a slot, or retry it after
//!   cancellation. This pure model cannot enforce an executor's global budget.
//! - Execution, cancellation, an operator disable mechanism, applied-fault evidence,
//!   and output accounting require separate integration. Do not infer execution
//!   from a returned plan or map a synthetic response to a backend failure.
//!   Eligibility must preserve existing HTTP validation/body limits; a plan must
//!   never authorize bypassing those checks. Shared/cloned policies must share
//!   one ordinal allocator in an executor, rather than restarting their budgets.
//! - Selectors may contain secrets. Debug output and validation errors omit them;
//!   plans retain only a nonsecret numeric rule ID and the scalar effect.
//!
//! ```
//! use core_engine::{fault::{FaultEffect, FaultPolicy, FaultRule, FaultSchedule,
//!     FaultSelector}, Method};
//! use std::num::{NonZeroU32, NonZeroU64};
//!
//! let rule = FaultRule::new(
//!     NonZeroU32::new(1).unwrap(),
//!     FaultSelector::new(Method::GET, "/orders")?,
//!     FaultEffect::delay_ms(100)?,
//!     FaultSchedule::new(2, 3)?, // Ordinals 2, 4 and 6; no later selections.
//! );
//! let policy = FaultPolicy::enabled(rule);
//! let plan = policy.plan(&Method::GET, "/orders", NonZeroU64::new(2).unwrap());
//! assert_eq!(plan.unwrap().effect().delay().unwrap().as_millis(), 100);
//! assert!(FaultPolicy::default().plan(&Method::GET, "/orders",
//!     NonZeroU64::new(2).unwrap()).is_none());
//! # Ok::<(), core_engine::fault::FaultConfigError>(())
//! ```

use std::{fmt, num::NonZeroU32, num::NonZeroU64, time::Duration};

use hyper::{Method, StatusCode};

/// Invalid configuration, deliberately excluding potentially sensitive input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultConfigError {
    InvalidDelay,
    InvalidStatus,
    InvalidMethod,
    InvalidPath,
    InvalidSchedule,
}

impl fmt::Display for FaultConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidDelay => "fault delay must be 1..=5000 milliseconds",
            Self::InvalidStatus => "synthetic fault status must be 500..=599",
            Self::InvalidMethod => "fault method must be a token of at most 32 bytes",
            Self::InvalidPath => "fault path must be an encoded absolute path of at most 256 bytes, without a query or fragment",
            Self::InvalidSchedule => "fault interval and selection budget must each be 1..=1000000",
        })
    }
}

impl std::error::Error for FaultConfigError {}

/// Effect category; selecting an effect does not execute it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultKind {
    Delay,
    HttpError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Effect {
    Delay(Duration),
    HttpError(StatusCode),
}

/// Validated, immutable scalar effect. Invalid effects cannot be constructed
/// through public fields, and there is no implicit/default effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaultEffect(Effect);

impl FaultEffect {
    pub const MAX_DELAY_MS: u64 = 5_000;

    /// Validate an integer delay in milliseconds, without clamping or rounding.
    pub fn delay_ms(milliseconds: u64) -> Result<Self, FaultConfigError> {
        if !(1..=Self::MAX_DELAY_MS).contains(&milliseconds) {
            return Err(FaultConfigError::InvalidDelay);
        }
        Ok(Self(Effect::Delay(Duration::from_millis(milliseconds))))
    }

    /// Validate an error response status. Informational/success/redirect/client
    /// statuses are deliberately outside this initial model.
    pub fn http_error(status: u16) -> Result<Self, FaultConfigError> {
        if !(500..=599).contains(&status) {
            return Err(FaultConfigError::InvalidStatus);
        }
        let status = StatusCode::from_u16(status).map_err(|_| FaultConfigError::InvalidStatus)?;
        Ok(Self(Effect::HttpError(status)))
    }

    pub const fn kind(self) -> FaultKind {
        match self.0 {
            Effect::Delay(_) => FaultKind::Delay,
            Effect::HttpError(_) => FaultKind::HttpError,
        }
    }

    pub const fn delay(self) -> Option<Duration> {
        match self.0 {
            Effect::Delay(delay) => Some(delay),
            Effect::HttpError(_) => None,
        }
    }

    pub const fn status(self) -> Option<StatusCode> {
        match self.0 {
            Effect::HttpError(status) => Some(status),
            Effect::Delay(_) => None,
        }
    }
}

/// Exact method/raw-path selector, bounded and copied only at configuration time.
/// Debug intentionally hides both fields; this is a selector, not access control.
#[derive(Clone, PartialEq, Eq)]
pub struct FaultSelector {
    method: Method,
    path: Box<str>,
}

impl fmt::Debug for FaultSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FaultSelector").finish_non_exhaustive()
    }
}

impl FaultSelector {
    pub const MAX_METHOD_BYTES: usize = 32;
    pub const MAX_PATH_BYTES: usize = 256;

    /// Require a valid HTTP method and ASCII encoded path beginning with `/`.
    /// Reject spaces, controls, non-URI bytes, queries, fragments and malformed
    /// percent escapes. Valid percent escapes remain verbatim and case-sensitive.
    pub fn new(method: Method, path: &str) -> Result<Self, FaultConfigError> {
        if method.as_str().len() > Self::MAX_METHOD_BYTES {
            return Err(FaultConfigError::InvalidMethod);
        }
        if path.len() > Self::MAX_PATH_BYTES || !valid_path(path) {
            return Err(FaultConfigError::InvalidPath);
        }
        Ok(Self {
            method,
            path: path.into(),
        })
    }

    /// Compare an already separated raw path. Passing a path-and-query will not
    /// match. No body, header, query, normalization or framework routing is read.
    pub fn matches(&self, method: &Method, raw_path: &str) -> bool {
        self.method == *method && self.path.as_ref() == raw_path
    }
}

fn valid_path(path: &str) -> bool {
    if !path.starts_with('/') {
        return false;
    }
    let mut bytes = path.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            if !bytes.next().is_some_and(|b| b.is_ascii_hexdigit())
                || !bytes.next().is_some_and(|b| b.is_ascii_hexdigit())
            {
                return false;
            }
        } else if !byte.is_ascii_alphanumeric() && !b"/-._~!$&'()*+,;=:@".contains(&byte) {
            return false;
        }
    }
    true
}

/// Immutable deterministic selection schedule. Both values are required;
/// neither zero nor an unbounded budget has a special meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaultSchedule {
    every_nth: u32,
    max_faults: u32,
}

impl FaultSchedule {
    pub const MAX_INTERVAL: u32 = 1_000_000;
    pub const MAX_FAULTS: u32 = 1_000_000;

    pub fn new(every_nth: u32, max_faults: u32) -> Result<Self, FaultConfigError> {
        if !(1..=Self::MAX_INTERVAL).contains(&every_nth)
            || !(1..=Self::MAX_FAULTS).contains(&max_faults)
        {
            return Err(FaultConfigError::InvalidSchedule);
        }
        Ok(Self {
            every_nth,
            max_faults,
        })
    }

    pub const fn every_nth(self) -> u32 {
        self.every_nth
    }

    pub const fn max_faults(self) -> u32 {
        self.max_faults
    }

    /// The ordinal counts eligible *matching* requests for one policy/run.
    /// Quotient/remainder arithmetic avoids multiplication/addition overflow.
    pub fn selects(self, matching_ordinal: NonZeroU64) -> bool {
        let ordinal = matching_ordinal.get();
        let interval = u64::from(self.every_nth);
        ordinal.is_multiple_of(interval) && ordinal / interval <= u64::from(self.max_faults)
    }
}

/// One validated rule. Numeric IDs must be nonsecret and nonzero; they are local
/// labels, not globally unique identities or idempotency tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultRule {
    id: NonZeroU32,
    selector: FaultSelector,
    effect: FaultEffect,
    schedule: FaultSchedule,
}

impl FaultRule {
    pub const fn new(
        id: NonZeroU32,
        selector: FaultSelector,
        effect: FaultEffect,
        schedule: FaultSchedule,
    ) -> Self {
        Self {
            id,
            selector,
            effect,
            schedule,
        }
    }

    pub const fn id(&self) -> NonZeroU32 {
        self.id
    }

    pub const fn effect(&self) -> FaultEffect {
        self.effect
    }

    pub const fn schedule(&self) -> FaultSchedule {
        self.schedule
    }

    pub fn matches(&self, method: &Method, raw_path: &str) -> bool {
        self.selector.matches(method, raw_path)
    }
}

/// Scalar-only intent, with no request reference, path, query, header or body.
/// A future executor must record applied/cancelled/failed evidence separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaultPlan {
    rule_id: NonZeroU32,
    effect: FaultEffect,
}

impl FaultPlan {
    pub const fn rule_id(self) -> NonZeroU32 {
        self.rule_id
    }

    pub const fn effect(self) -> FaultEffect {
        self.effect
    }
}

/// Disabled by default; enabling requires an explicitly constructed valid rule.
/// Planning borrows metadata transiently and has no allocation, I/O or mutation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FaultPolicy {
    rule: Option<FaultRule>,
}

impl FaultPolicy {
    pub const fn enabled(rule: FaultRule) -> Self {
        Self { rule: Some(rule) }
    }

    pub const fn rule(&self) -> Option<&FaultRule> {
        self.rule.as_ref()
    }

    /// Return at most one intent when the exact scope and selection slot match.
    /// The caller owns ordinal uniqueness and eligibility; repeated inputs return
    /// the same intent and do not consume any hidden execution budget.
    pub fn plan(
        &self,
        method: &Method,
        raw_path: &str,
        matching_ordinal: NonZeroU64,
    ) -> Option<FaultPlan> {
        let rule = self.rule.as_ref()?;
        if !rule.matches(method, raw_path) || !rule.schedule.selects(matching_ordinal) {
            return None;
        }
        Some(FaultPlan {
            rule_id: rule.id,
            effect: rule.effect,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ordinal(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).unwrap()
    }

    fn rule(effect: FaultEffect) -> FaultRule {
        FaultRule::new(
            NonZeroU32::new(7).unwrap(),
            FaultSelector::new(Method::GET, "/orders").unwrap(),
            effect,
            FaultSchedule::new(2, 3).unwrap(),
        )
    }

    #[test]
    fn disabled_policy_never_plans_a_fault() {
        let policy = FaultPolicy::default();
        assert!(policy.rule().is_none());
        for n in [1, 2, 6, u64::MAX] {
            assert_eq!(policy.plan(&Method::GET, "/orders", ordinal(n)), None);
        }
    }

    #[test]
    fn effect_limits_are_validated_without_clamping() {
        for delay in [1, 100, FaultEffect::MAX_DELAY_MS] {
            let effect = FaultEffect::delay_ms(delay).unwrap();
            assert_eq!(effect.kind(), FaultKind::Delay);
            assert_eq!(effect.delay(), Some(Duration::from_millis(delay)));
            assert_eq!(effect.status(), None);
        }
        for delay in [0, FaultEffect::MAX_DELAY_MS + 1, u64::MAX] {
            assert_eq!(
                FaultEffect::delay_ms(delay),
                Err(FaultConfigError::InvalidDelay)
            );
        }
        for status in [500, 503, 599] {
            let effect = FaultEffect::http_error(status).unwrap();
            assert_eq!(effect.kind(), FaultKind::HttpError);
            assert_eq!(effect.status().unwrap().as_u16(), status);
            assert_eq!(effect.delay(), None);
        }
        for status in [0, 101, 200, 400, 499, 600, 999, u16::MAX] {
            assert_eq!(
                FaultEffect::http_error(status),
                Err(FaultConfigError::InvalidStatus)
            );
        }
    }

    #[test]
    fn plans_require_scope_and_scheduled_slot() {
        for effect in [
            FaultEffect::delay_ms(25).unwrap(),
            FaultEffect::http_error(503).unwrap(),
        ] {
            let source = rule(effect);
            assert_eq!(source.id().get(), 7);
            assert_eq!(source.effect(), effect);
            assert_eq!(source.schedule().every_nth(), 2);
            assert_eq!(source.schedule().max_faults(), 3);
            let policy = FaultPolicy::enabled(source);
            for n in 1..=10 {
                let plan = policy.plan(&Method::GET, "/orders", ordinal(n));
                if [2, 4, 6].contains(&n) {
                    let plan = plan.unwrap();
                    assert_eq!(plan.rule_id().get(), 7);
                    assert_eq!(plan.effect(), effect);
                } else {
                    assert!(plan.is_none());
                }
                assert!(policy.plan(&Method::POST, "/orders", ordinal(n)).is_none());
                assert!(policy.plan(&Method::GET, "/other", ordinal(n)).is_none());
            }
        }
    }
}
