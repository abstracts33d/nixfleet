//! 30s reconcile loop; freshness gate prevents stale build-time bytes clobbering upstream-fresh snapshot.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use nixfleet_proto::FleetResolved;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::{render_plan, tick, TickInputs};

use super::state::{AppState, HostCheckinRecord, RECONCILE_INTERVAL};

pub(super) fn spawn_reconcile_loop(
    cancel: CancellationToken,
    state: Arc<AppState>,
    inputs: TickInputs,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Build-time artifact is the fallback prime; never overwrite an already-primed upstream-fresh snapshot.
        {
            let already_primed = state.verified_fleet.read().await.is_some();
            if !already_primed {
                let prime_inputs = TickInputs {
                    now: Utc::now(),
                    ..inputs.clone()
                };
                if let Some(fleet) = verify_fleet_only(&prime_inputs) {
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
                    "verified-fleet snapshot already populated; skipping build-time prime",
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

            let channel_refs = {
                let cache = state.channel_refs_cache.read().await;
                cache.refs.clone()
            };
            let checkins = state.host_checkins.read().await.clone();

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

            // Empty projection falls back to file-backed observed.json (deploy-without-agents path).
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

            // LOADBEARING: single write-lock atomic swap — dispatch readers
            // can never see a half-built snapshot. Compare signed_at (not
            // wall clock) so an out-of-order tick doesn't downgrade fresh state.
            if let Some(fleet) = verified_fleet {
                let mut guard = state.verified_fleet.write().await;
                let should_overwrite = match guard.as_ref() {
                    None => true,
                    Some(existing) => {
                        match (existing.fleet.meta.signed_at, fleet.meta.signed_at) {
                            (Some(prev), Some(new)) => new >= prev,
                            _ => true,
                        }
                    }
                };
                if should_overwrite {
                    if let Ok(h) = nixfleet_reconciler::compute_canonical_hash(&fleet) {
                        *guard = Some(crate::server::VerifiedFleetSnapshot {
                            fleet: Arc::new(fleet),
                            fleet_resolved_hash: h,
                        });
                    }
                }
            }

            match result {
                Ok(mut out) => {
                    // The verify result above came from re-reading the static
                    // boot artifact at `inputs.artifact_path`. Dispatch decisions
                    // already operate on the live `verified_fleet` cache (kept
                    // fresh by the channel-refs poll), so the log line should
                    // reflect the same freshness — otherwise `ci_commit` and
                    // `signed_at` lag behind reality until the CP itself is
                    // restarted onto a closure containing the new artifact.
                    if let crate::VerifyOutcome::Ok(ok) = &mut out.verify {
                        if let Some(snapshot) = state.verified_fleet.read().await.as_ref() {
                            if let Some(snap_signed_at) = snapshot.fleet.meta.signed_at {
                                if snap_signed_at >= ok.signed_at {
                                    ok.signed_at = snap_signed_at;
                                    ok.ci_commit = snapshot.fleet.meta.ci_commit.clone();
                                }
                            }
                        }
                    }
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

/// At-least-once action handler; SoakHost + ConvergeRollout mutate DB, others are journal-only.
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
                match db
                    .dispatch_history()
                    .mark_rollout_converged(rollout, chrono::Utc::now())
                {
                    Ok(0) => {}
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

/// Returns `(tick_output, fleet)`; fleet `None` on verify failure so caller preserves prior snapshot.
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

/// `None` on verify failure → caller preserves prior snapshot.
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
