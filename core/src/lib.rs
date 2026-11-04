//! Reusable networking and proxy functionality for ExaProp.

pub mod detection;
pub mod diagnostics;
pub mod observation;
pub mod proxy;

pub use hyper::{Method, StatusCode};
pub use observation::{Observation, ObservationStats, ObservationStatsSnapshot, RequestOutcome};
