//! Per-checkin host-state hygiene that runs alongside dispatch:
//! the RFC-0002 §5.1 rollback-and-halt signal emission and the
//! "left Healthy" soak-marker sweep.
//!
//! Both touch `host_rollout_state` from the checkin path but do
//! not gate dispatch — dispatch decisions live in the sibling
//! `dispatch_target` module.

use nixfleet_proto::agent_wire::CheckinRequest;

use super::super::state::AppState;

/// RFC-0002 §5.1: when the host is `Failed` on a rollout whose
/// channel uses `on_health_failure = "rollback-and-halt"`, ship a
/// `RollbackSignal` so the agent re-activates its prior generation.
/// Idempotent at the protocol level — the signal keeps emitting
/// while the host's state stays Failed; once the agent's
/// `RollbackTriggered` post flips state to `Reverted`, this returns
/// None.
pub(super) async fn rollback_signal_for_checkin(
    state: &AppState,
    req: &CheckinRequest,
) -> Option<nixfleet_proto::agent_wire::RollbackSignal> {
    let db = state.db.as_ref()?;
    let fleet = state.verified_fleet.read().await.clone()?;
    let failed = match db.rollout_state().failed_rollouts_for_host(&req.hostname) {
        Ok(v) => v,
        Err(err) => {
            tracing::error!(
                hostname = %req.hostname,
                error = %err,
                "rollback-signal: failed_rollouts_for_host query failed",
            );
            return None;
        }
    };
    let signal = compute_rollback_signal(&fleet, &req.hostname, &failed)?;
    tracing::info!(
        target: "rollback-signal",
        hostname = %req.hostname,
        rollout = %signal.rollout,
        target_ref = %signal.target_ref,
        "rollback-signal: emitting RollbackSignal (policy: rollback-and-halt, host: Failed)",
    );
    Some(signal)
}

/// Pure decision: does the host's policy + failed-rollout list
/// produce a `RollbackSignal`? Extracted for unit-testability —
/// `rollback_signal_for_checkin` only adds the AppState plumbing
/// + journal logging on top.
///
/// Returns Some when:
/// - The host is declared in `fleet.hosts`,
/// - Its channel's rollout policy carries `RollbackAndHalt`,
/// - The host has at least one Failed rollout.
///
/// Multiple Failed rollouts for one host is degenerate; the first
/// is picked deterministically (caller's `failed_rollouts` ordering
/// is preserved by SQL `DISTINCT` over the indexed scan).
fn compute_rollback_signal(
    fleet: &nixfleet_proto::FleetResolved,
    hostname: &str,
    failed_rollouts: &[(String, String)],
) -> Option<nixfleet_proto::agent_wire::RollbackSignal> {
    let host = fleet.hosts.get(hostname)?;
    let channel = fleet.channels.get(&host.channel)?;
    let policy = fleet.rollout_policies.get(&channel.rollout_policy)?;
    if !matches!(
        policy.on_health_failure,
        nixfleet_proto::OnHealthFailure::RollbackAndHalt
    ) {
        return None;
    }
    let (rollout_id, target_ref) = failed_rollouts.first()?;
    Some(nixfleet_proto::agent_wire::RollbackSignal {
        rollout: rollout_id.clone(),
        target_ref: target_ref.clone(),
        reason: format!("policy: rollback-and-halt; host {} is Failed", hostname,),
    })
}

/// Per-checkin "left Healthy" sweep. Compares the reported
/// `current_generation.closure_hash` against each rollout the host
/// is currently recorded as Healthy in; on mismatch, clears the
/// Healthy marker so the soak timer restarts on the next confirm.
/// Best-effort: errors log + return without affecting dispatch —
/// the reconciler re-derives on its next tick. Runs before
/// `dispatch_target_for_checkin` so soak-state hygiene is in place
/// before any new target is issued.
pub(super) async fn clear_left_healthy_for_checkin(state: &AppState, req: &CheckinRequest) {
    let Some(db) = state.db.as_ref() else {
        return;
    };
    let healthy = match db.rollout_state().healthy_rollouts_for_host(&req.hostname) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(
                hostname = %req.hostname,
                error = %err,
                "checkin: healthy_rollouts_for_host query failed",
            );
            return;
        }
    };
    for (rollout_id, target_closure) in healthy {
        if req.current_generation.closure_hash == target_closure {
            continue;
        }
        match db
            .rollout_state()
            .clear_healthy_marker(&req.hostname, &rollout_id)
        {
            Ok(n) if n > 0 => {
                tracing::info!(
                    target: "soak",
                    hostname = %req.hostname,
                    rollout = %rollout_id,
                    target_closure = %target_closure,
                    current_closure = %req.current_generation.closure_hash,
                    "checkin: host left Healthy (closure mismatch); cleared soak timer",
                );
            }
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(
                    hostname = %req.hostname,
                    rollout = %rollout_id,
                    error = %err,
                    "checkin: clear_healthy_marker failed",
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::fleet_with_host;
    use super::*;

    /// Override the rollout policy on `fleet_with_host`'s default
    /// fleet so the `rollback_signal` tests can flip Halt ↔
    /// RollbackAndHalt without re-deriving the whole fixture.
    fn with_policy(
        mut fleet: nixfleet_proto::FleetResolved,
        policy: nixfleet_proto::OnHealthFailure,
    ) -> nixfleet_proto::FleetResolved {
        if let Some(p) = fleet.rollout_policies.get_mut("default") {
            p.on_health_failure = policy;
        }
        fleet
    }

    #[test]
    fn compute_rollback_signal_emits_under_rollback_and_halt() {
        let fleet = with_policy(
            fleet_with_host("test-host", Some("system-r1")),
            nixfleet_proto::OnHealthFailure::RollbackAndHalt,
        );
        let failed = vec![("stable@abc12345".to_string(), "ref-r1".to_string())];
        let signal =
            compute_rollback_signal(&fleet, "test-host", &failed).expect("signal expected");
        assert_eq!(signal.rollout, "stable@abc12345");
        assert_eq!(signal.target_ref, "ref-r1");
        assert!(
            signal.reason.contains("rollback-and-halt"),
            "reason should name the policy: {}",
            signal.reason,
        );
    }

    #[test]
    fn compute_rollback_signal_returns_none_under_halt() {
        // Policy-driven gate: pure `halt` never auto-rolls-back even
        // when the host has Failed rollouts.
        let fleet = with_policy(
            fleet_with_host("test-host", Some("system-r1")),
            nixfleet_proto::OnHealthFailure::Halt,
        );
        let failed = vec![("stable@abc12345".to_string(), "ref-r1".to_string())];
        assert!(compute_rollback_signal(&fleet, "test-host", &failed).is_none());
    }

    #[test]
    fn compute_rollback_signal_returns_none_when_no_failed_rollouts() {
        // Steady-state host: no Failed rows → no signal even under
        // rollback-and-halt. The signal stops emitting once the
        // agent's RollbackTriggered post flips state to Reverted
        // (db query no longer returns the row).
        let fleet = with_policy(
            fleet_with_host("test-host", Some("system-r1")),
            nixfleet_proto::OnHealthFailure::RollbackAndHalt,
        );
        assert!(compute_rollback_signal(&fleet, "test-host", &[]).is_none());
    }

    #[test]
    fn compute_rollback_signal_returns_none_when_host_unknown() {
        // Host not in fleet (e.g. just-removed): no signal — same
        // posture as the dispatch path.
        let fleet = with_policy(
            fleet_with_host("test-host", Some("system-r1")),
            nixfleet_proto::OnHealthFailure::RollbackAndHalt,
        );
        let failed = vec![("stable@abc12345".to_string(), "ref-r1".to_string())];
        assert!(compute_rollback_signal(&fleet, "ghost-host", &failed).is_none());
    }
}
