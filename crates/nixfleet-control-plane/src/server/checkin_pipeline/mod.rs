//! `/v1/agent/checkin` and `/v1/agent/confirm` handlers.
//!
//! Submodule layout:
//!
//! - [`recovery`] — orphan-confirm and soak-state recovery paths
//!   (CP-rebuild robustness).
//! - [`rollback_signal`] — RFC-0002 §5.1 rollback-and-halt signal
//!   emission + the per-checkin "left Healthy" sweep.
//! - [`dispatch_target`] — dispatch decision + persist the
//!   `host_dispatch_state` operational + `dispatch_history` audit
//!   rows that anchor idempotency.
//! - [`wave_gate`] — checkin-side caller around the pure
//!   `evaluate_channel_gate` evaluator (top-level `crate::wave_gate`).

mod dispatch_target;
mod recovery;
mod rollback_signal;
mod wave_gate;

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::Json;
use chrono::Utc;
use nixfleet_proto::agent_wire::{CheckinRequest, CheckinResponse, ConfirmRequest};

use super::middleware::AuthenticatedCn;
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
    Extension(cn): Extension<AuthenticatedCn>,
    Json(req): Json<CheckinRequest>,
) -> Result<Json<CheckinResponse>, StatusCode> {
    let cn = cn.into_string();
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
    // info-level: harness scenarios (teardown, rollback-policy,
    // boot-recovery) gate on this line as the canonical "agent
    // checked in" signal. Volume concern (3000 lines/hr at 50
    // hosts × 60s poll) is real but breaks observability if
    // demoted; revisit with a periodic-summary or first-checkin-
    // after-restart approach if the spam becomes operationally
    // painful.
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

    rollback_signal::clear_left_healthy_for_checkin(&state, &req).await;
    recovery::recover_soak_state_from_attestation(&state, &req, now).await;
    // Catches the deadline-fired-before-confirm-arrived race: agent
    // is on the target but the row got marked rolled-back. Revives
    // to confirmed; lab/2026-05-02 split-brain class.
    let _ = recovery::try_recover_pending_from_checkin(&state, &req).await;

    let target = dispatch_target::dispatch_target_for_checkin(&state, &req, now).await;
    let rollback = rollback_signal::rollback_signal_for_checkin(&state, &req).await;

    Ok(Json(CheckinResponse {
        target,
        rollback,
        next_checkin_secs: NEXT_CHECKIN_SECS,
    }))
}

/// `POST /v1/agent/confirm` — agent confirms successful activation.
/// Flips the matching `host_dispatch_state` row from 'pending' to
/// 'confirmed'.
///
/// Behaviour:
/// - Pending row exists, deadline not passed → flip confirmed, 204.
/// - No matching row in 'pending' state → orphan-recovery path:
///   if the agent's reported `closure_hash` matches the host's
///   declared target in the verified `FleetResolved`, treat as a
///   CP-rebuild recovery — synthesise a confirmed operational row
///   + transition to Healthy + 204. Closure-hash mismatch → genuine
///   410 (rollout cancelled / wrong rollout / deadline expired;
///   agent triggers local rollback per RFC §4.2).
/// - DB unset → 503 (endpoint requires persistence).
pub(super) async fn confirm(
    State(state): State<Arc<AppState>>,
    Extension(cn): Extension<AuthenticatedCn>,
    Json(req): Json<ConfirmRequest>,
) -> Result<Response, StatusCode> {
    let cn = cn.into_string();
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

    let updated = db
        .host_dispatch_state()
        .confirm(&req.hostname, &req.rollout)
        .map_err(|err| {
            tracing::error!(error = %err, "confirm: db update failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if updated == 0 {
        // Try the orphan-confirm recovery path before declaring 410.
        // Recovery succeeds only when the agent's reported
        // closure_hash matches the host's verified target — that's
        // the same authorisation invariant as the positive flow
        // (mTLS-CN + closure on file).
        if recovery::try_recover_orphan_confirm(&state, &req).await {
            // Fall through to the success log + 204 path.
        } else {
            // Stamp the operational row terminal-rolled-back inline
            // before returning 410. The rollback_timer (30s tick)
            // would do the same on its next pass, but doing it here
            // means the agent's local-rollback decision (driven by
            // the 410) and the CP's view of the host converge in a
            // single round-trip rather than racing the timer.
            //
            // Best-effort: a write failure logs and falls through to
            // 410 anyway — the timer will catch up.
            if let Err(err) = db.host_dispatch_state().mark_rolled_back(&[(
                req.hostname.clone(),
                req.rollout.clone(),
            )]) {
                tracing::warn!(
                    hostname = %req.hostname,
                    rollout = %req.rollout,
                    error = %err,
                    "confirm-410: inline mark_rolled_back failed; rollback_timer will retry",
                );
            }
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
        // Standard path: stamp last_healthy_since (ConfirmWindow →
        // Healthy). The orphan-recovery branch already wrote it
        // inline so we don't double-up here.
        if let Err(err) = db.rollout_state().transition_host_state(
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
pub(super) mod tests {
    //! Cross-submodule test fixtures. Each submodule's `tests`
    //! reaches up into here for fleet/state/request constructors so
    //! the recovery + rollback-signal scenarios share one definition.

    use crate::db::Db;
    use chrono::{DateTime, Utc};
    use nixfleet_proto::agent_wire::{ConfirmRequest, GenerationRef};
    use nixfleet_proto::fleet_resolved::Meta;
    use nixfleet_proto::{Channel, Compliance, Host};
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::AppState;

    pub(super) fn fleet_with_host(
        hostname: &str,
        closure: Option<&str>,
    ) -> nixfleet_proto::FleetResolved {
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
        let mut rollout_policies = HashMap::new();
        rollout_policies.insert(
            "default".to_string(),
            nixfleet_proto::RolloutPolicy {
                strategy: "waves".to_string(),
                waves: vec![],
                health_gate: nixfleet_proto::HealthGate::default(),
                on_health_failure: nixfleet_proto::OnHealthFailure::Halt,
            },
        );
        nixfleet_proto::FleetResolved {
            schema_version: 1,
            hosts,
            channels,
            rollout_policies,
            waves: HashMap::new(),
            edges: vec![],
            disruption_budgets: vec![],
            meta: Meta {
                schema_version: 1,
                signed_at: Some(
                    DateTime::parse_from_rfc3339("2026-04-30T00:00:00Z")
                        .unwrap()
                        .with_timezone(&Utc),
                ),
                ci_commit: Some("abc12345".to_string()),
                signature_algorithm: Some("ed25519".to_string()),
            },
        }
    }

    /// Stable test fleet hash. Tests pass this as the
    /// `fleet_resolved_hash` so the projection result is deterministic
    /// even though the real one is computed from canonical bytes.
    pub(super) const TEST_FLEET_HASH: &str =
        "0000000000000000000000000000000000000000000000000000000000000000";

    pub(super) fn expected_rollout_id_for(
        fleet: &nixfleet_proto::FleetResolved,
        channel: &str,
    ) -> String {
        nixfleet_reconciler::compute_rollout_id_for_channel(fleet, TEST_FLEET_HASH, channel)
            .expect("projection succeeds")
            .expect("non-empty channel")
    }

    pub(super) fn checkin_req_with_attestation(
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

    pub(super) fn confirm_req(hostname: &str, rollout: &str, closure: &str) -> ConfirmRequest {
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

    pub(super) async fn state_with_fleet_and_db(
        fleet: nixfleet_proto::FleetResolved,
    ) -> (Arc<AppState>, Arc<Db>) {
        let db = Arc::new(Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let state = Arc::new(AppState {
            db: Some(Arc::clone(&db)),
            verified_fleet: Arc::new(tokio::sync::RwLock::new(Some(
                crate::server::VerifiedFleetSnapshot {
                    fleet: Arc::new(fleet),
                    fleet_resolved_hash: TEST_FLEET_HASH.to_string(),
                },
            ))),
            ..AppState::default()
        });
        (state, db)
    }
}
