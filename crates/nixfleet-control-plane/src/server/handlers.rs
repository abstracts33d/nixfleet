//! HTTP route handlers for the long-running CP server.
//!
//! Pulled out of the monolithic `server.rs`. Each handler is its
//! own free function with the route's signature; the router in
//! `serve.rs` (this module's parent) wires them under the `/v1/*`
//! tree. State + middleware are shared via the parent's `state` and
//! `middleware` modules.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Utc};
use nixfleet_proto::agent_wire::{
    CheckinRequest, CheckinResponse, ConfirmRequest, ReportRequest, ReportResponse,
};
use nixfleet_proto::enroll_wire::{EnrollRequest, EnrollResponse, RenewRequest, RenewResponse};
use rcgen::PublicKeyData;
use serde::Serialize;

use crate::auth_cn::PeerCertificates;

use super::middleware::require_cn;
use super::state::{
    AppState, HostCheckinRecord, ReportRecord, NEXT_CHECKIN_SECS, REPORT_RING_CAP,
};

#[derive(Debug, Serialize)]
pub(super) struct HealthzResponse {
    ok: bool,
    version: &'static str,
    /// rfc3339-formatted UTC timestamp, or `null` if the reconcile
    /// loop has not yet ticked once. (Realistic only for the first
    /// ~30s after boot.)
    last_tick_at: Option<String>,
}

pub(super) async fn healthz(state: State<Arc<AppState>>) -> Json<HealthzResponse> {
    let last = *state.last_tick_at.read().await;
    Json(HealthzResponse {
        ok: true,
        version: env!("CARGO_PKG_VERSION"),
        last_tick_at: last.map(|t| t.to_rfc3339()),
    })
}

#[derive(Debug, Serialize)]
pub(super) struct WhoamiResponse {
    cn: String,
    /// rfc3339-formatted timestamp the server received the request.
    /// `issuedAt` semantically refers to "the moment we observed
    /// this verified identity", not the cert's notBefore.
    #[serde(rename = "issuedAt")]
    issued_at: String,
}

/// `GET /v1/whoami` — returns the verified mTLS CN of the caller.
pub(super) async fn whoami(
    State(state): State<Arc<AppState>>,
    Extension(peer_certs): Extension<PeerCertificates>,
) -> Result<Json<WhoamiResponse>, StatusCode> {
    let cn = require_cn(&state, &peer_certs).await?;
    Ok(Json(WhoamiResponse {
        cn,
        issued_at: Utc::now().to_rfc3339(),
    }))
}

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

/// Gap A orphan-confirm recovery. Returns `true` when the orphan
/// confirm represents a CP-rebuild recovery case the CP can absorb
/// without forcing the agent to roll back, `false` when it should
/// fall through to a 410 (genuine wrong-rollout / cancelled case).
///
/// Recovery requires:
/// 1. A verified fleet snapshot (otherwise we cannot validate the
///    agent's claimed target).
/// 2. The agent's `request.generation.closure_hash` matches the
///    host's declared `closureHash` in the verified
///    `FleetResolved.hosts[hostname]`. This is the same authorisation
///    invariant the positive flow trusts (mTLS-CN + closure on file).
///
/// On success: synthesise a `confirmed` `pending_confirms` row +
/// stamp `record_host_healthy`. On any failure (DB error, missing
/// fleet, missing host, missing closure declaration, closure
/// mismatch): return false. Failures are non-fatal — the agent
/// still hears 410 and triggers its local rollback, no worse than
/// pre-gap-A behaviour.
async fn try_recover_orphan_confirm(
    state: &Arc<AppState>,
    req: &ConfirmRequest,
) -> bool {
    let Some(db) = state.db.as_ref() else {
        return false;
    };
    let Some(fleet) = state.verified_fleet.read().await.clone() else {
        tracing::debug!(
            hostname = %req.hostname,
            "orphan-confirm recovery: no verified fleet snapshot yet",
        );
        return false;
    };
    let Some(host_decl) = fleet.hosts.get(&req.hostname) else {
        tracing::debug!(
            hostname = %req.hostname,
            "orphan-confirm recovery: host not in verified fleet",
        );
        return false;
    };
    let Some(target_closure) = host_decl.closure_hash.as_ref() else {
        tracing::debug!(
            hostname = %req.hostname,
            "orphan-confirm recovery: host has no declared closureHash",
        );
        return false;
    };
    if target_closure != &req.generation.closure_hash {
        tracing::info!(
            hostname = %req.hostname,
            rollout = %req.rollout,
            agent_closure = %req.generation.closure_hash,
            target_closure = %target_closure,
            "orphan-confirm recovery: closure_hash mismatch — genuine 410",
        );
        return false;
    }

    // Issue #54 — defensive rollout-id check. The closure_hash match
    // above proves the agent activated the closure the fleet declares
    // for this host, but it doesn't prove `req.rollout` is THIS
    // fleet's rollout id. A future schema evolution where two
    // rollouts target the same closure (e.g. a rollback-and-halt
    // reissuing the prior rev) would let the agent's stale `req.rollout`
    // synthesise a confirmed row for the wrong rollout and corrupt
    // the audit trail. Compute the expected rollout id the same way
    // `dispatch::decide_target` does (channel + 8-char ci_commit prefix
    // or closure prefix as fallback) and refuse to synthesise on
    // mismatch.
    //
    // KNOWN EDGE CASE — fleet-forward with closure-stable rollout-id
    // change: if CI signs a new fleet.resolved (new ci_commit) between
    // the agent's dispatch and its orphan confirm, AND the host's
    // declared closure is unchanged across both fleets, the
    // expected_rollout_id derived from the *current* fleet won't match
    // the agent's *historical* rollout id. We'll return a 410, the
    // agent will roll back a healthy activation, and the next dispatch
    // will pick up the new rollout id cleanly. Operationally rare
    // (CI re-signing without a closure change is unusual: lab's CI
    // rebuilds + re-signs only on commit) and recoverable in one
    // poll cycle. Documenting here rather than gating on it because
    // a fix would require persisting the dispatch-time rollout id
    // across CP rebuilds — out of scope for gap A.
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
        return false;
    }

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
    if let Err(err) = db.record_host_healthy(&req.hostname, &req.rollout, now) {
        tracing::warn!(
            hostname = %req.hostname,
            rollout = %req.rollout,
            error = %err,
            "orphan-confirm recovery: record_host_healthy failed (synthetic row already inserted)",
        );
        // Don't reverse the synthetic insert — leaving it in place
        // means the rollout audit trail still reflects the
        // activation, even if the soak marker is missing. The
        // soak loop's worst case becomes "this host's soak timer
        // restarts on next confirm" — same as pre-gap-B.
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
/// (RFC-0003 §4.1 wire-additive field) lets the CP repopulate
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
    if let Err(err) = db.record_host_healthy(&req.hostname, &rollout_id, stamp) {
        tracing::warn!(
            hostname = %req.hostname,
            rollout = %rollout_id,
            error = %err,
            "soak-state recovery: record_host_healthy failed (synthetic confirmed row already inserted)",
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

/// Per-checkin "left Healthy" sweep (RFC-0002 §3.2). Compares the
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
        match db.clear_host_healthy(&req.hostname, &rollout_id) {
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
                    "checkin: clear_host_healthy failed",
                );
            }
        }
    }
}

/// Per-checkin dispatch decision. Reads the latest verified
/// `FleetResolved` from `AppState`, queries the DB for any pending
/// confirm row for this host (idempotency guard), and runs
/// `dispatch::decide_target`. On `Dispatch`, inserts a
/// `pending_confirms` row keyed on the deterministic rollout id and
/// returns the target. All other Decision variants resolve to None.
///
/// Failures here log + return None — a transient DB hiccup or
/// missing fleet snapshot must not surface as HTTP 500 to the
/// agent. The agent retries on its next checkin (60s).
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

    // Issue #59 — wave-staging compliance gate. Block dispatch when
    // any host on this host's channel has outstanding signature-
    // verified ComplianceFailure / RuntimeGateError events under
    // `compliance.mode = "enforce"`. Permissive mode never blocks;
    // disabled / no-mode falls through. The gate is per-channel —
    // wave N's outstanding failures hold wave N+1.
    if let Some(channel_name) = fleet.hosts.get(&req.hostname).map(|h| &h.channel) {
        if let Some(channel) = fleet.channels.get(channel_name) {
            // Channel mode is a plain enum-string after the
            // strict-removal cleanup; `from_wire_str` is forward-
            // compat for unknown strings (fall back to Permissive).
            let resolved_mode =
                nixfleet_proto::compliance::GateMode::from_wire_str(&channel.compliance.mode);
            // Gather per-host (records, current_rollout) for every
            // host on this channel.
            let reports_guard = state.host_reports.read().await;
            let checkins_guard = state.host_checkins.read().await;
            let channel_hosts: Vec<&String> = fleet
                .hosts
                .iter()
                .filter_map(|(n, h)| (h.channel == *channel_name).then_some(n))
                .collect();
            // Stage data into owned slices so the iterator passed
            // into evaluate_channel_gate has stable lifetimes. Each
            // entry carries the host's wave_index for the per-wave
            // gate decision (#59 issue E).
            type StagedHost = (
                String,                              // hostname
                Vec<crate::server::ReportRecord>,    // report buffer (cloned)
                Option<String>,                       // current rollout id
                Option<u32>,                          // wave index
            );
            let staged: Vec<StagedHost> = channel_hosts
                .iter()
                .map(|n| {
                    let buf: Vec<crate::server::ReportRecord> = reports_guard
                        .get(*n)
                        .map(|q| q.iter().cloned().collect())
                        .unwrap_or_default();
                    let cur_rollout = checkins_guard
                        .get(*n)
                        .and_then(|c| c.checkin.last_evaluated_target.as_ref())
                        .and_then(|t| t.rollout_id.clone());
                    let wave_idx = fleet
                        .waves
                        .get(channel_name)
                        .and_then(|waves| {
                            waves
                                .iter()
                                .position(|w| w.hosts.iter().any(|h| h == *n))
                                .map(|i| i as u32)
                        });
                    (n.to_string(), buf, cur_rollout, wave_idx)
                })
                .collect();
            drop(reports_guard);
            drop(checkins_guard);

            // Wave the requesting host belongs to.
            let requesting_wave = fleet.waves.get(channel_name).and_then(|waves| {
                waves
                    .iter()
                    .position(|w| w.hosts.iter().any(|h| h == &req.hostname))
                    .map(|i| i as u32)
            });

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
                return None;
            }
            // Permissive: log advisory but don't block.
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
        }
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
        } => {
            let confirm_deadline =
                now + chrono::Duration::seconds(state.confirm_deadline_secs);
            match db.record_pending_confirm(
                &req.hostname,
                &rollout_id,
                /* wave */ wave_index.unwrap_or(0),
                &target.closure_hash,
                &target.channel_ref,
                confirm_deadline,
            ) {
                Ok(_) => {
                    tracing::info!(
                        target: "dispatch",
                        hostname = %req.hostname,
                        rollout = %rollout_id,
                        target_closure = %target.closure_hash,
                        confirm_deadline = %confirm_deadline.to_rfc3339(),
                        "dispatch: target issued",
                    );
                    Some(target)
                }
                Err(err) => {
                    tracing::warn!(
                        hostname = %req.hostname,
                        rollout = %rollout_id,
                        error = %err,
                        "dispatch: record_pending_confirm failed; returning no target",
                    );
                    None
                }
            }
        }
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

/// `POST /v1/agent/report` — record an out-of-band event report.
///
/// In-memory ring buffer per host, capped at `REPORT_RING_CAP`.
/// New reports push to the back; oldest is dropped on overflow.
/// Future work: promote to SQLite + correlate with rollouts.
pub(super) async fn report(
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

    let event_id = format!(
        "evt-{}-{}",
        Utc::now().timestamp_millis(),
        rand_suffix(8)
    );
    let received_at = Utc::now();

    // Render the event variant for the journal in a grep-friendly
    // way: `event=activation-failed`, `event=verify-mismatch`, etc.
    // The serde_json round-trip extracts the kebab-case discriminator.
    let event_str = serde_json::to_value(&req.event)
        .ok()
        .and_then(|v| v.get("event").and_then(|e| e.as_str()).map(String::from))
        .unwrap_or_else(|| "<unknown>".to_string());
    let rollout_str = req
        .rollout
        .clone()
        .unwrap_or_else(|| "<none>".to_string());

    // Issue #12 root-3 / #59 — verify probe-output signatures on the
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

    // Issue #60 — write through to SQLite alongside the in-memory
    // ring buffer. Ring stays for hot-path latency in dispatch
    // decisions; SQLite is the durable record that survives CP
    // restart. DB write is best-effort: a failure logs warn + lets
    // the in-memory write proceed (degraded == old in-memory-only
    // behaviour, so no regression).
    if let Some(db) = state.db.as_ref() {
        let signature_status_str = signature_status.as_ref().and_then(|s| {
            serde_json::to_value(s).ok().and_then(|v| {
                v.as_str().map(String::from)
            })
        });
        // Best-effort SQLite persistence. Two failure modes, both
        // handled the same way: log + skip the DB write, let the
        // in-memory ring buffer below absorb the event regardless.
        // The serde failure path is what matters here — previously
        // `unwrap_or_default()` would write `""` into report_json,
        // and on next CP startup the hydration parse would fail and
        // skip the row, leaving a phantom DB row that could never
        // be replayed. Now we never write the row at all on serde
        // failure.
        match serde_json::to_string(&req) {
            Ok(report_json) => {
                if let Err(err) = db.record_host_report(&crate::db::HostReportInsert {
                    hostname: &req.hostname,
                    event_id: &event_id,
                    received_at,
                    event_kind: &event_str,
                    rollout: req.rollout.as_deref(),
                    signature_status: signature_status_str.as_deref(),
                    report_json: &report_json,
                }) {
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

    let mut reports = state.host_reports.write().await;
    let buf = reports.entry(req.hostname).or_default();
    if buf.len() >= REPORT_RING_CAP {
        buf.pop_front();
    }
    buf.push_back(record);

    Ok(Json(ReportResponse { event_id }))
}

/// Compute the signature verdict for an incoming report (issue #12
/// root-3 / #59). Only `ComplianceFailure` and `RuntimeGateError`
/// carry probe-output signatures today; all other variants return
/// `None`. The host's pubkey comes from `verified_fleet`'s
/// `hosts.<hostname>.pubkey`; absent pubkey → `NoPubkey`.
async fn compute_signature_status(
    state: &Arc<AppState>,
    req: &ReportRequest,
) -> Option<crate::evidence_verify::SignatureStatus> {
    use crate::evidence_verify;
    use nixfleet_proto::agent_wire::ReportEvent;

    let pubkey: Option<String> = {
        let fleet_guard = state.verified_fleet.read().await;
        fleet_guard
            .as_ref()
            .and_then(|f| f.hosts.get(&req.hostname))
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
            let payload = evidence_verify::ComplianceFailureSignedPayload {
                hostname: &req.hostname,
                rollout: req.rollout.as_deref(),
                control_id,
                status,
                framework_articles,
                evidence_collected_at: *evidence_collected_at,
                evidence_snippet_sha256: snippet_sha,
            };
            Some(evidence_verify::verify_event(
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
            let payload = evidence_verify::RuntimeGateErrorSignedPayload {
                hostname: &req.hostname,
                rollout: req.rollout.as_deref(),
                reason,
                collector_exit_code: *collector_exit_code,
                evidence_collected_at: *evidence_collected_at,
                activation_completed_at: *activation_completed_at,
            };
            Some(evidence_verify::verify_event(
                signature.as_deref(),
                pubkey.as_deref(),
                &payload,
            ))
        }
        _ => None,
    }
}

/// 8-char lowercase-alnum suffix for event IDs. Not crypto-grade —
/// just enough to make IDs visually distinct in journal output.
fn rand_suffix(n: usize) -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..n)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

/// `POST /v1/enroll` — bootstrap a new fleet host.
///
/// No mTLS required (this is the path before the host has a cert).
/// Authentication is via the bootstrap-token signature against the
/// org root key in trust.json. Order of checks matches RFC-0003 §2:
/// 1. Replay defense
/// 2. Expiry
/// 3. Signature against `orgRootKey.{current,previous}`
/// 4. Hostname binding (claim ↔ CSR CN)
/// 5. Pubkey-fingerprint binding (SHA-256 of CSR pubkey DER)
pub(super) async fn enroll(
    State(state): State<Arc<AppState>>,
    Json(req): Json<EnrollRequest>,
) -> Result<Json<EnrollResponse>, StatusCode> {
    use base64::Engine;

    let now = chrono::Utc::now();

    let db = state.db.as_ref().ok_or_else(|| {
        tracing::warn!("enroll: no db configured — endpoint unusable");
        StatusCode::SERVICE_UNAVAILABLE
    })?;

    // 1. Replay defense.
    match db.token_seen(&req.token.claims.nonce) {
        Ok(true) => {
            tracing::warn!(nonce = %req.token.claims.nonce, "enroll: token replay rejected");
            return Err(StatusCode::CONFLICT);
        }
        Ok(false) => {}
        Err(err) => {
            tracing::error!(error = %err, "enroll: db token_seen failed");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }

    // 2. Expiry.
    if now < req.token.claims.issued_at || now >= req.token.claims.expires_at {
        tracing::warn!(
            hostname = %req.token.claims.hostname,
            "enroll: token outside validity window"
        );
        return Err(StatusCode::UNAUTHORIZED);
    }

    // 3. Signature against orgRootKey. Re-read trust.json each
    // enroll so operator key rotations propagate without restart.
    let trust_path = state
        .issuance_paths
        .read()
        .await
        .fleet_ca_cert
        .as_ref()
        .and_then(|p| p.parent())
        .map(|d| d.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("/etc/nixfleet/cp"))
        .join("trust.json");
    let trust_raw = std::fs::read_to_string(&trust_path).map_err(|err| {
        tracing::error!(error = %err, path = %trust_path.display(), "enroll: read trust.json");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let trust: nixfleet_proto::TrustConfig = serde_json::from_str(&trust_raw).map_err(|err| {
        tracing::error!(error = %err, "enroll: parse trust.json");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let org_root = trust.org_root_key.as_ref().ok_or_else(|| {
        tracing::error!(
            "enroll: trust.json has no orgRootKey — refusing to accept any token. \
             Set nixfleet.trust.orgRootKey.current in fleet.nix and rebuild."
        );
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let candidates = org_root.active_keys();
    if candidates.is_empty() {
        tracing::error!("enroll: orgRootKey has no current/previous keys");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    let mut sig_ok = false;
    for pubkey in &candidates {
        if pubkey.algorithm != "ed25519" {
            tracing::warn!(
                algorithm = %pubkey.algorithm,
                "enroll: skipping non-ed25519 orgRootKey candidate (only ed25519 supported)"
            );
            continue;
        }
        let pubkey_bytes = match base64::engine::general_purpose::STANDARD.decode(&pubkey.public) {
            Ok(b) => b,
            Err(err) => {
                tracing::warn!(error = %err, "enroll: orgRootKey base64 decode");
                continue;
            }
        };
        if crate::issuance::verify_token_signature(&req.token, &pubkey_bytes).is_ok() {
            sig_ok = true;
            break;
        }
    }
    if !sig_ok {
        tracing::warn!(
            hostname = %req.token.claims.hostname,
            nonce = %req.token.claims.nonce,
            "enroll: token signature did not verify against any orgRootKey candidate"
        );
        return Err(StatusCode::UNAUTHORIZED);
    }

    // 4. Hostname / 5. pubkey-fingerprint validation against CSR.
    let csr_params =
        rcgen::CertificateSigningRequestParams::from_pem(&req.csr_pem).map_err(|err| {
            tracing::warn!(error = %err, "enroll: parse CSR PEM");
            StatusCode::BAD_REQUEST
        })?;
    let csr_cn: Option<String> = csr_params
        .params
        .distinguished_name
        .iter()
        .find_map(|(t, v): (&rcgen::DnType, &rcgen::DnValue)| {
            if matches!(t, rcgen::DnType::CommonName) {
                Some(match v {
                    rcgen::DnValue::PrintableString(s) => s.to_string(),
                    rcgen::DnValue::Utf8String(s) => s.to_string(),
                    _ => format!("{:?}", v),
                })
            } else {
                None
            }
        });
    let csr_cn = csr_cn.ok_or_else(|| {
        tracing::warn!("enroll: CSR has no CN");
        StatusCode::BAD_REQUEST
    })?;
    let csr_pubkey_der = csr_params.public_key.der_bytes();
    let csr_fingerprint = crate::issuance::fingerprint(csr_pubkey_der);

    if let Err(err) = crate::issuance::validate_token_claims(
        &req.token.claims,
        &csr_cn,
        &csr_fingerprint,
        now,
    ) {
        tracing::warn!(error = %err, hostname = %req.token.claims.hostname, "enroll: claim validation");
        return Err(StatusCode::UNAUTHORIZED);
    }

    // All checks passed — commit the nonce as seen.
    if let Err(err) = db.record_token_nonce(&req.token.claims.nonce, &req.token.claims.hostname) {
        tracing::warn!(error = %err, "enroll: db record_token_nonce failed; proceeding");
    }

    // Issue the cert.
    let paths = state.issuance_paths.read().await.clone();
    let (ca_cert, ca_key, audit_log_path) = match (&paths.fleet_ca_cert, &paths.fleet_ca_key) {
        (Some(c), Some(k)) => (c.clone(), k.clone(), paths.audit_log.clone()),
        _ => {
            tracing::error!("enroll: fleet CA cert/key paths not configured");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    let (cert_pem, not_after) = crate::issuance::issue_cert(
        &req.csr_pem,
        &ca_cert,
        &ca_key,
        crate::issuance::AGENT_CERT_VALIDITY,
        now,
    )
    .map_err(|err| {
        tracing::error!(error = %err, "enroll: issue_cert failed");
        StatusCode::BAD_REQUEST
    })?;

    if let Some(path) = &audit_log_path {
        crate::issuance::audit_log(
            path,
            now,
            "<enroll>",
            &req.token.claims.hostname,
            not_after,
            &crate::issuance::AuditContext::Enroll {
                token_nonce: req.token.claims.nonce.clone(),
            },
        );
    }
    tracing::info!(
        target: "issuance",
        hostname = %req.token.claims.hostname,
        not_after = %not_after.to_rfc3339(),
        "enrolled"
    );

    Ok(Json(EnrollResponse { cert_pem, not_after }))
}

/// `POST /v1/agent/renew` — issue a fresh cert for an authenticated
/// agent. mTLS-required; the verified CN is stamped onto the new
/// cert via `issuance::issue_cert`.
pub(super) async fn renew(
    State(state): State<Arc<AppState>>,
    Extension(peer_certs): Extension<PeerCertificates>,
    Json(req): Json<RenewRequest>,
) -> Result<Json<RenewResponse>, StatusCode> {
    let cn = require_cn(&state, &peer_certs).await?;
    let now = chrono::Utc::now();

    let paths = state.issuance_paths.read().await.clone();
    let (ca_cert, ca_key, audit_log_path) = match (&paths.fleet_ca_cert, &paths.fleet_ca_key) {
        (Some(c), Some(k)) => (c.clone(), k.clone(), paths.audit_log.clone()),
        _ => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };

    let (cert_pem, not_after) = crate::issuance::issue_cert(
        &req.csr_pem,
        &ca_cert,
        &ca_key,
        crate::issuance::AGENT_CERT_VALIDITY,
        now,
    )
    .map_err(|err| {
        tracing::error!(error = %err, "renew: issue_cert failed");
        StatusCode::BAD_REQUEST
    })?;

    if let Some(path) = &audit_log_path {
        crate::issuance::audit_log(
            path,
            now,
            &cn,
            &cn,
            not_after,
            &crate::issuance::AuditContext::Renew {
                previous_cert_serial: "<unknown>".to_string(),
            },
        );
    }
    tracing::info!(
        target: "issuance",
        hostname = %cn,
        not_after = %not_after.to_rfc3339(),
        "renewed"
    );

    Ok(Json(RenewResponse { cert_pem, not_after }))
}

/// `POST /v1/agent/confirm` — agent confirms successful activation.
/// Marks the matching `pending_confirms` row as confirmed.
///
/// Behaviour:
/// - Pending row exists, deadline not passed → mark confirmed, 204.
/// - No matching row in 'pending' state → orphan-recovery path
///   (gap A in docs/roadmap/0002-v0.2-completeness-gaps.md): if the
///   agent's reported `closure_hash` matches the host's declared
///   target in the verified `FleetResolved`, treat this as a CP-
///   rebuild recovery — synthesise a confirmed pending_confirms
///   row + record_host_healthy + 204. Closure-hash mismatch →
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
        // Try the gap A orphan-confirm recovery path before
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
        // Standard path: stamp last_healthy_since (RFC-0002 §3.2
        // ConfirmWindow → Healthy). The orphan-recovery branch
        // already wrote it inline so we don't double-up here.
        if let Err(err) = db.record_host_healthy(&req.hostname, &req.rollout, Utc::now()) {
            tracing::warn!(
                hostname = %req.hostname,
                rollout = %req.rollout,
                error = %err,
                "confirm: record_host_healthy failed; soak timer will skip this host",
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

#[derive(Debug, Serialize)]
pub(super) struct ChannelStatusResponse {
    /// Channel name as declared in `fleet.resolved.channels`.
    name: String,
    /// CI commit currently on the verified `fleet.resolved`
    /// snapshot — the ref the CP is rolling out toward. Mirrors
    /// `Observed.channel_refs[name]` once the projection has
    /// caught up. `None` when the verified snapshot's
    /// `meta.ciCommit` is unset (offline / file-backed deploys).
    declared_ci_commit: Option<String>,
    /// rfc3339 of the verified `meta.signedAt`. Operators read
    /// this to confirm the snapshot is fresh.
    signed_at: Option<String>,
    /// Channel's per-policy `freshnessWindow` (minutes), so
    /// operators can compare `now - signedAt` against the gate
    /// without re-fetching the artifact.
    freshness_window_minutes: u32,
}

/// `GET /v1/channels/{name}` — declared vs currently-rolled
/// snapshot for a channel (issue #3 acceptance criterion). Reads
/// from the in-memory verified-fleet snapshot — the same source
/// of truth dispatch decisions are made against. Returns 404 when
/// the channel is not declared in the verified `FleetResolved`.
/// Returns 503 when no verified snapshot has been primed yet
/// (CP just booted; agents will see 503 on this endpoint until
/// the channel-refs poll succeeds).
pub(super) async fn channel_status(
    State(state): State<Arc<AppState>>,
    Extension(peer_certs): Extension<PeerCertificates>,
    Path(name): Path<String>,
) -> Result<Json<ChannelStatusResponse>, StatusCode> {
    let _cn = require_cn(&state, &peer_certs).await?;

    let snapshot = state.verified_fleet.read().await.clone();
    let fleet = snapshot.ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let channel = fleet.channels.get(&name).ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(ChannelStatusResponse {
        name,
        declared_ci_commit: fleet.meta.ci_commit.clone(),
        signed_at: fleet.meta.signed_at.map(|t| t.to_rfc3339()),
        freshness_window_minutes: channel.freshness_window,
    }))
}

#[derive(Debug, Serialize)]
pub(super) struct HostsResponse {
    hosts: Vec<HostStatusEntry>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct HostStatusEntry {
    /// Hostname per `fleet.resolved.hosts`.
    hostname: String,
    /// Channel the host is on (declarative).
    channel: String,
    /// Closure declared as the host's target on the most recent
    /// verified `fleet.resolved.json` snapshot. Null when the
    /// fleet hasn't been signed since CI started stamping
    /// closure hashes.
    declared_closure_hash: Option<String>,
    /// Closure the host is currently running, per its most recent
    /// checkin. `None` when the host has never checked in.
    current_closure_hash: Option<String>,
    /// Closure queued for next boot, if any. Null when current ==
    /// pending (the typical converged-and-rebooted state).
    pending_closure_hash: Option<String>,
    /// Wall-clock of the most recent checkin (rfc3339). `None`
    /// when the host has never checked in.
    last_checkin_at: Option<String>,
    /// Rollout id the host most recently confirmed against, per
    /// the agent's last `last_evaluated_target` echo. Useful for
    /// operators to see "host X is on rollout Y" without
    /// scraping the journal.
    last_rollout_id: Option<String>,
    /// True iff the host's current closure matches the declared
    /// closure on the verified fleet snapshot. Mirrors the
    /// dispatch decision `Decision::Converged`.
    converged: bool,
    /// Count of outstanding `ComplianceFailure` events in the
    /// host's report buffer (events whose rollout matches the
    /// host's current rollout — i.e. events not yet
    /// resolved-by-replacement). 0 when the host has no
    /// outstanding events.
    outstanding_compliance_failures: usize,
    /// Count of outstanding `RuntimeGateError` events (same
    /// resolution semantics as `outstanding_compliance_failures`).
    outstanding_runtime_gate_errors: usize,
    /// Count of `signature_status = Verified` events in the
    /// host's report buffer. Auditor-chain visibility metric —
    /// when this matches `outstanding_compliance_failures +
    /// outstanding_runtime_gate_errors`, every outstanding
    /// failure has a verified signature against the host's
    /// `fleet.resolved.hosts.<n>.pubkey`.
    verified_event_count: usize,
}

/// `GET /v1/hosts` — fleet-wide observed state, per host.
///
/// Joins the verified fleet snapshot's host declarations with the
/// CP's per-host checkin record + report buffer. Replaces the
/// brittle journal-scraping path that fleet-status' render.sh
/// previously used (the JSON tracing subscriber emits structured
/// fields that don't match the bash awk parser's `key=value`
/// expectation).
pub(super) async fn hosts_status(
    State(state): State<Arc<AppState>>,
    Extension(peer_certs): Extension<PeerCertificates>,
) -> Result<Json<HostsResponse>, StatusCode> {
    let _cn = require_cn(&state, &peer_certs).await?;

    let fleet = state
        .verified_fleet
        .read()
        .await
        .clone()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let checkins = state.host_checkins.read().await;
    let reports = state.host_reports.read().await;

    let mut entries: Vec<HostStatusEntry> = fleet
        .hosts
        .iter()
        .map(|(hostname, host_decl)| {
            let checkin = checkins.get(hostname);
            let last_checkin_at = checkin.map(|c| c.last_checkin.to_rfc3339());
            let current = checkin.map(|c| c.checkin.current_generation.closure_hash.clone());
            let pending = checkin.and_then(|c| {
                c.checkin
                    .pending_generation
                    .as_ref()
                    .map(|p| p.closure_hash.clone())
            });
            let last_rollout_id = checkin.and_then(|c| {
                c.checkin
                    .last_evaluated_target
                    .as_ref()
                    .and_then(|t| t.rollout_id.clone())
            });
            let converged = match (&host_decl.closure_hash, &current) {
                (Some(declared), Some(running)) => declared == running,
                _ => false,
            };

            // Walk the host's report buffer, counting outstanding
            // ComplianceFailure / RuntimeGateError events scoped to
            // the host's current rollout id (resolution-by-
            // replacement: events for older rollouts are considered
            // resolved).
            let host_buf = reports.get(hostname);
            let cur_rollout = last_rollout_id.as_deref();
            let mut compliance_failures = 0usize;
            let mut runtime_gate_errors = 0usize;
            let mut verified_count = 0usize;
            if let Some(buf) = host_buf {
                use nixfleet_proto::agent_wire::ReportEvent;
                for record in buf.iter() {
                    let is_compliance =
                        matches!(record.report.event, ReportEvent::ComplianceFailure { .. });
                    let is_runtime_gate =
                        matches!(record.report.event, ReportEvent::RuntimeGateError { .. });
                    if !is_compliance && !is_runtime_gate {
                        continue;
                    }
                    // Resolution-by-replacement check: skip events
                    // whose rollout the host has moved past.
                    let event_rollout = record.report.rollout.as_deref();
                    let outstanding = !matches!(
                        (cur_rollout, event_rollout),
                        (Some(cur), Some(ev_r)) if cur != ev_r
                    );
                    if !outstanding {
                        continue;
                    }
                    if is_compliance {
                        compliance_failures += 1;
                    }
                    if is_runtime_gate {
                        runtime_gate_errors += 1;
                    }
                    if matches!(
                        record.signature_status,
                        Some(crate::evidence_verify::SignatureStatus::Verified)
                    ) {
                        verified_count += 1;
                    }
                }
            }

            HostStatusEntry {
                hostname: hostname.clone(),
                channel: host_decl.channel.clone(),
                declared_closure_hash: host_decl.closure_hash.clone(),
                current_closure_hash: current,
                pending_closure_hash: pending,
                last_checkin_at,
                last_rollout_id,
                converged,
                outstanding_compliance_failures: compliance_failures,
                outstanding_runtime_gate_errors: runtime_gate_errors,
                verified_event_count: verified_count,
            }
        })
        .collect();
    entries.sort_by(|a, b| a.hostname.cmp(&b.hostname));

    Ok(Json(HostsResponse { hosts: entries }))
}

/// `GET /v1/agent/closure/{hash}` — closure proxy fallback for hosts
/// that can't reach the binary cache directly. Forwards narinfo
/// requests to the configured cache upstream (any nix-cache-protocol
/// HTTP backend: harmonia, attic, cachix, plain nix-serve, …). Real
/// Nix-cache-protocol forwarding (full nar streaming) is a follow-up
/// PR; this lands the wire shape + the upstream config path.
///
/// When `closure_upstream` is unset, returns 501 Not Implemented.
pub(super) async fn closure_proxy(
    State(state): State<Arc<AppState>>,
    Extension(peer_certs): Extension<PeerCertificates>,
    Path(closure_hash): Path<String>,
) -> Result<Response, StatusCode> {
    let cn = require_cn(&state, &peer_certs).await?;

    let upstream = match &state.closure_upstream {
        Some(u) => u,
        None => {
            tracing::info!(
                target: "closure_proxy",
                cn = %cn,
                closure = %closure_hash,
                "closure proxy hit but no --closure-upstream configured (501)"
            );
            let body = serde_json::json!({
                "error": "closure proxy not configured",
                "closure": closure_hash,
                "tracking": "set services.nixfleet-control-plane.closureUpstream",
            });
            return Ok(Response::builder()
                .status(StatusCode::NOT_IMPLEMENTED)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("Response::builder with valid status + body is infallible"));
        }
    };

    let url = format!(
        "{}/{}.narinfo",
        upstream.base_url.trim_end_matches('/'),
        closure_hash
    );
    tracing::debug!(target: "closure_proxy", cn = %cn, url = %url, "forwarding");

    let resp = match upstream.client.get(&url).send().await {
        Ok(r) => r,
        Err(err) => {
            tracing::warn!(error = %err, "closure proxy: upstream unreachable");
            return Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(Body::from(format!("upstream error: {err}")))
                .expect("Response::builder with valid status + body is infallible"));
        }
    };
    let status = resp.status().as_u16();
    let body = resp.bytes().await.map_err(|err| {
        tracing::warn!(error = %err, "closure proxy: upstream body read failed");
        StatusCode::BAD_GATEWAY
    })?;
    Ok(Response::builder()
        .status(status)
        .header("content-type", "text/x-nix-narinfo")
        .body(Body::from(body))
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
        // confirm or from gap A's orphan recovery). Re-attestation
        // must NOT overwrite — the existing row is more
        // authoritative.
        let fleet = fleet_with_host("test-host", Some("system-r1"));
        let (state, db) = state_with_fleet_and_db(fleet).await;

        // Pre-populate a Healthy row for the rollout the host
        // would derive (channel "stable", short "abc12345" from the
        // fleet's ci_commit).
        let original = Utc::now() - chrono::Duration::seconds(5);
        db.record_host_healthy("test-host", "stable@abc12345", original)
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
