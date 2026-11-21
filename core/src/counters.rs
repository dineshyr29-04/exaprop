//! Internal saturating updates for best-effort M3 observation/diagnostic counts.

use std::sync::atomic::{AtomicU64, Ordering};

/// Counts stop at MAX rather than wrap into misleading small values. At MAX,
/// exact totals are unavailable. Relaxed ordering is sufficient for counters;
/// channel/completion operations provide lifecycle synchronization separately.
pub(crate) fn increment(counter: &AtomicU64) {
    let mut current = counter.load(Ordering::Relaxed);
    while current < u64::MAX {
        match counter.compare_exchange_weak(
            current,
            current + 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(actual) => current = actual,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_updates_saturate_without_wraparound() {
        let counter = AtomicU64::new(u64::MAX - 1);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..64 {
                        increment(&counter);
                    }
                });
            }
        });
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }
}
