//! Transition dispatcher. Routes `(state, event)` to the per-source-state
//! handler in a sibling module, after performing the universal pre-checks
//! (seq monotonicity, basic structural validity).
//!
//! Each per-state module pattern-matches the events legal from that source
//! state and returns either the new state + effects, or `IllegalForState`
//! for events the §3 graph doesn't permit from that source. The runtime
//! layer is expected to log + drop illegal events (out-of-order retransmits)
//! and rely on `Replay-From` heartbeat drift-detection (RFC-0008 §4.3) for
//! gap recovery.

mod activating;
mod converged;
mod failed;
mod pending;
mod reverted;
mod soaking;

use chrono::{DateTime, Utc};
use nixfleet_proto::RolloutPolicy;

use crate::effect::Effect;
use crate::error::TransitionError;
use crate::event::Event;
use crate::state::{HostRolloutState, HostState};

pub(crate) fn dispatch(
    state: HostRolloutState,
    event: Event,
    now: DateTime<Utc>,
    policy: &RolloutPolicy,
) -> Result<(HostRolloutState, Vec<Effect>), TransitionError> {
    // Universal pre-check: event seq must be strictly greater than the last
    // event we've seen for this (rollout, host). Equal seq = retransmit
    // (idempotent at the runtime layer via (host, rollout, seq) dedup); less
    // than = stale out-of-order arrival.
    let got = event.seq();
    if got <= state.last_event_seq {
        return Err(TransitionError::SeqRegression {
            got,
            last: state.last_event_seq,
            rollout_id: state.rollout_id.clone(),
            hostname: state.hostname.clone(),
        });
    }

    match state.state {
        HostState::Pending => pending::handle(state, event, now, policy),
        HostState::Activating => activating::handle(state, event, now, policy),
        HostState::Soaking => soaking::handle(state, event, now, policy),
        HostState::Failed => failed::handle(state, event, now, policy),
        HostState::Converged => converged::handle(state, event, now, policy),
        HostState::Reverted => reverted::handle(state, event, now, policy),
    }
}

/// Shared utility — variant name for diagnostic strings.
pub(crate) fn event_name(event: &Event) -> &'static str {
    match event {
        Event::LocalActivate { .. } => "LocalActivate",
        Event::LocalActivationStarted { .. } => "LocalActivationStarted",
        Event::LocalActivationCompleted { .. } => "LocalActivationCompleted",
        Event::LocalActivationDeferred { .. } => "LocalActivationDeferred",
        Event::LocalActivationFailed { .. } => "LocalActivationFailed",
        Event::LocalProbeTopologyDeclared { .. } => "LocalProbeTopologyDeclared",
        Event::LocalProbeObservedFirst { .. } => "LocalProbeObservedFirst",
        Event::LocalProbeResult { .. } => "LocalProbeResult",
        Event::LocalProbeFailureFirst { .. } => "LocalProbeFailureFirst",
        Event::LocalSustainedFailureCrossed { .. } => "LocalSustainedFailureCrossed",
        Event::LocalRollbackCompleted { .. } => "LocalRollbackCompleted",
        Event::LocalConvergedReached { .. } => "LocalConvergedReached",
        Event::RemoteDispatchAck { .. } => "RemoteDispatchAck",
        Event::RemoteActivationStarted { .. } => "RemoteActivationStarted",
        Event::RemoteActivationCompleted { .. } => "RemoteActivationCompleted",
        Event::RemoteActivationDeferred { .. } => "RemoteActivationDeferred",
        Event::RemoteActivationFailed { .. } => "RemoteActivationFailed",
        Event::RemoteProbeTopologyDeclared { .. } => "RemoteProbeTopologyDeclared",
        Event::RemoteProbeObservedFirst { .. } => "RemoteProbeObservedFirst",
        Event::RemoteProbeResult { .. } => "RemoteProbeResult",
        Event::RemoteProbeFailureFirst { .. } => "RemoteProbeFailureFirst",
        Event::RemoteFailed { .. } => "RemoteFailed",
        Event::RemoteRollbackComplete { .. } => "RemoteRollbackComplete",
        Event::RemoteConverged { .. } => "RemoteConverged",
    }
}

pub(crate) fn illegal(state: &HostRolloutState, event: &Event) -> TransitionError {
    TransitionError::IllegalForState {
        from: state.state,
        event: event_name(event),
        rollout_id: state.rollout_id.clone(),
        hostname: state.hostname.clone(),
    }
}
