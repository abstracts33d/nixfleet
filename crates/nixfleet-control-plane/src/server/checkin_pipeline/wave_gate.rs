//! Per-channel wave-staging compliance gate evaluation. Lives
//! alongside `dispatch_target` because it gates dispatch decisions
//! on signature-verified failures from earlier waves.
//!
//! Note: there is also a `crate::wave_gate` module (top-level CP)
//! that exposes the pure `evaluate_channel_gate` function. This
//! module is the checkin-side caller — orchestration around the
//! pure evaluator.

use nixfleet_proto::agent_wire::CheckinRequest;

use super::super::state::AppState;
use super::dispatch_target::{stage_channel_hosts, wave_index_for};

/// Per-channel wave-staging compliance gate. Returns true iff dispatch
/// must be blocked (enforce mode + outstanding signature-verified
/// failures on an earlier wave). Permissive mode logs an advisory.
pub(super) async fn wave_gate_blocks_dispatch(
    state: &AppState,
    req: &CheckinRequest,
    fleet: &nixfleet_proto::FleetResolved,
) -> bool {
    let Some(channel_name) = fleet.hosts.get(&req.hostname).map(|h| &h.channel) else {
        return false;
    };
    let Some(channel) = fleet.channels.get(channel_name) else {
        return false;
    };
    let resolved_mode =
        nixfleet_proto::compliance::GateMode::from_wire_str(&channel.compliance.mode);

    let staged = stage_channel_hosts(state, fleet, channel_name).await;
    let requesting_wave = wave_index_for(fleet, channel_name, &req.hostname);

    let outcome = crate::wave_gate::evaluate_channel_gate(
        resolved_mode,
        requesting_wave,
        staged.iter().map(
            |(n, recs, rollout, wave_idx)| crate::wave_gate::HostGateInput {
                hostname: n.as_str(),
                records: recs.as_slice(),
                current_rollout: rollout.as_deref(),
                wave_index: *wave_idx,
            },
        ),
    );

    if outcome.blocks() {
        tracing::warn!(
            target: "dispatch",
            hostname = %req.hostname,
            channel = %channel_name,
            requesting_wave = ?requesting_wave,
            outcome = ?outcome,
            "dispatch: wave-staging gate blocked target (outstanding compliance failures)",
        );
        return true;
    }
    if matches!(
        outcome,
        crate::wave_gate::WaveGateOutcome::Permissive { failing_events_count } if failing_events_count > 0
    ) {
        tracing::info!(
            target: "dispatch",
            hostname = %req.hostname,
            channel = %channel_name,
            outcome = ?outcome,
            "dispatch: permissive mode — outstanding compliance failures advisory only",
        );
    }
    false
}
