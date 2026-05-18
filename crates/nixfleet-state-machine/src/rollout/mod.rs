//! Pure rollout-level reducer (RFC-0008 §3). Parallel to the per-host
//! reducer (`crate::step`); same functional-core discipline (RFC-0006 §2):
//! no I/O, no clock reads, no shared mutable state, deterministic given
//! `(state, event, now)`.
//!
//! Phase 10a ships the skeleton (types + dispatch returning
//! `RolloutTransitionError::Unimplemented`); Phase 10b lands the
//! per-source-state transition bodies and proptest invariants.

pub mod effect;
pub mod error;
pub mod event;
pub mod state;

mod transitions;

#[cfg(test)]
mod tests;

pub use effect::RolloutEffect;
pub use error::RolloutTransitionError;
pub use event::{HostId, HostRolloutState, RolloutEvent};
pub use state::{ChannelId, ClosureHash, RolloutId, RolloutRecord, RolloutState};

use chrono::{DateTime, Utc};

/// Pure rollout-level reducer. CP-side only (rollout events are
/// synthesized from inbound agent events; agents never emit `RolloutEvent`s).
///
/// `now` is a parameter (no `chrono::Utc::now()` inside the crate); the
/// applier supplies it from its own `ClockHandle` so test fixtures with
/// `FakeClock` stay deterministic.
pub fn step(
    record: RolloutRecord,
    event: RolloutEvent,
    now: DateTime<Utc>,
) -> Result<(RolloutRecord, Vec<RolloutEffect>), RolloutTransitionError> {
    transitions::dispatch(record, event, now)
}
