//! Hybrid logical clock. Wall time is stored so a person can read it; ordering
//! uses the clock compare, not file modification times.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Hlc {
    pub wall_ms: u64,
    pub counter: u64,
    pub device: String,
}

impl Hlc {
    pub fn new(device: impl Into<String>, wall_ms: u64) -> Self {
        Self { wall_ms, counter: 0, device: device.into() }
    }

    /// Compare by wall time, then counter, then device id bytes.
    pub fn ord(&self, other: &Self) -> Ordering {
        self.wall_ms
            .cmp(&other.wall_ms)
            .then(self.counter.cmp(&other.counter))
            .then(self.device.as_bytes().cmp(other.device.as_bytes()))
    }
}

impl PartialOrd for Hlc {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Hlc {
    fn cmp(&self, other: &Self) -> Ordering {
        self.ord(other)
    }
}

/// One device's clock. `tick` moves it forward for a local change.
#[derive(Clone, Debug)]
pub struct Clock {
    current: Hlc,
}

impl Clock {
    pub fn new(device: impl Into<String>, wall_ms: u64) -> Self {
        Self { current: Hlc::new(device, wall_ms) }
    }

    /// Continue a clock that was stored earlier.
    pub fn resume(current: Hlc) -> Self {
        Self { current }
    }

    pub fn current(&self) -> &Hlc {
        &self.current
    }

    /// Advance past both the local wall clock and any remote clock we have seen.
    pub fn tick(&mut self, now_ms: u64) -> Hlc {
        if now_ms > self.current.wall_ms {
            self.current.wall_ms = now_ms;
            self.current.counter = 0;
        } else {
            self.current.counter = self.current.counter.saturating_add(1);
        }
        self.current.clone()
    }

    /// Record a remote clock so the next local tick sorts after it.
    pub fn observe(&mut self, remote: &Hlc, now_ms: u64) {
        if remote.ord(&self.current) == Ordering::Greater || remote.wall_ms > self.current.wall_ms {
            self.current.wall_ms = remote.wall_ms.max(now_ms);
            self.current.counter = remote.counter;
        } else if now_ms > self.current.wall_ms {
            self.current.wall_ms = now_ms;
            self.current.counter = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_moves_forward_when_the_wall_clock_does() {
        let mut clock = Clock::new("a", 1_000);
        let first = clock.tick(1_000);
        let second = clock.tick(1_000);
        let third = clock.tick(2_000);
        assert!(second > first);
        assert_eq!(third.wall_ms, 2_000);
        assert_eq!(third.counter, 0);
        assert!(third > second);
    }

    #[test]
    fn equal_wall_and_counter_break_ties_by_device_id() {
        let a = Hlc { wall_ms: 5, counter: 1, device: "b".into() };
        let b = Hlc { wall_ms: 5, counter: 1, device: "a".into() };
        assert!(b < a);
    }

    #[test]
    fn observe_sorts_the_next_local_tick_after_a_remote_clock() {
        let mut clock = Clock::new("local", 10);
        clock.observe(&Hlc { wall_ms: 50, counter: 3, device: "remote".into() }, 10);
        let next = clock.tick(10);
        assert!(next > Hlc { wall_ms: 50, counter: 3, device: "remote".into() });
    }
}
