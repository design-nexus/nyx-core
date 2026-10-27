//! Retry delay for a failed sync or feed fetch. The cap matches the news
//! client's feed backoff. Jitter is deterministic for a given attempt so tests
//! can assert a range without sleeping.

use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delay {
    pub wait: Duration,
}

/// `attempt` starts at 1. A `retry_after` hint is honored up to 24 hours.
/// Otherwise the wait doubles from 30 seconds and stops at 6 hours.
pub fn delay(attempt: u32, retry_after: Option<Duration>) -> Delay {
    const CAP: Duration = Duration::from_secs(6 * 60 * 60);
    const HINT_CAP: Duration = Duration::from_secs(24 * 60 * 60);
    if let Some(hint) = retry_after {
        return Delay { wait: hint.min(HINT_CAP) };
    }
    let shift = attempt.saturating_sub(1).min(16);
    let base = Duration::from_secs(30u64.saturating_mul(1u64 << shift));
    let capped = base.min(CAP);
    let jitter = capped.as_secs() / 10;
    let nudged = capped.as_secs().saturating_sub(jitter / 2).saturating_add((attempt as u64 * 17) % jitter.max(1));
    Delay { wait: Duration::from_secs(nudged.min(CAP.as_secs())) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_is_capped_at_a_day() {
        let wait = delay(1, Some(Duration::from_secs(48 * 60 * 60))).wait;
        assert_eq!(wait, Duration::from_secs(24 * 60 * 60));
    }

    #[test]
    fn exponential_growth_stays_under_six_hours() {
        let first = delay(1, None).wait.as_secs();
        let later = delay(3, None).wait.as_secs();
        let far = delay(20, None).wait.as_secs();
        assert!(first < later);
        assert!(far <= 6 * 60 * 60);
        assert!(later < 6 * 60 * 60);
    }
}
