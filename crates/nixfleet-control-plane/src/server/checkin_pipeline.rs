//! Checkin / confirm state machine: the `/v1/agent/checkin` and
//! `/v1/agent/confirm` handlers plus their orphan-confirm and
//! soak-state recovery helpers.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Utc};
use nixfleet_proto::agent_wire::{
    CheckinRequest, CheckinResponse, ConfirmRequest,
};

use crate::auth_cn::PeerCertificates;

use super::middleware::require_cn;
use super::state::{AppState, HostCheckinRecord, NEXT_CHECKIN_SECS};

/// `POST /v1/agent/checkin` — record an agent checkin.
///
/// Validates the body's `hostname` matches the verified mTLS CN
/// (sanity check, not a security boundary — the CN was already
/// authenticated by WebPkiClientVerifier; this just catches
/// configuration drift like a host using the wrong cert).
///
/// Emits a journal line per checkin so operators can grep
/// `journalctl -u nixfleet-control-plane | grep checkin`.
pub(super) async fn checkin(
    State(state): State<Arc<AppState>>,
    Extension(peer_certs): Extension<PeerCertificates>,
    Json(req): Json<CheckinRequest>,
) -> Result<Json<CheckinResponse>, StatusCode> {
    let cn = require_cn(&state, &peer_certs).await?;
    if cn != req.hostname {
        tracing::warn!(
            cert_cn = %cn,
            body_hostname = %req.hostname,
            "checkin rejected: cert CN does not match body hostname"
        );
        return Err(StatusCode::FORBIDDEN);
    }

    let last_fetch = req
        .last_fetch_outcome
        .as_ref()
        .map(|o| format!("{:?}", o.result).to_lowercase())
        .unwrap_or_else(|| "none".to_string());
    let pending = req
        .pending_generation
        .as_ref()
        .map(|p| p.closure_hash.as_str())
        .unwrap_or("null");
    tracing::info!(
        target: "checkin",
        hostname = %req.hostname,
        closure_hash = %req.current_generation.closure_hash,
        pending = %pending,
        last_fetch = %last_fetch,
        "checkin received"
    );

    let now = Utc::now();
    let record = HostCheckinRecord {
        last_checkin: now,
        checkin: req.clone(),
    };
    state
        .host_checkins
        .write()
        .await
        .insert(req.hostname.clone(), record);

    clear_left_healthy_for_checkin(&state, &req).await;
    recover_soak_state_from_attestation(&state, &req, now).await;

    let target = dispatch_target_for_checkin(&state, &req, now).await;

    Ok(Json(CheckinResponse {
        target,
        next_checkin_secs: NEXT_CHECKIN_SECS,
    }))
}

/// CP-rebuild recovery for an orphan confirm. Returns `true` when
/// the CP can absorb the confirm without forcing rollback, `false`
/// when it should fall through to 410. All failures are non-fatal:
/// the agent's local rollback still fires on 410.
async fn try_recover_orphan_confirm(
    state: &Arc<AppState>,
    req: &ConfirmRequest,
) -> bool {
    let Some(db) = state.db.as_ref() else {
        return false;
    };
    let Some(target_closure) = validate_orphan_recovery(state, req).await else {
        return false;
    };
    synthesise_orphan_confirm_rows(db, req, &target_closure)
}

/// Returns the validated target closure when the orphan confirm
/// matches the verified fleet's declared target for this host
/// (closure AND rollout id). None otherwise — caller falls through
/// to 410.
async fn validate_orphan_recovery(
    state: &AppState,
    req: &ConfirmRequest,
) -> Option<String> {
    let fleet = state.verified_fleet.read().await.clone().or_else(|| {
        tracing::debug!(
            hostname = %req.hostname,
            "orphan-confirm recovery: no verified fleet snapshot yet",
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
    // fleet's rollout id (a future schema where two rollouts target
    // the same closure could collapse them). KNOWN EDGE CASE: if CI
    // re-signs with a new ci_commit between dispatch and orphan
    // confirm AND the closure is unchanged, the agent's historical
    // rollout id won't match — we 410, agent rolls back a healthy
    // activation, next dispatch picks up the new id cleanly. Rare
    // and recoverable in one poll cycle.
    let expected_rollout_id = crate::dispatch::derive_rollout_id(
        &host_decl.channel,
        fleet.meta.ci_commit.as_deref(),
        target_closure,
    );
    if expected_rollout_id != req.rollout {
        tracing::info!(
            hostname = %req.hostname,
            agent_rollout = %req.rollout,
            expected_rollout = %expected_rollout_id,
            "orphan-confirm recovery: rollout id mismatch — agent is on a stale rollout, genuine 410",
        );
        return None;
    }

    Some(target_closure.clone())
}

/// Insert the synthetic `pending_confirms` (confirmed) + Healthy
/// marker. Returns true iff the pending_confirms write succeeded;
/// the host_healthy write is best-effort (worst case the soak timer
/// restarts on next confirm — same as pre-gap-B).
fn synthesise_orphan_confirm_rows(
    db: &crate::db::Db,
    req: &ConfirmRequest,
    target_closure: &str,
) -> bool {
    let now = Utc::now();
    if let Err(err) = db.record_confirmed_pending(
        &req.hostname,
        &req.rollout,
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
    if let Err(err) = db.transition_host_state(
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

/// Gap B-cp soak-state recovery from agent attestation.
///
/// After a CP rebuild, `host_rollout_state.last_healthy_since` is
/// gone for every host. Hosts that were mid-soak when the CP died
/// would otherwise restart their soak window from zero on the
/// next confirm, costing up to one full `soak_minutes` per
/// affected wave. The agent's `last_confirmed_at` attestation
/// ( wire-additive field) lets the CP repopulate
/// `last_healthy_since` from the agent-known timestamp — bringing
/// the soak gate's effective state back close to its pre-rebuild
/// position.
///
/// Triggers when ALL of:
/// 1. Agent reports `last_confirmed_at` (legacy agents leave it
///   None, no-op for them).
/// 2. CP has a verified `FleetResolved` snapshot.
/// 3. The host is declared in the fleet with a `closureHash`.
/// 4. The host's reported `current_generation.closure_hash` matches
///   the declared target — i.e. it's converged on the live target.
/// 5. No `host_rollout_state` row already exists for
///   (rollout, host). An existing row reflects the actual
///   lifecycle (Healthy/Soaked/Reverted) and is more authoritative
///   than a re-attestation.
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
async fn recover_soak_state_from_attestation(
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
    // `outstanding_compliance_events_by_rollout` lines up. Use the
    // shared `derive_rollout_id` helper — see its docstring for the
    // single-source-of-truth invariant across all three CP sites.
    let rollout_id = crate::dispatch::derive_rollout_id(
        &host_decl.channel,
        fleet.meta.ci_commit.as_deref(),
        target_closure,
    );

    match db.host_rollout_state_exists(&req.hostname, &rollout_id) {
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

    if let Err(err) = db.record_confirmed_pending(
        &req.hostname,
        &rollout_id,
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
    if let Err(err) = db.transition_host_state(
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

/// Per-checkin "left Healthy" sweep . Compares the
/// reported `current_generation.closure_hash` against each rollout
/// the host is currently recorded as Healthy in; on mismatch,
/// clears the Healthy marker so the soak timer restarts on the
/// next confirm. Best-effort: errors log + return without
/// affecting dispatch — the reconciler re-derives on its next
/// tick. Runs before `dispatch_target_for_checkin` so soak-state
/// hygiene is in place before any new target is issued.
async fn clear_left_healthy_for_checkin(state: &AppState, req: &CheckinRequest) {
    let Some(db) = state.db.as_ref() else {
        return;
    };
    let healthy = match db.healthy_rollouts_for_host(&req.hostname) {
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
        match db.clear_healthy_marker(&req.hostname, &rollout_id) {
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

/// Per-checkin dispatch decision. Failures log + return None: a
/// transient DB hiccup or missing fleet snapshot must not surface as
/// HTTP 500 to the agent (it retries every 60s).
async fn dispatch_target_for_checkin(
    state: &AppState,
    req: &CheckinRequest,
    now: DateTime<Utc>,
) -> Option<nixfleet_proto::agent_wire::EvaluatedTarget> {
    let db = state.db.as_ref()?;
    let fleet = state.verified_fleet.read().await.clone()?;
    let pending_for_host = match db.pending_confirm_exists(&req.hostname) {
        Ok(b) => b,
        Err(err) => {
            tracing::error!(
                hostname = %req.hostname,
                error = %err,
                "dispatch: pending_confirm_exists query failed",
            );
            return None;
        }
    };

    if wave_gate_blocks_dispatch(state, req, &fleet).await {
        return None;
    }

    let decision = crate::dispatch::decide_target(
        &req.hostname,
        req,
        &fleet,
        pending_for_host,
        now,
        state.confirm_deadline_secs as u32,
    );
    match decision {
        crate::dispatch::Decision::Dispatch {
            target,
            rollout_id,
            wave_index,
        } => record_dispatched_target(db, &req.hostname, &rollout_id, wave_index, target, state, now),
        other => {
            tracing::debug!(
                target: "dispatch",
                hostname = %req.hostname,
                decision = ?other,
                "dispatch: no target",
            );
            None
        }
    }
}

/// Per-channel wave-staging compliance gate. Returns true iff dispatch
/// must be blocked (enforce mode + outstanding signature-verified
/// failures on an earlier wave). Permissive mode logs an advisory.
async fn wave_gate_blocks_dispatch(
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
    let resolved_mode = nixfleet_proto::compliance::GateMode::from_wire_str(&channel.compliance.mode);

    let staged = stage_channel_hosts(state, fleet, channel_name).await;
    let requesting_wave = wave_index_for(fleet, channel_name, &req.hostname);

    let outcome = crate::wave_gate::evaluate_channel_gate(
        resolved_mode,
        requesting_wave,
        staged.iter().map(|(n, recs, rollout, wave_idx)| {
            crate::wave_gate::HostGateInput {
                hostname: n.as_str(),
                records: recs.as_slice(),
                current_rollout: rollout.as_deref(),
                wave_index: *wave_idx,
            }
        }),
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

/// Snapshot per-host (records, current rollout, wave index) for every
/// host on the given channel. Owned data so the gate iterator has
/// stable lifetimes after the locks drop.
async fn stage_channel_hosts(
    state: &AppState,
    fleet: &nixfleet_proto::FleetResolved,
    channel_name: &str,
) -> Vec<(String, Vec<crate::server::ReportRecord>, Option<String>, Option<u32>)> {
    let reports_guard = state.host_reports.read().await;
    let checkins_guard = state.host_checkins.read().await;
    fleet
        .hosts
        .iter()
        .filter(|(_, h)| h.channel == channel_name)
        .map(|(n, _)| {
            let buf: Vec<crate::server::ReportRecord> = reports_guard
                .get(n)
                .map(|q| q.iter().cloned().collect())
                .unwrap_or_default();
            let cur_rollout = checkins_guard
                .get(n)
                .and_then(|c| c.checkin.last_evaluated_target.as_ref())
                .and_then(|t| t.rollout_id.clone());
            let wave_idx = wave_index_for(fleet, channel_name, n);
            (n.clone(), buf, cur_rollout, wave_idx)
        })
        .collect()
}

fn wave_index_for(
    fleet: &nixfleet_proto::FleetResolved,
    channel_name: &str,
    hostname: &str,
) -> Option<u32> {
    fleet.waves.get(channel_name).and_then(|waves| {
        waves
            .iter()
            .position(|w| w.hosts.iter().any(|h| h == hostname))
            .map(|i| i as u32)
    })
}

/// Persist the `pending_confirms` row for a freshly-dispatched
/// target. Returns the target on success, None if the DB write fails
/// (the row is the idempotency anchor — without it the next checkin
/// would re-dispatch, breaking the contract).
fn record_dispatched_target(
    db: &crate::db::Db,
    hostname: &str,
    rollout_id: &str,
    wave_index: Option<u32>,
    target: nixfleet_proto::agent_wire::EvaluatedTarget,
    state: &AppState,
    now: DateTime<Utc>,
) -> Option<nixfleet_proto::agent_wire::EvaluatedTarget> {
    let confirm_deadline = now + chrono::Duration::seconds(state.confirm_deadline_secs);
    match db.record_pending_confirm(&crate::db::PendingConfirmInsert {
        hostname,
        rollout_id,
        wave: wave_index.unwrap_or(0),
        target_closure_hash: &target.closure_hash,
        target_channel_ref: &target.channel_ref,
        confirm_deadline,
    }) {
        Ok(_) => {
            tracing::info!(
                target: "dispatch",
                hostname = %hostname,
                rollout = %rollout_id,
                target_closure = %target.closure_hash,
                confirm_deadline = %confirm_deadline.to_rfc3339(),
                "dispatch: target issued",
            );
            Some(target)
        }
        Err(err) => {
            tracing::warn!(
                hostname = %hostname,
                rollout = %rollout_id,
                error = %err,
                "dispatch: record_pending_confirm failed; returning no target",
            );
            None
        }
    }
}

/// `POST /v1/agent/confirm` — agent confirms successful activation.
/// Marks the matching `pending_confirms` row as confirmed.
///
/// Behaviour:
/// - Pending row exists, deadline not passed → mark confirmed, 204.
/// - No matching row in 'pending' state → orphan-recovery path:
///   if the agent's reported `closure_hash` matches the host's
///   declared target in the verified `FleetResolved`, treat as a
///   CP-rebuild recovery — synthesise a confirmed pending_confirms
///   row + transition to Healthy + 204. Closure-hash mismatch →
///   genuine 410 (rollout cancelled / wrong rollout / deadline
///   expired; agent triggers local rollback per RFC §4.2).
/// - DB unset → 503 (endpoint requires persistence).
pub(super) async fn confirm(
    State(state): State<Arc<AppState>>,
    Extension(peer_certs): Extension<PeerCertificates>,
    Json(req): Json<ConfirmRequest>,
) -> Result<Response, StatusCode> {
    let cn = require_cn(&state, &peer_certs).await?;
    if cn != req.hostname {
        tracing::warn!(
            cert_cn = %cn,
            body_hostname = %req.hostname,
            "confirm rejected: cert CN does not match body hostname"
        );
        return Err(StatusCode::FORBIDDEN);
    }

    let db = state.db.as_ref().ok_or_else(|| {
        tracing::warn!("confirm: no db configured — endpoint unusable");
        StatusCode::SERVICE_UNAVAILABLE
    })?;

    let updated = db.confirm_pending(&req.hostname, &req.rollout).map_err(|err| {
        tracing::error!(error = %err, "confirm: db update failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if updated == 0 {
        // Try the orphan-confirm recovery path before
        // declaring 410. Recovery succeeds only when the agent's
        // reported closure_hash matches the host's verified
        // target — that's the same authorisation invariant as
        // the positive flow (mTLS-CN + closure on file).
        if try_recover_orphan_confirm(&state, &req).await {
            // Fall through to the success log + 204 path.
        } else {
            tracing::info!(
                hostname = %req.hostname,
                rollout = %req.rollout,
                "confirm: no matching pending row + no recoverable orphan — returning 410"
            );
            return Ok(Response::builder()
                .status(StatusCode::GONE)
                .body(Body::from(""))
                .expect("Response::builder with valid status + body is infallible"));
        }
    } else {
        // Standard path: stamp last_healthy_since (
        // ConfirmWindow → Healthy). The orphan-recovery branch
        // already wrote it inline so we don't double-up here.
        if let Err(err) = db.transition_host_state(
            &req.hostname,
            &req.rollout,
            crate::state::HostRolloutState::Healthy,
            crate::state::HealthyMarker::Set(Utc::now()),
            None,
        ) {
            tracing::warn!(
                hostname = %req.hostname,
                rollout = %req.rollout,
                error = %err,
                "confirm: transition to Healthy failed; soak timer will skip this host",
            );
        }
    }

    tracing::info!(
        target: "confirm",
        hostname = %req.hostname,
        rollout = %req.rollout,
        wave = req.wave,
        closure_hash = %req.generation.closure_hash,
        "confirm received"
    );
    Ok(Response::builder()
        .status(StatusCode::NO_CONTENT)
        .body(Body::from(""))
        .expect("Response::builder with valid status + body is infallible"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use nixfleet_proto::agent_wire::{ConfirmRequest, GenerationRef};
    use nixfleet_proto::fleet_resolved::Meta;
    use nixfleet_proto::{Channel, Compliance, Host};
    use std::collections::HashMap;
    use std::sync::Arc;

    fn fleet_with_host(hostname: &str, closure: Option<&str>) -> nixfleet_proto::FleetResolved {
        let mut hosts = HashMap::new();
        hosts.insert(
            hostname.to_string(),
            Host {
                system: "x86_64-linux".to_string(),
                tags: vec![],
                channel: "stable".to_string(),
                closure_hash: closure.map(String::from),
                pubkey: None,
            },
        );
        let mut channels = HashMap::new();
        channels.insert(
            "stable".to_string(),
            Channel {
                rollout_policy: "default".to_string(),
                reconcile_interval_minutes: 5,
                freshness_window: 60,
                signing_interval_minutes: 30,
                compliance: Compliance {
                    frameworks: vec![],
                    mode: "disabled".to_string(),
                },
            },
        );
        nixfleet_proto::FleetResolved {
            schema_version: 1,
            hosts,
            channels,
            rollout_policies: HashMap::new(),
            waves: HashMap::new(),
            edges: vec![],
            disruption_budgets: vec![],
            meta: Meta {
                schema_version: 1,
                signed_at: None,
                ci_commit: Some("abc12345".to_string()),
                signature_algorithm: None,
            },
        }
    }

    fn checkin_req_with_attestation(
        hostname: &str,
        closure: &str,
        attested: Option<DateTime<Utc>>,
    ) -> nixfleet_proto::agent_wire::CheckinRequest {
        nixfleet_proto::agent_wire::CheckinRequest {
            hostname: hostname.to_string(),
            agent_version: "test".into(),
            current_generation: GenerationRef {
                closure_hash: closure.to_string(),
                channel_ref: None,
                boot_id: "boot".to_string(),
            },
            pending_generation: None,
            last_evaluated_target: None,
            last_fetch_outcome: None,
            uptime_secs: None,
            last_confirmed_at: attested,
        }
    }

    fn confirm_req(hostname: &str, rollout: &str, closure: &str) -> ConfirmRequest {
        ConfirmRequest {
            hostname: hostname.to_string(),
            rollout: rollout.to_string(),
            wave: 0,
            generation: GenerationRef {
                closure_hash: closure.to_string(),
                channel_ref: None,
                boot_id: "boot".to_string(),
            },
        }
    }

    async fn state_with_fleet_and_db(
        fleet: nixfleet_proto::FleetResolved,
    ) -> (Arc<AppState>, Arc<Db>) {
        let db = Arc::new(Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let state = Arc::new(AppState {
            db: Some(Arc::clone(&db)),
            verified_fleet: Arc::new(tokio::sync::RwLock::new(Some(Arc::new(fleet)))),
            ..AppState::default()
        });
        (state, db)
    }

    #[tokio::test]
    async fn orphan_recovery_succeeds_when_closure_matches() {
        // Gap A happy path. CP rebuilt mid-flight; agent posts a
        // confirm whose closure matches the verified target. The
        // recovery path synthesises a confirmed row + Healthy
        // marker and returns true so the handler emits 204 instead
        // of forcing a local rollback.
        let fleet = fleet_with_host("test-host", Some("target-system-r1"));
        let (state, db) = state_with_fleet_and_db(fleet).await;
        let req = confirm_req("test-host", "stable@abc12345", "target-system-r1");

        assert!(
            try_recover_orphan_confirm(&state, &req).await,
            "matching closure should recover",
        );

        let snap = db.active_rollouts_snapshot().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].rollout_id, "stable@abc12345");
        assert_eq!(snap[0].target_closure_hash, "target-system-r1");
        // Healthy marker stamped in the same call.
        let healthy = db.healthy_rollouts_for_host("test-host").unwrap();
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
        assert!(db.active_rollouts_snapshot().unwrap().is_empty());
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
        // Gap B-cp happy path. Host is converged on the verified
        // target, no host_rollout_state row exists (CP rebuilt),
        // attestation arrives → stamp last_healthy_since.
        let fleet = fleet_with_host("test-host", Some("system-r1"));
        let (state, db) = state_with_fleet_and_db(fleet).await;
        let attested = Utc::now() - chrono::Duration::minutes(3);
        let req = checkin_req_with_attestation("test-host", "system-r1", Some(attested));

        recover_soak_state_from_attestation(&state, &req, Utc::now()).await;

        let snap = db.active_rollouts_snapshot().unwrap();
        assert_eq!(snap.len(), 1, "snapshot should contain the recovered rollout");
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

        let snap = db.active_rollouts_snapshot().unwrap();
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
        assert!(db.active_rollouts_snapshot().unwrap().is_empty());
    }

    #[tokio::test]
    async fn b_cp_recovery_skips_when_host_state_already_exists() {
        // host_rollout_state already has a row (e.g. from a normal
        // confirm or from 's orphan recovery). Re-attestation
        // must NOT overwrite — the existing row is more
        // authoritative.
        let fleet = fleet_with_host("test-host", Some("system-r1"));
        let (state, db) = state_with_fleet_and_db(fleet).await;

        // Pre-populate a Healthy row for the rollout the host
        // would derive (channel "stable", short "abc12345" from the
        // fleet's ci_commit).
        let original = Utc::now() - chrono::Duration::seconds(5);
        db.transition_host_state(
            "test-host",
            "stable@abc12345",
            crate::state::HostRolloutState::Healthy,
            crate::state::HealthyMarker::Set(original),
            None,
        )
        .unwrap();

        let attested = Utc::now() - chrono::Duration::hours(2);
        let req = checkin_req_with_attestation("test-host", "system-r1", Some(attested));

        recover_soak_state_from_attestation(&state, &req, Utc::now()).await;

        // last_healthy_since stays at `original` (5s ago), NOT at
        // `attested` (2h ago) — recovery saw the existing row and
        // skipped.
        let map = db.host_soak_state_for_rollout("stable@abc12345").unwrap();
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
        assert!(db.active_rollouts_snapshot().unwrap().is_empty());
    }
}
