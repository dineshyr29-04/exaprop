//! Reusable networking and proxy functionality for ExaProp.

pub mod diagnostics;
pub mod observation;
pub mod proxy;

pub use observation::{Observation, ObservationStats, ObservationStatsSnapshot, RequestOutcome};
