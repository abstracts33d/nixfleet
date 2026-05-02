//! Background reconcile loop.
//!
//! Runs every [`RECONCILE_INTERVAL`] (30s default), reads the
//! in-memory projection of host checkins + Forgejo channel-refs,
//! verifies the build-time `--artifact` against the trust file,
//! reconciles, and writes the resulting `FleetResolved` snapshot
//! into `AppState.verified_fleet` — *only* when the new bytes are
//! at least as fresh as what's already there. The Forgejo poll
//! task is the other writer; the freshness gate keeps its
//! Forgejo-fresh snapshot from being clobbered by the static
//! build-time bytes.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use nixfleet_proto::FleetResolved;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::{render_plan, tick, TickInputs};

use super::state::{AppState, HostCheckinRecord, RECONCILE_INTERVAL};

/// Spawn the reconcile loop. Each tick:
/// 1. Reads the channel-refs cache (refreshed by the Forgejo poll
///   task; falls back to file-backed observed.json when empty).
/// 2. Builds an `Observed` from the in-memory checkin state +
///   cached channel-refs.
/// 3. Verifies the resolved artifact and reconciles against the
///   projected `Observed`.
/// 4. Emits the plan via tracing.
///
/// Errors at any step are logged and fall through; the loop never
/// crashes on transient failures.
pub(super) fn spawn_reconcile_loop(
    cancel: CancellationToken,
    state: Arc<AppState>,
    inputs: TickInputs,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Prime the verified-fleet snapshot from the build-time
        // artifact, IF it isn't already populated. The Forgejo
        // prime in `serve ` runs first and sets it from the
        // operator's freshest repo bytes; this fallback only fires
        // when Forgejo isn't configured or its fetch failed. Either
        // way we don't overwrite a Forgejo-fresh snapshot with a
        // stale build-time one — that's exactly the regression that
        // caused lab to stair-step backwards through deploy history
        // during the GitOps validation pass.
        {
            let already_primed = state.verified_fleet.read().await.is_some();
            if !already_primed {
                let prime_inputs = TickInputs {
                    now: Utc::now(),
                    ..inputs.clone()
                };
                if let Some(fleet) = verify_fleet_only(&prime_inputs) {
                    // Compute the canonical-bytes hash that anchors
                    // every rolloutId derivation downstream (RFC-0002
                    // §4.4). Re-canonicalising the parsed FleetResolved
                    // is byte-stable. Atomic write of (fleet, hash)
                    // pair: a torn snapshot would corrupt the anchor.
                    let fleet_hash = nixfleet_reconciler::compute_canonical_hash(&fleet).ok();
                    if let Some(h) = fleet_hash {
                        *state.verified_fleet.write().await =
                            Some(crate::server::VerifiedFleetSnapshot {
                                fleet: Arc::new(fleet),
                                fleet_resolved_hash: h,
                            });
                    }
                    tracing::info!(
                        target: "reconcile",
                        "primed verified-fleet snapshot from build-time artifact (Forgejo prime unavailable)",
                    );
                } else {
                    tracing::warn!(
                        target: "reconcile",
                        "could not prime verified-fleet snapshot (verify failed); dispatch will block until first tick succeeds",
                    );
                }
            } else {
                tracing::debug!(
                    target: "reconcile",
                    "verified-fleet snapshot already populated by Forgejo prime; skipping build-time prime",
                );
            }
        }

        let mut ticker =
            tokio::time::interval_at(Instant::now() + RECONCILE_INTERVAL, RECONCILE_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::info!(target: "shutdown", task = "reconcile_loop", "task shut down");
                    return;
                }
                _ = ticker.tick() => {}
            }
            let now = Utc::now();

            // Snapshot the cache + checkins under read locks. Drop
            // the locks before doing the (potentially slow) verify +
            // reconcile work.
            let channel_refs = {
                let cache = state.channel_refs_cache.read().await;
                cache.refs.clone()
            };
            let checkins = state.host_checkins.read().await.clone();

            // Active rollouts come from the DB snapshot when the
            // CP has persistence. Without a DB, the projection
            // sees no rollouts (same as before this PR landed).
            // Errors here are logged + treated as empty so the
            // tick still runs; the reconciler is read-only on
            // active_rollouts so missing data is conservative.
            let rollouts = match state
                .db
                .as_deref()
                .map(|db| db.host_dispatch_state().active_rollouts_snapshot())
            {
                Some(Ok(v)) => v,
                Some(Err(err)) => {
                    tracing::warn!(error = %err, "reconcile: active_rollouts_snapshot failed; treating as empty");
                    Vec::new()
                }
                None => Vec::new(),
            };

            // — fold the durable per-host outstanding-event
            // counts into Observed so the reconciler can emit
            // Action::WaveBlocked. Empty map on missing DB or query
            // failure (degraded == old behaviour).
            let compliance_failures_by_rollout = match state
                .db
                .as_deref()
                .map(|db| db.reports().outstanding_compliance_events_by_rollout())
            {
                Some(Ok(m)) => m,
                Some(Err(err)) => {
                    tracing::warn!(
                        error = %err,
                        "reconcile: outstanding_compliance_events_by_rollout failed; treating as empty",
                    );
                    HashMap::new()
                }
                None => HashMap::new(),
            };

            // Live projection: in-memory checkins + cached channel-refs
            // + DB-derived rollouts. When the Forgejo poll hasn't
            // succeeded yet AND no agents have checked in, fall
            // back to the file-backed observed.json so the deploy-
            // without-agents path keeps working.
            let inputs_now = TickInputs {
                now,
                ..inputs.clone()
            };
            let (result, verified_fleet) = if checkins.is_empty() && channel_refs.is_empty() {
                (tick(&inputs_now), verify_fleet_only(&inputs_now))
            } else {
                run_tick_with_projection(
                    &inputs_now,
                    &checkins,
                    &channel_refs,
                    &rollouts,
                    compliance_failures_by_rollout,
                )
            };

            // Snapshot the verified fleet so the dispatch path can
            // read it. Three preserve rules layered on top:
            //
            // 1. Verify-failed tick → preserve previous snapshot.
            // 2. The build-time artifact path is immutable for the
            // CP's lifetime, so the bytes verify_fleet_only re-
            // reads here are the SAME every tick.
            // 3. Compare `signed_at`: only overwrite if the new
            // snapshot is at least as fresh as what's already
            // there. Keeps the Forgejo-fresh snapshot from being
            // clobbered.
            if let Some(fleet) = verified_fleet {
                let mut guard = state.verified_fleet.write().await;
                let should_overwrite = match guard.as_ref() {
                    None => true,
                    Some(existing) => {
                        match (existing.fleet.meta.signed_at, fleet.meta.signed_at) {
                            (Some(prev), Some(new)) => new >= prev,
                            // Either lacks signed_at (shouldn't happen
                            // for verified artifacts) → fall back to
                            // overwriting.
                            _ => true,
                        }
                    }
                };
                if should_overwrite {
                    if let Ok(h) = nixfleet_reconciler::compute_canonical_hash(&fleet) {
                        // Atomic write of (fleet, hash) pair under a
                        // single lock: dispatch readers can never see
                        // a torn snapshot.
                        *guard = Some(crate::server::VerifiedFleetSnapshot {
                            fleet: Arc::new(fleet),
                            fleet_resolved_hash: h,
                        });
                    }
                }
            }

            match result {
                Ok(out) => {
                    apply_actions(&state, &out).await;
                    let plan = render_plan(&out);
                    tracing::info!(target: "reconcile", "{}", plan.trim_end());
                }
                Err(err) => {
                    tracing::warn!(error = %err, "reconcile tick failed");
                }
            }
            *state.last_tick_at.write().await = Some(now);
        }
    })
}

/// Apply the side-effects of the reconciler's action stream that
/// require CP-side state mutation:
///
/// - `Action::SoakHost` — flip Healthy → Soaked on the host's row.
/// - `Action::ConvergeRollout` — stamp every open `dispatch_history`
///   row for the rollout with `terminal_state = 'converged'`. The
///   operational `host_dispatch_state` row stays Confirmed and is
///   replaced on the next dispatch. The action is the
///   reconciler's terminal signal — ConvergeRollout fires only when
///   every wave is fully Soaked, i.e. no host on that rollout still
///   has work to do.
///
/// Other action variants (HaltRollout, RollbackHost, ChannelUnknown,
/// WaveBlocked, …) are emitted for journal/observability and don't
/// write to the DB. Errors per action are logged + skipped; the tick
/// completes regardless. The action stream is at-least-once: the
/// reconciler re-emits SoakHost on every tick while the host remains
/// Healthy + soak elapsed, so transient failures retry on the next
/// tick automatically. ConvergeRollout is also re-emitted while the
/// rows survive — first run deletes, subsequent runs are no-ops.
async fn apply_actions(state: &AppState, out: &crate::TickOutput) {
    use nixfleet_reconciler::Action;

    let actions = match &out.verify {
        crate::VerifyOutcome::Ok(ok) => &ok.actions,
        crate::VerifyOutcome::Failed { .. } => return,
    };
    let Some(db) = state.db.as_ref() else {
        return;
    };
    for action in actions {
        match action {
            Action::SoakHost { rollout, host } => {
                match db.rollout_state().transition_host_state(
                    host,
                    rollout,
                    crate::state::HostRolloutState::Soaked,
                    crate::state::HealthyMarker::Untouched,
                    Some(crate::state::HostRolloutState::Healthy),
                ) {
                    Ok(0) => {
                        tracing::debug!(
                            target: "soak",
                            host = %host,
                            rollout = %rollout,
                            "soak: transition Healthy → Soaked no-op (host not in Healthy)",
                        );
                    }
                    Ok(_) => {
                        tracing::info!(
                            target: "soak",
                            host = %host,
                            rollout = %rollout,
                            "soak: host transitioned Healthy → Soaked",
                        );
                    }
                    Err(err) => {
                        tracing::warn!(
                            host = %host,
                            rollout = %rollout,
                            error = %err,
                            "soak: transition Healthy → Soaked failed",
                        );
                    }
                }
            }
            Action::ConvergeRollout { rollout } => {
                // Stamp every open `dispatch_history` row for this
                // rollout with `terminal_state = 'converged'`. The
                // operational `host_dispatch_state` rows are
                // intentionally left as 'confirmed' — they're the
                // host's current state and will be UPSERTed by the
                // next dispatch. No host_rollout_state cleanup
                // either; ConvergeRollout is idempotent and
                // re-emits each tick (handle_wave keeps reading
                // 'Soaked' rows as wave_all_soaked = true).
                match db
                    .dispatch_history()
                    .mark_rollout_converged(rollout, chrono::Utc::now())
                {
                    Ok(0) => {
                        // Already stamped by a prior tick — expected
                        // on every re-emission after the first.
                    }
                    Ok(n) => {
                        tracing::info!(
                            target: "converge",
                            rollout = %rollout,
                            history_rows_marked = n,
                            "converge: stamped dispatch_history terminal_state=converged",
                        );
                    }
                    Err(err) => {
                        tracing::warn!(
                            rollout = %rollout,
                            error = %err,
                            "converge: dispatch_history terminal stamp failed",
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

/// Run a tick using the in-memory projection rather than reading
/// `observed.json`. Mirrors `crate::tick` but takes the projected
/// `Observed` from the caller.
///
/// Returns both the tick output (for the journal plan) and the
/// verified `FleetResolved` (for the dispatch path's snapshot in
/// `AppState`). The fleet is `None` when the tick failed verify
/// the caller preserves whatever snapshot was previously in place.
fn run_tick_with_projection(
    inputs: &TickInputs,
    checkins: &HashMap<String, HostCheckinRecord>,
    channel_refs: &HashMap<String, String>,
    rollouts: &[crate::db::RolloutDbSnapshot],
    compliance_failures_by_rollout: HashMap<String, HashMap<String, usize>>,
) -> (anyhow::Result<crate::TickOutput>, Option<FleetResolved>) {
    use anyhow::Context;
    let artifact = match std::fs::read(&inputs.artifact_path)
        .with_context(|| format!("read artifact {}", inputs.artifact_path.display()))
    {
        Ok(b) => b,
        Err(e) => return (Err(e), None),
    };
    let signature = match std::fs::read(&inputs.signature_path)
        .with_context(|| format!("read signature {}", inputs.signature_path.display()))
    {
        Ok(b) => b,
        Err(e) => return (Err(e), None),
    };
    let (trusted_keys, reject_before) =
        match crate::polling::signed_fetch::read_trust_roots(&inputs.trust_path) {
            Ok(t) => t,
            Err(e) => return (Err(e), None),
        };

    let (verify, fleet) = match nixfleet_reconciler::verify_artifact(
        &artifact,
        &signature,
        &trusted_keys,
        inputs.now,
        inputs.freshness_window,
        reject_before,
    ) {
        Ok(fleet) => {
            let signed_at = match fleet.meta.signed_at {
                Some(ts) => ts,
                None => {
                    return (
                        Err(anyhow::anyhow!(
                            "verified artifact lacks meta.signedAt despite §4 contract — verify layer bug",
                        )),
                        None,
                    );
                }
            };
            let ci_commit = fleet.meta.ci_commit.clone();
            let observed = crate::observed_projection::project(
                checkins,
                channel_refs,
                rollouts,
                compliance_failures_by_rollout,
            );
            let actions = nixfleet_reconciler::reconcile(&fleet, &observed, inputs.now);
            (
                crate::VerifyOutcome::Ok(Box::new(crate::VerifyOk {
                    signed_at,
                    ci_commit,
                    observed,
                    actions,
                })),
                Some(fleet),
            )
        }
        Err(err) => (
            crate::VerifyOutcome::Failed {
                reason: format!("{:?}", err),
            },
            None,
        ),
    };

    (
        Ok(crate::TickOutput {
            now: inputs.now,
            verify,
        }),
        fleet,
    )
}

/// Verify-only variant for the empty-projection fallback path. The
/// caller runs the rest of the tick via `crate::tick` — this just
/// produces the verified fleet snapshot for `AppState.verified_fleet`.
/// Returns `None` when verify fails; the caller preserves the prior
/// snapshot.
pub(super) fn verify_fleet_only(inputs: &TickInputs) -> Option<FleetResolved> {
    let artifact = std::fs::read(&inputs.artifact_path).ok()?;
    let signature = std::fs::read(&inputs.signature_path).ok()?;
    let (trusted_keys, reject_before) =
        crate::polling::signed_fetch::read_trust_roots(&inputs.trust_path).ok()?;
    nixfleet_reconciler::verify_artifact(
        &artifact,
        &signature,
        &trusted_keys,
        inputs.now,
        inputs.freshness_window,
        reject_before,
    )
    .ok()
}
