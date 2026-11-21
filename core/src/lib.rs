//! Reusable networking and proxy functionality for ExaProp.

mod counters;

pub mod detection;
pub mod diagnostics;
pub mod fault;
pub mod observation;
pub mod proxy;

pub use hyper::{Method, StatusCode};
pub use observation::{Observation, ObservationStats, ObservationStatsSnapshot, RequestOutcome};
