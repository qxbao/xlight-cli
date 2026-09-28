// SPDX-License-Identifier: GPL-3.0-only

//! Backoff helper for transient upstream failures (5xx, network errors, 429 without
//! `Retry-After`). Rate limiting still respects `retry_after` (INV-9: never switch accounts
//! instead of backing off).

use std::time::Duration;

/// Exponential backoff with a cap and optional jitter-free determinism (jitter is applied by the
/// caller if desired — kept out of this type so tests can assert exact delays).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Backoff {
    base: Duration,
    max: Duration,
    multiplier: u32,
}

impl Backoff {
    pub fn new(base: Duration, max: Duration) -> Self {
        Self {
            base,
            max,
            multiplier: 2,
        }
    }

    /// Delay before retry attempt `attempt` (0-indexed: the first retry is `attempt == 0`).
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let factor = self.multiplier.saturating_pow(attempt);
        self.base
            .checked_mul(factor)
            .unwrap_or(self.max)
            .min(self.max)
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new(Duration::from_millis(500), Duration::from_secs(30))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn delay_doubles_each_attempt() {
        let backoff = Backoff::new(Duration::from_millis(100), Duration::from_secs(60));
        assert_eq!(backoff.delay_for_attempt(0), Duration::from_millis(100));
        assert_eq!(backoff.delay_for_attempt(1), Duration::from_millis(200));
        assert_eq!(backoff.delay_for_attempt(2), Duration::from_millis(400));
    }

    #[test]
    fn delay_is_capped_at_max() {
        let backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(5));
        assert_eq!(backoff.delay_for_attempt(10), Duration::from_secs(5));
    }

    #[test]
    fn does_not_overflow_on_huge_attempt_numbers() {
        let backoff = Backoff::default();
        assert_eq!(backoff.delay_for_attempt(u32::MAX), backoff.max);
    }
}
