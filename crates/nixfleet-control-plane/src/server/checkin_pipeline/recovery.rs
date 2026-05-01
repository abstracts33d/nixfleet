//! Orphan-confirm recovery (CP rebuild mid-flight) and soak-state
//! recovery from agent attestation.
//!
//! Both paths share a defensive posture: the CP only synthesises
//! state when the agent's claim matches what the verified fleet
//! says about the host. Closure-hash mismatch, missing fleet
//! snapshot, or missing host declaration → fall through (caller
//! decides 410 vs no-op).

use std::sync::Arc;

use chrono::{DateTime, Utc};
use nixfleet_proto::agent_wire::{CheckinRequest, ConfirmRequest};

use super::super::state::AppState;

/// CP-rebuild recovery for an orphan confirm. Returns `true` when
/// the CP can absorb the confirm without forcing rollback, `false`
/// when it should fall through to 410. All failures are non-fatal:
/// the agent's local rollback still fires on 410.
pub(super) async fn try_recover_orphan_confirm(
    state: &Arc<AppState>,
    req: &ConfirmRequest,
) -> bool {
    let Some(db) = state.db.as_ref() else {
        return false;
    };
    let Some((target_closure, channel)) = validate_orphan_recovery(state, req).await else {
        return false;
    };
    synthesise_orphan_confirm_rows(db, req, &target_closure, &channel)
}

/// Returns the validated target closure when the orphan confirm
/// matches the verified fleet's declared target for this host
/// (closure AND rollout id). None otherwise — caller falls through
/// to 410.
async fn validate_orphan_recovery(
    state: &AppState,
    req: &ConfirmRequest,
) -> Option<(String, String)> {
    let fleet = state.verified_fleet.read().await.clone().or_else(|| {
        tracing::debug!(
            hostname = %req.hostname,
            "orphan-confirm recovery: no verified fleet snapshot yet",
        );
        None
    })?;
    let fleet_resolved_hash = state.fleet_resolved_hash.read().await.clone().or_else(|| {
        tracing::debug!(
            hostname = %req.hostname,
            "orphan-confirm recovery: no fleet_resolved_hash yet (CP just booted?)",
        );
        None
    })?;
    let host_decl = fleet.hosts.get(&req.hostname).or_else(|| {
        tracing::debug!(
            hostname = %req.hostname,
            "orphan-confirm recovery: host not in verified fleet",
        );
        None
    })?;
    let target_closure = host_decl.closure_hash.as_ref().or_else(|| {
        tracing::debug!(
            hostname = %req.hostname,
            "orphan-confirm recovery: host has no declared closureHash",
        );
        None
    })?;
    if target_closure != &req.generation.closure_hash {
        tracing::info!(
            hostname = %req.hostname,
            rollout = %req.rollout,
            agent_closure = %req.generation.closure_hash,
            target_closure = %target_closure,
            "orphan-confirm recovery: closure_hash mismatch — genuine 410",
        );
        return None;
    }

    // Defensive: closure match doesn't prove `req.rollout` is THIS
    // fleet's rollout id. With content-addressed manifests (RFC-0002
    // §4.4), a CI re-sign with the same closure but different
    // host_set / wave_layout / etc. produces a different rolloutId,
    // and the cross-snapshot mismatch surfaces here.
    let expected_rollout_id = match nixfleet_reconciler::compute_rollout_id_for_channel(
        &fleet,
        &fleet_resolved_hash,
        &host_decl.channel,
    ) {
        Ok(Some(id)) => id,
        Ok(None) | Err(_) => {
            tracing::info!(
                hostname = %req.hostname,
                "orphan-confirm recovery: rolloutId could not be projected — genuine 410",
            );
            return None;
        }
    };
    if expected_rollout_id != req.rollout {
        tracing::info!(
            hostname = %req.hostname,
            agent_rollout = %req.rollout,
            expected_rollout = %expected_rollout_id,
            "orphan-confirm recovery: rollout id mismatch — agent is on a stale rollout, genuine 410",
        );
        return None;
    }

    Some((target_closure.clone(), host_decl.channel.clone()))
}

/// Insert the synthetic `pending_confirms` (confirmed) + Healthy
/// marker. Returns true iff the pending_confirms write succeeded;
/// the host_healthy write is best-effort (worst case the soak timer
/// restarts on next confirm — same as pre-recovery behaviour).
fn synthesise_orphan_confirm_rows(
    db: &crate::db::Db,
    req: &ConfirmRequest,
    target_closure: &str,
    channel: &str,
) -> bool {
    let now = Utc::now();
    if let Err(err) = db.confirms().record_confirmed_pending(
        &req.hostname,
        &req.rollout,
        channel,
        req.wave,
        target_closure,
        &req.rollout,
        now,
    ) {
        tracing::warn!(
            hostname = %req.hostname,
            rollout = %req.rollout,
            error = %err,
            "orphan-confirm recovery: record_confirmed_pending failed",
        );
        return false;
    }
    if let Err(err) = db.rollout_state().transition_host_state(
        &req.hostname,
        &req.rollout,
        crate::state::HostRolloutState::Healthy,
        crate::state::HealthyMarker::Set(now),
        None,
    ) {
        tracing::warn!(
            hostname = %req.hostname,
            rollout = %req.rollout,
            error = %err,
            "orphan-confirm recovery: transition to Healthy failed (synthetic row already inserted)",
        );
    }
    tracing::info!(
        target: "confirm",
        hostname = %req.hostname,
        rollout = %req.rollout,
        target_closure = %target_closure,
        "orphan-confirm recovery: synthesised confirmed pending_confirms row + Healthy marker",
    );
    true
}

/// Soak-state recovery from agent attestation.
///
/// After a CP rebuild, `host_rollout_state.last_healthy_since` is
/// gone for every host. Hosts that were mid-soak when the CP died
/// would otherwise restart their soak window from zero on the
/// next confirm, costing up to one full `soak_minutes` per
/// affected wave. The agent's `last_confirmed_at` attestation
/// (wire-additive field) lets the CP repopulate
/// `last_healthy_since` from the agent-known timestamp — bringing
/// the soak gate's effective state back close to its pre-rebuild
/// position.
///
/// Triggers when ALL of:
/// 1. Agent reports `last_confirmed_at` (legacy agents leave it
///    None, no-op for them).
/// 2. CP has a verified `FleetResolved` snapshot.
/// 3. The host is declared in the fleet with a `closureHash`.
/// 4. The host's reported `current_generation.closure_hash` matches
///    the declared target — i.e. it's converged on the live target.
/// 5. No `host_rollout_state` row already exists for
///    (rollout, host). An existing row reflects the actual
///    lifecycle (Healthy/Soaked/Reverted) and is more authoritative
///    than a re-attestation.
///
/// On success: synthesise a confirmed `pending_confirms` row +
/// a `host_rollout_state` Healthy marker stamped with
/// `min(now, last_confirmed_at)`. The clamp prevents a clock-
/// skewed agent from claiming future-dated state to short-circuit
/// the soak gate.
///
/// Trust model: the agent has root on its own host — the soak
/// gate is operator-policy, not a security boundary against the
/// host. Cross-checking against `boot_id` / `uptime_secs` is
/// available if a fleet wants stricter enforcement (out of scope
/// here).
pub(super) async fn recover_soak_state_from_attestation(
    state: &Arc<AppState>,
    req: &CheckinRequest,
    now: DateTime<Utc>,
) {
    let Some(attested) = req.last_confirmed_at else {
        return;
    };
    let Some(db) = state.db.as_ref() else {
        return;
    };
    let Some(fleet) = state.verified_fleet.read().await.clone() else {
        return;
    };
    let Some(fleet_resolved_hash) = state.fleet_resolved_hash.read().await.clone() else {
        return;
    };
    let Some(host_decl) = fleet.hosts.get(&req.hostname) else {
        return;
    };
    let Some(target_closure) = host_decl.closure_hash.as_ref() else {
        return;
    };
    if target_closure != &req.current_generation.closure_hash {
        return;
    }

    // The recovered row's rollout_id MUST match what dispatch would
    // emit so the per-rollout grouping in
    // `outstanding_compliance_events_by_rollout` lines up. Same
    // projection both dispatch and the orphan-confirm recovery path
    // call — single source of truth at
    // `nixfleet_reconciler::compute_rollout_id_for_channel`.
    let rollout_id = match nixfleet_reconciler::compute_rollout_id_for_channel(
        &fleet,
        &fleet_resolved_hash,
        &host_decl.channel,
    ) {
        Ok(Some(id)) => id,
        Ok(None) | Err(_) => return,
    };

    match db
        .rollout_state()
        .host_rollout_state_exists(&req.hostname, &rollout_id)
    {
        Ok(true) => return, // already known — leave alone
        Ok(false) => {}
        Err(err) => {
            tracing::warn!(
                hostname = %req.hostname,
                rollout = %rollout_id,
                error = %err,
                "soak-state recovery: existence check failed",
            );
            return;
        }
    }

    let stamp = std::cmp::min(now, attested);

    if let Err(err) = db.confirms().record_confirmed_pending(
        &req.hostname,
        &rollout_id,
        &host_decl.channel,
        0,
        target_closure,
        &rollout_id,
        now,
    ) {
        tracing::warn!(
            hostname = %req.hostname,
            rollout = %rollout_id,
            error = %err,
            "soak-state recovery: record_confirmed_pending failed",
        );
        return;
    }
    if let Err(err) = db.rollout_state().transition_host_state(
        &req.hostname,
        &rollout_id,
        crate::state::HostRolloutState::Healthy,
        crate::state::HealthyMarker::Set(stamp),
        None,
    ) {
        tracing::warn!(
            hostname = %req.hostname,
            rollout = %rollout_id,
            error = %err,
            "soak-state recovery: transition to Healthy failed (synthetic confirmed row already inserted)",
        );
        return;
    }
    tracing::info!(
        target: "soak",
        hostname = %req.hostname,
        rollout = %rollout_id,
        attested = %attested.to_rfc3339(),
        stamped = %stamp.to_rfc3339(),
        "soak-state recovery: stamped last_healthy_since from agent attestation",
    );
}

#[cfg(test)]
mod tests {
    use super::super::tests::{
        checkin_req_with_attestation, confirm_req, expected_rollout_id_for, fleet_with_host,
        state_with_fleet_and_db,
    };
    use super::*;
    use crate::db::Db;
    use std::sync::Arc;

    #[tokio::test]
    async fn orphan_recovery_succeeds_when_closure_matches() {
        // Happy path. CP rebuilt mid-flight; agent posts a confirm
        // whose closure matches the verified target. The recovery
        // path synthesises a confirmed row + Healthy marker and
        // returns true so the handler emits 204 instead of forcing a
        // local rollback.
        let fleet = fleet_with_host("test-host", Some("target-system-r1"));
        let expected_id = expected_rollout_id_for(&fleet, "stable");
        let (state, db) = state_with_fleet_and_db(fleet).await;
        let req = confirm_req("test-host", &expected_id, "target-system-r1");

        assert!(
            try_recover_orphan_confirm(&state, &req).await,
            "matching closure should recover",
        );

        let snap = db.rollout_state().active_rollouts_snapshot().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].rollout_id, expected_id);
        assert_eq!(snap[0].target_closure_hash, "target-system-r1");
        // Healthy marker stamped in the same call.
        let healthy = db
            .rollout_state()
            .healthy_rollouts_for_host("test-host")
            .unwrap();
        assert_eq!(healthy.len(), 1);
    }

    #[tokio::test]
    async fn orphan_recovery_rejects_closure_mismatch() {
        // Genuine wrong-rollout case. Agent claims to have
        // activated something the fleet doesn't agree with — must
        // fall through to 410.
        let fleet = fleet_with_host("test-host", Some("target-system-r1"));
        let (state, db) = state_with_fleet_and_db(fleet).await;
        let req = confirm_req("test-host", "stable@evil", "target-system-different");

        assert!(
            !try_recover_orphan_confirm(&state, &req).await,
            "mismatched closure must not recover",
        );
        assert!(db
            .rollout_state()
            .active_rollouts_snapshot()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn orphan_recovery_rejects_when_host_not_in_fleet() {
        // Agent claims to be a host the verified fleet doesn't
        // know about — recovery refuses to invent state for it.
        let fleet = fleet_with_host("known-host", Some("target"));
        let (state, _db) = state_with_fleet_and_db(fleet).await;
        let req = confirm_req("rogue-host", "stable@abc", "target");

        assert!(!try_recover_orphan_confirm(&state, &req).await);
    }

    #[tokio::test]
    async fn orphan_recovery_rejects_when_no_verified_fleet() {
        // First-boot CP with no verified snapshot yet — recovery
        // can't validate the agent's claim, so it stays
        // conservative.
        let db = Arc::new(Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let state = Arc::new(AppState {
            db: Some(Arc::clone(&db)),
            ..AppState::default()
        });
        let req = confirm_req("test-host", "stable@abc", "target");
        assert!(!try_recover_orphan_confirm(&state, &req).await);
    }

    #[tokio::test]
    async fn orphan_recovery_rejects_when_host_lacks_closure_declaration() {
        // The fleet lists the host but with no closureHash (CI
        // didn't produce one). Without a target to validate
        // against, recovery refuses.
        let fleet = fleet_with_host("test-host", None);
        let (state, _db) = state_with_fleet_and_db(fleet).await;
        let req = confirm_req("test-host", "stable@abc", "anything");
        assert!(!try_recover_orphan_confirm(&state, &req).await);
    }

    #[tokio::test]
    async fn b_cp_recovery_stamps_attested_timestamp_when_no_existing_row() {
        // Happy path. Host is converged on the verified target, no
        // host_rollout_state row exists (CP rebuilt), attestation
        // arrives → stamp last_healthy_since.
        let fleet = fleet_with_host("test-host", Some("system-r1"));
        let (state, db) = state_with_fleet_and_db(fleet).await;
        let attested = Utc::now() - chrono::Duration::minutes(3);
        let req = checkin_req_with_attestation("test-host", "system-r1", Some(attested));

        recover_soak_state_from_attestation(&state, &req, Utc::now()).await;

        let snap = db.rollout_state().active_rollouts_snapshot().unwrap();
        assert_eq!(
            snap.len(),
            1,
            "snapshot should contain the recovered rollout"
        );
        let stamped = snap[0]
            .last_healthy_since
            .get("test-host")
            .expect("host has stamped soak marker");
        assert_eq!(
            stamped.timestamp(),
            attested.timestamp(),
            "stamp must clamp to min(now, attested) — attested is in the past so it wins",
        );
    }

    #[tokio::test]
    async fn b_cp_recovery_clamps_future_attestation_to_now() {
        // Defensive clamp: a clock-skewed agent claims attestation
        // in the future. CP must clamp to `now` so the agent can't
        // short-circuit the soak gate.
        let fleet = fleet_with_host("test-host", Some("system-r1"));
        let (state, db) = state_with_fleet_and_db(fleet).await;
        let now = Utc::now();
        let future = now + chrono::Duration::minutes(60);
        let req = checkin_req_with_attestation("test-host", "system-r1", Some(future));

        recover_soak_state_from_attestation(&state, &req, now).await;

        let snap = db.rollout_state().active_rollouts_snapshot().unwrap();
        let stamped = snap[0].last_healthy_since.get("test-host").unwrap();
        assert_eq!(
            stamped.timestamp(),
            now.timestamp(),
            "future-dated attestation must clamp to now",
        );
    }

    #[tokio::test]
    async fn b_cp_recovery_skips_when_host_not_converged() {
        // Host reports a closure that doesn't match the verified
        // target — it's still rolling out, not in the recovery
        // window. Skip.
        let fleet = fleet_with_host("test-host", Some("target-r1"));
        let (state, db) = state_with_fleet_and_db(fleet).await;
        let attested = Utc::now() - chrono::Duration::minutes(1);
        let req = checkin_req_with_attestation("test-host", "different-closure", Some(attested));

        recover_soak_state_from_attestation(&state, &req, Utc::now()).await;
        assert!(db
            .rollout_state()
            .active_rollouts_snapshot()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn b_cp_recovery_skips_when_host_state_already_exists() {
        // host_rollout_state already has a row. Re-attestation must
        // NOT overwrite — the existing row is authoritative.
        let fleet = fleet_with_host("test-host", Some("system-r1"));
        let expected_id = expected_rollout_id_for(&fleet, "stable");
        let (state, db) = state_with_fleet_and_db(fleet).await;

        // Pre-populate a Healthy row for the rolloutId the host
        // would derive from the projected manifest.
        let original = Utc::now() - chrono::Duration::seconds(5);
        db.rollout_state()
            .transition_host_state(
                "test-host",
                &expected_id,
                crate::state::HostRolloutState::Healthy,
                crate::state::HealthyMarker::Set(original),
                None,
            )
            .unwrap();

        let attested = Utc::now() - chrono::Duration::hours(2);
        let req = checkin_req_with_attestation("test-host", "system-r1", Some(attested));

        recover_soak_state_from_attestation(&state, &req, Utc::now()).await;

        let map = db
            .rollout_state()
            .host_soak_state_for_rollout(&expected_id)
            .unwrap();
        let stamped = map.get("test-host").unwrap();
        assert_eq!(
            stamped.timestamp(),
            original.timestamp(),
            "existing row must not be overwritten by attestation",
        );
    }

    #[tokio::test]
    async fn b_cp_recovery_noop_for_legacy_agents_without_attestation() {
        // Legacy agent — no last_confirmed_at. CP behaviour is
        // unchanged: no soak-state writes happen.
        let fleet = fleet_with_host("test-host", Some("system-r1"));
        let (state, db) = state_with_fleet_and_db(fleet).await;
        let req = checkin_req_with_attestation("test-host", "system-r1", None);

        recover_soak_state_from_attestation(&state, &req, Utc::now()).await;
        assert!(db
            .rollout_state()
            .active_rollouts_snapshot()
            .unwrap()
            .is_empty());
    }
}
