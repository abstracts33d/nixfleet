#![forbid(unsafe_code)]
//! Pure per-host rollout state-machine reducer.
//!
//! Implements RFC-0005 §3 (6-state machine) and RFC-0006 §3 (functional
//! core / imperative shell). The reducer is a single function:
//!
//! ```ignore
//! step(state, event, now, policy) -> Result<(state, Vec<Effect>), TransitionError>
//! ```
//!
//! Properties enforced by the crate's structure:
//!
//! - **No I/O.** `Cargo.toml` declares no tokio, no reqwest, no rusqlite.
//!   CI verifies via `cargo tree -p nixfleet-state-machine` (RFC-0006 §11).
//! - **No clock reads.** `chrono::Utc::now()` is not called anywhere in
//!   this crate; `now` is always a parameter.
//! - **Pure transitions.** `step` is deterministic for a given input.
//!   Same `(state, event, now, policy)` → same `(state, effects)`.
//! - **Side effects as data.** `Effect` is a description, not an execution.
//!   The agent runtime (RFC-0006 §7.1) and CP runtime (§7.2) each
//!   exhaustively match on `Effect` variants; adding a variant is a
//!   compiler-enforced change at every applier.
//!
//! ## Same code, both sides
//!
//! The CP-mirror view of per-host state (RFC-0006 §2 principle 4) runs the
//! same `step()` as the agent. `Event` carries both `Local*` (agent-side,
//! synthesized from worker output) and `Remote*` (CP-side, synthesized from
//! inbound agent events) variants — the transitions they drive are identical
//! by construction.

mod effect;
mod error;
mod event;
mod rehydration;
mod state;
mod transitions;
mod wire_conversions;

pub mod rollout;

pub use effect::{Effect, LogLevel, OutboundAgentEvent};
pub use error::TransitionError;
pub use event::{Event, ProbeTopologyEntry};
pub use rehydration::rehydration_effects;
pub use state::{
    ClosureHash, HostRolloutState, HostState, ProbeMode, ProbeName, ProbeRecord, ProbeStatus,
    ProbeSubResult, RolloutId,
};

use chrono::{DateTime, Utc};
use nixfleet_proto::RolloutPolicy;

/// Pure reducer. Same signature on agent and CP-mirror sides.
///
/// `now` is a parameter so tests can advance time deterministically and the
/// reducer cannot drift from any caller's notion of "current time" by
/// hitting a global clock.
///
/// `policy` is borrowed from the signed manifest the caller already verified
/// (via [`nixfleet_reconciler::verify_rollout_manifest`]). The reducer cannot
/// fetch it from a different source.
pub fn step(
    state: HostRolloutState,
    event: Event,
    now: DateTime<Utc>,
    policy: &RolloutPolicy,
) -> Result<(HostRolloutState, Vec<Effect>), TransitionError> {
    transitions::dispatch(state, event, now, policy)
}
