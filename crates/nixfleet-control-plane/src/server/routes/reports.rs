//! `/v1/agent/report` handler plus its signature-verification and
//! event-id helpers.

use std::sync::Arc;

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::Utc;
use nixfleet_proto::agent_wire::{ReportRequest, ReportResponse};

use crate::auth::auth_cn::PeerCertificates;

use super::super::middleware::require_cn;
use super::super::state::{AppState, ReportRecord, REPORT_RING_CAP};

/// `POST /v1/agent/report` — record an out-of-band event report.
///
/// In-memory ring buffer per host, capped at `REPORT_RING_CAP`.
/// New reports push to the back; oldest is dropped on overflow.
/// Future work: promote to SQLite + correlate with rollouts.
pub(in crate::server) async fn report(
    State(state): State<Arc<AppState>>,
    Extension(peer_certs): Extension<PeerCertificates>,
    Json(req): Json<ReportRequest>,
) -> Result<Json<ReportResponse>, StatusCode> {
    let cn = require_cn(&state, &peer_certs).await?;
    if cn != req.hostname {
        tracing::warn!(
            cert_cn = %cn,
            body_hostname = %req.hostname,
            "report rejected: cert CN does not match body hostname"
        );
        return Err(StatusCode::FORBIDDEN);
    }

    let event_id = format!("evt-{}-{}", Utc::now().timestamp_millis(), rand_suffix(8));
    let received_at = Utc::now();

    // Render the event variant for the journal in a grep-friendly
    // way: `event=activation-failed`, `event=verify-mismatch`, etc.
    // The serde_json round-trip extracts the kebab-case discriminator.
    let event_str = serde_json::to_value(&req.event)
        .ok()
        .and_then(|v| v.get("event").and_then(|e| e.as_str()).map(String::from))
        .unwrap_or_else(|| "<unknown>".to_string());
    let rollout_str = req.rollout.clone().unwrap_or_else(|| "<none>".to_string());

    // root-3 — verify probe-output signatures on the
    // two event variants that carry them. Non-signed events surface
    // as `None`; the wave-staging gate consults `signature_status`
    // when honouring outstanding events. Verification is best-
    // effort: we always store the record (mTLS already authenticated
    // the post), the signature verdict shapes downstream gating.
    let signature_status = compute_signature_status(&state, &req).await;

    tracing::info!(
        target: "report",
        hostname = %req.hostname,
        event = %event_str,
        rollout = %rollout_str,
        agent_version = %req.agent_version,
        event_id = %event_id,
        signature_status = ?signature_status,
        "report received"
    );

    let record = ReportRecord {
        event_id: event_id.clone(),
        received_at,
        report: req.clone(),
        signature_status,
    };

    // — write through to SQLite alongside the in-memory
    // ring buffer. Ring stays for hot-path latency in dispatch
    // decisions; SQLite is the durable record that survives CP
    // restart. DB write is best-effort: a failure logs warn + lets
    // the in-memory write proceed (degraded == old in-memory-only
    // behaviour, so no regression).
    if let Some(db) = state.db.as_ref() {
        let signature_status_str = signature_status.as_ref().and_then(|s| {
            serde_json::to_value(s)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
        });
        // Best-effort SQLite persistence. Two failure modes, both
        // handled the same way: log + skip the DB write, let the
        // in-memory ring buffer below absorb the event regardless.
        // The serde failure path is what matters here — previously
        // `unwrap_or_default ` would write `""` into report_json,
        // and on next CP startup the hydration parse would fail and
        // skip the row, leaving a phantom DB row that could never
        // be replayed. Now we never write the row at all on serde
        // failure.
        match serde_json::to_string(&req) {
            Ok(report_json) => {
                if let Err(err) = db
                    .reports()
                    .record_host_report(&crate::db::HostReportInsert {
                        hostname: &req.hostname,
                        event_id: &event_id,
                        received_at,
                        event_kind: &event_str,
                        rollout: req.rollout.as_deref(),
                        signature_status: signature_status_str.as_deref(),
                        report_json: &report_json,
                    })
                {
                    tracing::warn!(
                        target: "report",
                        hostname = %req.hostname,
                        event_id = %event_id,
                        error = %err,
                        "report SQLite write failed; in-memory ring buffer still updated",
                    );
                }
            }
            Err(err) => {
                tracing::warn!(
                    target: "report",
                    hostname = %req.hostname,
                    event_id = %event_id,
                    error = %err,
                    "report serialisation to JSON failed; skipping SQLite persistence (in-memory ring still updated)",
                );
            }
        }
    }

    // — close the rollback-and-halt loop. After the agent
    // posts `RollbackTriggered`, advance host_rollout_state from
    // `Failed` to `Reverted` so `compute_rollback_signal` stops
    // emitting the signal on every checkin (otherwise the agent's
    // idempotent `rollback()` keeps churning fresh
    // `RollbackTriggered` posts forever). The wire docstring on
    // `RollbackSignal` already promises this behaviour; this arm
    // makes the promise real. Best-effort: a transition error must
    // NOT fail the report POST — the report endpoint is a
    // write-only sink for evidence, and a stale state row gets
    // re-detected on the next reconcile tick.
    if let Some(db) = state.db.as_ref() {
        apply_rollback_state_transition(db, &req);
    }

    let mut reports = state.host_reports.write().await;
    let buf = reports.entry(req.hostname).or_default();
    if buf.len() >= REPORT_RING_CAP {
        buf.pop_front();
    }
    buf.push_back(record);

    Ok(Json(ReportResponse { event_id }))
}

/// Flip `host_rollout_state.host_state` from `Failed` to `Reverted`
/// when a `RollbackTriggered` event arrives. No-op for any other
/// event variant or when the report carries no `rollout` id (the
/// CP-410 cancellation path can post `RollbackTriggered` without
/// a rollout context). Guarded with `expected_from = Failed` so
/// the other emitters of `RollbackTriggered` (`handle_cp_cancellation`
/// in CP-410, `handle_switch_failed` for agent self-detected
/// activation failure) leave non-Failed rows untouched —
/// `transition_host_state` returns `Ok(0)` when the WHERE clause
/// doesn't match.
fn apply_rollback_state_transition(db: &crate::db::Db, req: &ReportRequest) {
    use nixfleet_proto::agent_wire::ReportEvent;
    if !matches!(req.event, ReportEvent::RollbackTriggered { .. }) {
        return;
    }
    let Some(rollout) = req.rollout.as_deref() else {
        return;
    };
    match db.rollout_state().transition_host_state(
        &req.hostname,
        rollout,
        crate::state::HostRolloutState::Reverted,
        crate::state::HealthyMarker::Untouched,
        Some(crate::state::HostRolloutState::Failed),
    ) {
        Ok(0) => {
            // Row not in Failed (or absent). Expected for the CP-410
            // cancellation and agent self-detected activation-failure
            // paths; the guard intentionally leaves them alone.
            tracing::debug!(
                target: "report",
                hostname = %req.hostname,
                rollout = %rollout,
                "RollbackTriggered: no Failed row to transition (guard preserved non-Failed state)",
            );
        }
        Ok(_) => {
            tracing::info!(
                target: "report",
                hostname = %req.hostname,
                rollout = %rollout,
                "RollbackTriggered: host_rollout_state Failed → Reverted",
            );
            // Terminal stamp for the rollback-and-halt path. The
            // host has been rolled back; flip operational state to
            // `rolled-back` (so it stops surfacing in
            // `active_rollouts_snapshot`) and stamp the audit row's
            // terminal_state. Both are best-effort: an error is
            // logged but doesn't fail the report POST.
            //
            // record_terminal is race-resistant via WHERE rollout_id
            // — a newer dispatch on a different rollout_id is left
            // alone.
            let now = Utc::now();
            if let Err(err) = db.host_dispatch_state().record_terminal(
                &req.hostname,
                rollout,
                crate::state::TerminalState::RolledBack,
            ) {
                tracing::warn!(
                    target: "report",
                    hostname = %req.hostname,
                    rollout = %rollout,
                    error = %err,
                    "RollbackTriggered: operational terminal stamp failed",
                );
            }
            if let Err(err) = db.dispatch_history().mark_terminal_for_rollout_host(
                rollout,
                &req.hostname,
                crate::state::TerminalState::RolledBack,
                now,
            ) {
                tracing::warn!(
                    target: "report",
                    hostname = %req.hostname,
                    rollout = %rollout,
                    error = %err,
                    "RollbackTriggered: audit terminal stamp failed",
                );
            }
        }
        Err(err) => {
            // Best-effort: a transition error does not fail the
            // report POST. The reconciler will re-detect on the
            // next tick.
            tracing::warn!(
                target: "report",
                hostname = %req.hostname,
                rollout = %rollout,
                error = %err,
                "RollbackTriggered: Failed → Reverted transition failed; report still persisted",
            );
        }
    }
}

/// Compute the signature verdict for an incoming report (
/// root-3). Only `ComplianceFailure` and `RuntimeGateError`
/// carry probe-output signatures today; all other variants return
/// `None`. The host's pubkey comes from `verified_fleet`'s
/// `hosts.<hostname>.pubkey`; absent pubkey → `NoPubkey`.
async fn compute_signature_status(
    state: &Arc<AppState>,
    req: &ReportRequest,
) -> Option<nixfleet_reconciler::evidence::SignatureStatus> {
    use nixfleet_proto::agent_wire::ReportEvent;
    use nixfleet_proto::evidence_signing::{
        ActivationFailedSignedPayload, ClosureSignatureMismatchSignedPayload,
        ComplianceFailureSignedPayload, ManifestMismatchSignedPayload,
        ManifestMissingSignedPayload, ManifestVerifyFailedSignedPayload,
        RealiseFailedSignedPayload, RollbackTriggeredSignedPayload, RuntimeGateErrorSignedPayload,
        StaleTargetSignedPayload, VerifyMismatchSignedPayload,
    };
    use nixfleet_reconciler::evidence::verify_event;

    fn sha256_jcs_str(s: &str) -> String {
        match serde_jcs::to_vec(s) {
            Ok(bytes) => {
                use sha2::Digest;
                let d = sha2::Sha256::digest(&bytes);
                let mut out = String::with_capacity(64);
                for b in d.iter() {
                    out.push_str(&format!("{:02x}", b));
                }
                out
            }
            Err(_) => String::new(),
        }
    }

    let pubkey: Option<String> = {
        let fleet_guard = state.verified_fleet.read().await;
        fleet_guard
            .as_ref()
            .and_then(|snap| snap.fleet.hosts.get(&req.hostname))
            .and_then(|h| h.pubkey.clone())
    };

    match &req.event {
        ReportEvent::ComplianceFailure {
            control_id,
            status,
            framework_articles,
            evidence_snippet,
            evidence_collected_at,
            signature,
        } => {
            // Re-derive the snippet hash the agent included in its
            // signed payload (sha256 of JCS-canonical snippet bytes;
            // empty when snippet is None).
            let snippet_sha = match evidence_snippet {
                Some(v) => match serde_jcs::to_vec(v) {
                    Ok(bytes) => {
                        use sha2::Digest;
                        let d = sha2::Sha256::digest(&bytes);
                        let mut s = String::with_capacity(64);
                        for b in d.iter() {
                            s.push_str(&format!("{:02x}", b));
                        }
                        s
                    }
                    Err(_) => String::new(),
                },
                None => String::new(),
            };
            let payload = ComplianceFailureSignedPayload {
                hostname: &req.hostname,
                rollout: req.rollout.as_deref(),
                control_id,
                status,
                framework_articles,
                evidence_collected_at: *evidence_collected_at,
                evidence_snippet_sha256: snippet_sha,
            };
            Some(verify_event(
                signature.as_deref(),
                pubkey.as_deref(),
                &payload,
            ))
        }
        ReportEvent::RuntimeGateError {
            reason,
            collector_exit_code,
            evidence_collected_at,
            activation_completed_at,
            signature,
        } => {
            let payload = RuntimeGateErrorSignedPayload {
                hostname: &req.hostname,
                rollout: req.rollout.as_deref(),
                reason,
                collector_exit_code: *collector_exit_code,
                evidence_collected_at: *evidence_collected_at,
                activation_completed_at: *activation_completed_at,
            };
            Some(verify_event(
                signature.as_deref(),
                pubkey.as_deref(),
                &payload,
            ))
        }
        ReportEvent::ActivationFailed {
            phase,
            exit_code,
            stderr_tail,
            signature,
        } => {
            let stderr_tail_sha256 = stderr_tail
                .as_deref()
                .map(sha256_jcs_str)
                .unwrap_or_else(|| sha256_jcs_str(""));
            let payload = ActivationFailedSignedPayload {
                hostname: &req.hostname,
                rollout: req.rollout.as_deref(),
                phase,
                exit_code: *exit_code,
                stderr_tail_sha256,
            };
            Some(verify_event(
                signature.as_deref(),
                pubkey.as_deref(),
                &payload,
            ))
        }
        ReportEvent::RealiseFailed {
            closure_hash,
            reason,
            signature,
        } => {
            let payload = RealiseFailedSignedPayload {
                hostname: &req.hostname,
                rollout: req.rollout.as_deref(),
                closure_hash,
                reason,
            };
            Some(verify_event(
                signature.as_deref(),
                pubkey.as_deref(),
                &payload,
            ))
        }
        ReportEvent::VerifyMismatch {
            expected,
            actual,
            signature,
        } => {
            let payload = VerifyMismatchSignedPayload {
                hostname: &req.hostname,
                rollout: req.rollout.as_deref(),
                expected,
                actual,
            };
            Some(verify_event(
                signature.as_deref(),
                pubkey.as_deref(),
                &payload,
            ))
        }
        ReportEvent::RollbackTriggered { reason, signature } => {
            let payload = RollbackTriggeredSignedPayload {
                hostname: &req.hostname,
                rollout: req.rollout.as_deref(),
                reason,
            };
            Some(verify_event(
                signature.as_deref(),
                pubkey.as_deref(),
                &payload,
            ))
        }
        ReportEvent::ClosureSignatureMismatch {
            closure_hash,
            stderr_tail,
            signature,
        } => {
            let stderr_tail_sha256 = sha256_jcs_str(stderr_tail);
            let payload = ClosureSignatureMismatchSignedPayload {
                hostname: &req.hostname,
                rollout: req.rollout.as_deref(),
                closure_hash,
                stderr_tail_sha256,
            };
            Some(verify_event(
                signature.as_deref(),
                pubkey.as_deref(),
                &payload,
            ))
        }
        ReportEvent::StaleTarget {
            closure_hash,
            channel_ref,
            signed_at,
            freshness_window_secs,
            age_secs,
            signature,
        } => {
            let payload = StaleTargetSignedPayload {
                hostname: &req.hostname,
                rollout: req.rollout.as_deref(),
                closure_hash,
                channel_ref,
                signed_at: *signed_at,
                freshness_window_secs: *freshness_window_secs,
                age_secs: *age_secs,
            };
            Some(verify_event(
                signature.as_deref(),
                pubkey.as_deref(),
                &payload,
            ))
        }
        ReportEvent::ManifestMissing {
            rollout_id,
            reason,
            signature,
        } => {
            let payload = ManifestMissingSignedPayload {
                hostname: &req.hostname,
                rollout: req.rollout.as_deref(),
                rollout_id,
                reason,
            };
            Some(verify_event(
                signature.as_deref(),
                pubkey.as_deref(),
                &payload,
            ))
        }
        ReportEvent::ManifestVerifyFailed {
            rollout_id,
            reason,
            signature,
        } => {
            let payload = ManifestVerifyFailedSignedPayload {
                hostname: &req.hostname,
                rollout: req.rollout.as_deref(),
                rollout_id,
                reason,
            };
            Some(verify_event(
                signature.as_deref(),
                pubkey.as_deref(),
                &payload,
            ))
        }
        ReportEvent::ManifestMismatch {
            rollout_id,
            reason,
            signature,
        } => {
            let payload = ManifestMismatchSignedPayload {
                hostname: &req.hostname,
                rollout: req.rollout.as_deref(),
                rollout_id,
                reason,
            };
            Some(verify_event(
                signature.as_deref(),
                pubkey.as_deref(),
                &payload,
            ))
        }

        // Variants that intentionally carry no signature. Each line
        // is a deliberate decision documented in RFC-0003 §7 / agent
        // wire docs — touching this list means revisiting whether
        // the auditor chain wants to extend to a new evidence class.
        //
        // - ActivationStarted: pre-fire announcement, not evidence.
        // - EnrollmentFailed:  agent has no host-key-bound cert yet.
        // - RenewalFailed:     identity material, doesn't gate state.
        // - TrustError:        trust.json failed to load — signing key
        //                      can't be verified by an auditor without
        //                      the very roots that just failed.
        // - Other:             opaque catch-all; signing it would
        //                      paper over an unmodelled event class.
        ReportEvent::ActivationStarted { .. }
        | ReportEvent::EnrollmentFailed { .. }
        | ReportEvent::RenewalFailed { .. }
        | ReportEvent::TrustError { .. }
        | ReportEvent::Other { .. } => None,
    }
}

/// 8-char lowercase-alnum suffix for event IDs. Not crypto-grade
/// just enough to make IDs visually distinct in journal output.
fn rand_suffix(n: usize) -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..n)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    //! Unit tests for `apply_rollback_state_transition` — the
    //! Failed → Reverted flip that closes the rollback-and-halt
    //! signal loop after the agent's `RollbackTriggered` post.
    use super::*;
    use crate::db::Db;
    use crate::state::{HealthyMarker, HostRolloutState};
    use chrono::Utc;
    use nixfleet_proto::agent_wire::{ReportEvent, ReportRequest};

    fn fresh_db() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        db
    }

    fn rollback_report(host: &str, rollout: Option<&str>) -> ReportRequest {
        ReportRequest {
            hostname: host.to_string(),
            agent_version: "test".into(),
            occurred_at: Utc::now(),
            rollout: rollout.map(String::from),
            event: ReportEvent::RollbackTriggered {
                reason: "test".into(),
                signature: None,
            },
        }
    }

    #[test]
    fn rollback_triggered_flips_failed_to_reverted_then_stamps_terminals() {
        let db = fresh_db();
        // Seed an operational dispatch row + a Failed hrs row.
        let deadline = Utc::now() + chrono::Duration::seconds(120);
        db.host_dispatch_state()
            .record_dispatch(&crate::db::DispatchInsert {
                hostname: "ohm",
                rollout_id: "stable@abc12345",
                channel: "stable",
                wave: 0,
                target_closure_hash: "system-r1",
                target_channel_ref: "stable@abc12345",
                confirm_deadline: deadline,
            })
            .unwrap();
        db.rollout_state()
            .transition_host_state(
                "ohm",
                "stable@abc12345",
                HostRolloutState::Failed,
                HealthyMarker::Untouched,
                None,
            )
            .unwrap();
        // Pre-call sanity: hrs row is Failed.
        assert_eq!(
            db.rollout_state()
                .host_state("ohm", "stable@abc12345")
                .unwrap()
                .as_deref(),
            Some("Failed"),
        );

        let req = rollback_report("ohm", Some("stable@abc12345"));
        apply_rollback_state_transition(&db, &req);

        // hrs row flipped Failed → Reverted (no longer cleaned up
        // post-#81; that surface lives on dispatch_history now).
        assert_eq!(
            db.rollout_state()
                .host_state("ohm", "stable@abc12345")
                .unwrap()
                .as_deref(),
            Some("Reverted"),
        );
        // Operational state flipped to 'rolled-back'.
        let op = db
            .host_dispatch_state()
            .host_state("ohm")
            .unwrap()
            .expect("operational row present");
        assert_eq!(op.state, "rolled-back");
        // Audit row stamped terminal=rolled-back.
        let history = db
            .dispatch_history()
            .recent_for_host("ohm", 10)
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].terminal_state.as_deref(), Some("rolled-back"));
        assert!(history[0].terminal_at.is_some());
    }

    #[test]
    fn rollback_triggered_leaves_non_failed_states_untouched() {
        // The CP-410 cancel path and agent self-detected
        // activation-failure path both post `RollbackTriggered`
        // without entering `Failed` first. The DB-level guard
        // (`expected_from = Failed`) leaves Healthy / Soaked /
        // ConfirmWindow rows alone.
        let db = fresh_db();
        for state in [
            HostRolloutState::Healthy,
            HostRolloutState::Soaked,
            HostRolloutState::ConfirmWindow,
            HostRolloutState::Activating,
            HostRolloutState::Converged,
        ] {
            let rollout = format!("stable@{}", state.as_db_str().to_lowercase());
            db.rollout_state()
                .transition_host_state("ohm", &rollout, state, HealthyMarker::Untouched, None)
                .unwrap();
            let req = rollback_report("ohm", Some(&rollout));
            apply_rollback_state_transition(&db, &req);
            assert_eq!(
                db.rollout_state()
                    .host_state("ohm", &rollout)
                    .unwrap()
                    .as_deref(),
                Some(state.as_db_str()),
                "{} should not flip to Reverted",
                state.as_db_str(),
            );
        }
    }

    #[test]
    fn rollback_triggered_without_rollout_is_a_noop() {
        // A `RollbackTriggered` post with no rollout id (e.g. a
        // CP-410 cancellation that wasn't tied to a rollout in
        // the agent's state) must not error and must not touch
        // any state row. The report still records elsewhere.
        let db = fresh_db();
        db.rollout_state()
            .transition_host_state(
                "ohm",
                "stable@abc12345",
                HostRolloutState::Failed,
                HealthyMarker::Untouched,
                None,
            )
            .unwrap();
        let req = rollback_report("ohm", None);
        apply_rollback_state_transition(&db, &req);
        // Failed row untouched — no rollout id meant we couldn't
        // even target the right row.
        assert_eq!(
            db.rollout_state()
                .host_state("ohm", "stable@abc12345")
                .unwrap()
                .as_deref(),
            Some("Failed"),
        );
    }

    #[test]
    fn non_rollback_events_do_not_transition_state() {
        // The transition arm must only fire on `RollbackTriggered`.
        // ActivationFailed / RealiseFailed / etc. arrive in the
        // same handler but follow their own (currently empty)
        // pipelines.
        let db = fresh_db();
        db.rollout_state()
            .transition_host_state(
                "ohm",
                "stable@abc12345",
                HostRolloutState::Failed,
                HealthyMarker::Untouched,
                None,
            )
            .unwrap();
        let req = ReportRequest {
            hostname: "ohm".into(),
            agent_version: "test".into(),
            occurred_at: Utc::now(),
            rollout: Some("stable@abc12345".into()),
            event: ReportEvent::RealiseFailed {
                closure_hash: "abc".into(),
                reason: "substituter 503".into(),
                signature: None,
            },
        };
        apply_rollback_state_transition(&db, &req);
        assert_eq!(
            db.rollout_state()
                .host_state("ohm", "stable@abc12345")
                .unwrap()
                .as_deref(),
            Some("Failed"),
            "non-RollbackTriggered events must not trigger Failed → Reverted",
        );
    }
}
