//! Clock abstraction.
//!
//! Production code threads `ClockHandle` through constructors so tests can
//! drive time deterministically. The pure reducer (RFC-0009 §3) takes
//! `now: DateTime<Utc>` as a parameter and never reads it from a global —
//! consumers obtain `now` from a `Clock` at the impure boundary and pass it
//! down.
//!
//! Two methods on the trait:
//! - `now()` — wallclock `DateTime<Utc>`, agent-reported into events
//!   (RFC-0008 §4.2). Subject to NTP step-back.
//! - `monotonic_instant()` — `std::time::Instant`, for measuring elapsed
//!   durations (sustained-failure threshold, RFC-0008 §6). Never moves
//!   backwards under NTP step; `FakeClock` preserves this property too.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

pub type ClockHandle = Arc<dyn Clock>;

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
    fn monotonic_instant(&self) -> Instant;
}

pub struct SystemClock;

impl SystemClock {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }

    fn monotonic_instant(&self) -> Instant {
        Instant::now()
    }
}

pub struct FakeClock {
    state: Mutex<FakeState>,
}

struct FakeState {
    wall: DateTime<Utc>,
    monotonic: Instant,
}

impl FakeClock {
    pub fn new(initial_wall: DateTime<Utc>) -> Self {
        Self {
            state: Mutex::new(FakeState {
                wall: initial_wall,
                monotonic: Instant::now(),
            }),
        }
    }

    /// Advance both wallclock and monotonic by the same duration.
    pub fn advance(&self, by: Duration) {
        let mut s = self.state.lock().expect("FakeClock state poisoned");
        s.wall += chrono::Duration::from_std(by).expect("FakeClock advance overflow");
        s.monotonic += by;
    }

    /// Set the wallclock absolutely. Does not affect monotonic — same shape
    /// as a real NTP step: wall jumps, monotonic doesn't.
    pub fn set(&self, wall: DateTime<Utc>) {
        let mut s = self.state.lock().expect("FakeClock state poisoned");
        s.wall = wall;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> DateTime<Utc> {
        self.state.lock().expect("FakeClock state poisoned").wall
    }

    fn monotonic_instant(&self) -> Instant {
        self.state
            .lock()
            .expect("FakeClock state poisoned")
            .monotonic
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> DateTime<Utc> {
        "2026-05-16T00:00:00Z".parse().unwrap()
    }

    #[test]
    fn advance_moves_wall_and_monotonic_together() {
        let c = FakeClock::new(t0());
        let m0 = c.monotonic_instant();
        c.advance(Duration::from_secs(60));
        assert_eq!(c.now() - t0(), chrono::Duration::seconds(60));
        assert_eq!(c.monotonic_instant() - m0, Duration::from_secs(60));
    }

    #[test]
    fn set_moves_wall_only() {
        let c = FakeClock::new(t0());
        let m0 = c.monotonic_instant();
        let later = t0() + chrono::Duration::hours(1);
        c.set(later);
        assert_eq!(c.now(), later);
        assert_eq!(c.monotonic_instant(), m0);
    }

    #[test]
    fn monotonic_never_regresses_across_advances() {
        let c = FakeClock::new(t0());
        let m0 = c.monotonic_instant();
        c.advance(Duration::from_secs(30));
        let m1 = c.monotonic_instant();
        c.advance(Duration::from_secs(30));
        let m2 = c.monotonic_instant();
        assert!(m1 > m0);
        assert!(m2 > m1);
    }

    #[test]
    fn system_clock_now_tracks_real_now() {
        let c = SystemClock::new();
        let drift = (Utc::now() - c.now()).num_milliseconds().abs();
        assert!(drift < 1000, "SystemClock drift {drift}ms");
    }

    #[test]
    fn fake_clock_dispatches_via_trait_object() {
        let fake = Arc::new(FakeClock::new(t0()));
        let handle: ClockHandle = fake.clone();
        assert_eq!(handle.now(), t0());
        fake.advance(Duration::from_secs(10));
        assert_eq!(handle.now() - t0(), chrono::Duration::seconds(10));
    }
}
