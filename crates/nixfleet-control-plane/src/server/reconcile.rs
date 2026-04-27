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

use crate::{render_plan, tick, TickInputs};

use super::state::{AppState, HostCheckinRecord, RECONCILE_INTERVAL};

/// Spawn the reconcile loop. Each tick:
/// 1. Reads the channel-refs cache (refreshed by the Forgejo poll
///    task; falls back to file-backed observed.json when empty).
/// 2. Builds an `Observed` from the in-memory checkin state +
///    cached channel-refs.
/// 3. Verifies the resolved artifact and reconciles against the
///    projected `Observed`.
/// 4. Emits the plan via tracing.
///
/// Errors at any step are logged and fall through; the loop never
/// crashes on transient failures.
pub(super) fn spawn_reconcile_loop(state: Arc<AppState>, inputs: TickInputs) {
    tokio::spawn(async move {
        // Prime the verified-fleet snapshot from the build-time
        // artifact, IF it isn't already populated. The Forgejo
        // prime in `serve()` runs first and sets it from the
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
                    *state.verified_fleet.write().await = Some(Arc::new(fleet));
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

        let mut ticker = tokio::time::interval_at(
            Instant::now() + RECONCILE_INTERVAL,
            RECONCILE_INTERVAL,
        );
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            ticker.tick().await;
            let now = Utc::now();

            // Snapshot the cache + checkins under read locks. Drop
            // the locks before doing the (potentially slow) verify +
            // reconcile work.
            let channel_refs = {
                let cache = state.channel_refs_cache.read().await;
                cache.refs.clone()
            };
            let checkins = state.host_checkins.read().await.clone();

            // Live projection: in-memory checkins + cached channel-refs.
            // When the Forgejo poll hasn't succeeded yet AND no agents
            // have checked in, fall back to the file-backed
            // observed.json so the deploy-without-agents path keeps
            // working.
            let inputs_now = TickInputs {
                now,
                ..inputs.clone()
            };
            let (result, verified_fleet) = if checkins.is_empty() && channel_refs.is_empty() {
                (tick(&inputs_now), verify_fleet_only(&inputs_now))
            } else {
                run_tick_with_projection(&inputs_now, &checkins, &channel_refs)
            };

            // Snapshot the verified fleet so the dispatch path can
            // read it. Three preserve rules layered on top:
            //
            // 1. Verify-failed tick → preserve previous snapshot.
            // 2. The build-time artifact path is immutable for the
            //    CP's lifetime, so the bytes verify_fleet_only re-
            //    reads here are the SAME every tick.
            // 3. Compare `signed_at`: only overwrite if the new
            //    snapshot is at least as fresh as what's already
            //    there. Keeps the Forgejo-fresh snapshot from being
            //    clobbered.
            if let Some(fleet) = verified_fleet {
                let mut guard = state.verified_fleet.write().await;
                let should_overwrite = match guard.as_ref() {
                    None => true,
                    Some(existing) => match (existing.meta.signed_at, fleet.meta.signed_at) {
                        (Some(prev), Some(new)) => new >= prev,
                        // Either lacks signed_at (shouldn't happen for
                        // verified artifacts) → fall back to overwriting.
                        _ => true,
                    },
                };
                if should_overwrite {
                    *guard = Some(Arc::new(fleet));
                }
            }

            match result {
                Ok(out) => {
                    let plan = render_plan(&out);
                    tracing::info!(target: "reconcile", "{}", plan.trim_end());
                }
                Err(err) => {
                    tracing::warn!(error = %err, "reconcile tick failed");
                }
            }
            *state.last_tick_at.write().await = Some(now);
        }
    });
}

/// Run a tick using the in-memory projection rather than reading
/// `observed.json`. Mirrors `crate::tick` but takes the projected
/// `Observed` from the caller.
///
/// Returns both the tick output (for the journal plan) and the
/// verified `FleetResolved` (for the dispatch path's snapshot in
/// `AppState`). The fleet is `None` when the tick failed verify —
/// the caller preserves whatever snapshot was previously in place.
fn run_tick_with_projection(
    inputs: &TickInputs,
    checkins: &HashMap<String, HostCheckinRecord>,
    channel_refs: &HashMap<String, String>,
) -> (anyhow::Result<crate::TickOutput>, Option<FleetResolved>) {
    use anyhow::Context;
    let read_inputs = || -> anyhow::Result<(Vec<u8>, Vec<u8>, nixfleet_proto::TrustConfig)> {
        let artifact = std::fs::read(&inputs.artifact_path)
            .with_context(|| format!("read artifact {}", inputs.artifact_path.display()))?;
        let signature = std::fs::read(&inputs.signature_path)
            .with_context(|| format!("read signature {}", inputs.signature_path.display()))?;
        let trust_raw = std::fs::read_to_string(&inputs.trust_path)
            .with_context(|| format!("read trust {}", inputs.trust_path.display()))?;
        let trust: nixfleet_proto::TrustConfig =
            serde_json::from_str(&trust_raw).context("parse trust")?;
        Ok((artifact, signature, trust))
    };

    let (artifact, signature, trust) = match read_inputs() {
        Ok(t) => t,
        Err(e) => return (Err(e), None),
    };

    let trusted_keys = trust.ci_release_key.active_keys();
    let reject_before = trust.ci_release_key.reject_before;

    let (verify, fleet) = match nixfleet_reconciler::verify_artifact(
        &artifact,
        &signature,
        &trusted_keys,
        inputs.now,
        inputs.freshness_window,
        reject_before,
    ) {
        Ok(fleet) => {
            let signed_at = fleet.meta.signed_at.expect("verified artifact carries meta.signedAt");
            let ci_commit = fleet.meta.ci_commit.clone();
            let observed = crate::observed_projection::project(checkins, channel_refs);
            let actions = nixfleet_reconciler::reconcile(&fleet, &observed, inputs.now);
            (
                crate::VerifyOutcome::Ok {
                    signed_at,
                    ci_commit,
                    observed,
                    actions,
                },
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
    let trust_raw = std::fs::read_to_string(&inputs.trust_path).ok()?;
    let trust: nixfleet_proto::TrustConfig = serde_json::from_str(&trust_raw).ok()?;
    nixfleet_reconciler::verify_artifact(
        &artifact,
        &signature,
        &trust.ci_release_key.active_keys(),
        inputs.now,
        inputs.freshness_window,
        trust.ci_release_key.reject_before,
    )
    .ok()
}
