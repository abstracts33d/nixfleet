//! Long-poll worker: holds an open `GET /v1/agent/dispatch?wait=60`
//! against the CP. On a 200 with a Dispatch body, fetches + verifies
//! the rollout manifest via the disk-backed [`ManifestCache`], asserts
//! the dispatched `target_closure` matches the manifest's declaration
//! for this host (RFC-0008 §4.1 advisory-payload contract), then emits
//! `LocalActivate` into the reducer.
//!
//! First dispatch per rollout fetches `/v1/rollouts/{id}` from CP,
//! verifies, writes through to disk. Repeat dispatches hit disk +
//! re-verify (defense in depth). Disk cache + on-demand fetch is the
//! model; no in-memory pre-priming step, no periodic-refresh worker.

use std::time::Duration;

use nixfleet_proto::clock::ClockHandle;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::super::wire::DispatchResponse;
use super::super::{AgentConfig, ReducerInput, ShutdownToken};
use crate::manifest_cache::ManifestCache;

/// Per-poll wait window. Plan 06's locked-in value; matches CP's
/// `dispatch::MAX_WAIT_SECS`.
const WAIT_SECS: u64 = 60;

/// HTTP timeout = wait + 5s slack to cover request setup + response.
const HTTP_TIMEOUT: Duration = Duration::from_secs(WAIT_SECS + 5);

/// Per-error backoff floor. Real network failures should NOT hot-loop;
/// 5s gives the CP a chance to recover (e.g. mid-rollout deploy).
const ERROR_BACKOFF: Duration = Duration::from_secs(5);

pub fn spawn(
    cfg: AgentConfig,
    _clock: ClockHandle,
    input_tx: mpsc::Sender<ReducerInput>,
    shutdown: ShutdownToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut shutdown_rx = shutdown.into_inner();
        let client = match crate::comms::build_client(
            cfg.ca_cert.as_deref(),
            cfg.client_cert.as_deref(),
            cfg.client_key.as_deref(),
        ) {
            Ok(c) => c,
            Err(err) => {
                tracing::error!(
                    target: "agent_longpoll",
                    error = %err,
                    "failed to build mTLS HTTP client; worker exits",
                );
                return;
            }
        };
        let manifest_cache = ManifestCache::new_default(&cfg.state_dir);
        let url = format!(
            "{}/v1/agent/dispatch?wait={}",
            cfg.control_plane_url.trim_end_matches('/'),
            WAIT_SECS,
        );

        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => {
                    tracing::info!(
                        target: "shutdown",
                        task = "agent_longpoll",
                        "task shut down",
                    );
                    return;
                }
                res = poll_once(&client, &url) => {
                    match res {
                        Ok(Some(dispatch)) => {
                            if let Err(err) = handle_dispatch(&cfg, &client, &manifest_cache, &input_tx, dispatch).await {
                                tracing::warn!(
                                    target: "agent_longpoll",
                                    error = %err,
                                    "handle_dispatch failed; backing off",
                                );
                                tokio::time::sleep(ERROR_BACKOFF).await;
                            }
                        }
                        Ok(None) => {
                            // 204 / empty response: no work queued. Loop
                            // immediately into the next poll; the CP's
                            // `wait=60s` cap is the rate-limiter.
                        }
                        Err(err) => {
                            tracing::warn!(
                                target: "agent_longpoll",
                                error = %err,
                                "long-poll request failed; backing off",
                            );
                            tokio::time::sleep(ERROR_BACKOFF).await;
                        }
                    }
                }
            }
        }
    })
}

async fn poll_once(
    client: &reqwest::Client,
    url: &str,
) -> anyhow::Result<Option<DispatchResponse>> {
    // Per-request timeout override: long-poll waits up to 60s server-
    // side; the request needs slack on top of that. `comms::build_client`'s
    // 30s default would cut the long-poll off before CP could reply.
    let resp = client.get(url).timeout(HTTP_TIMEOUT).send().await?;
    let status = resp.status();
    if status == reqwest::StatusCode::NO_CONTENT {
        return Ok(None);
    }
    if !status.is_success() {
        anyhow::bail!("CP returned {status}");
    }
    // Some endpoints return 200 with a null body when no work; treat
    // any non-deserializable / null response as "no work pending"
    // rather than an error.
    let body = resp.text().await?;
    let trimmed = body.trim();
    if trimmed.is_empty() || trimmed == "null" {
        return Ok(None);
    }
    let parsed: serde_json::Value = serde_json::from_str(trimmed)?;
    if parsed.is_null() {
        return Ok(None);
    }
    let dispatch: DispatchResponse = serde_json::from_value(parsed)?;
    Ok(Some(dispatch))
}

async fn handle_dispatch(
    cfg: &AgentConfig,
    client: &reqwest::Client,
    manifest_cache: &ManifestCache,
    input_tx: &mpsc::Sender<ReducerInput>,
    dispatch: DispatchResponse,
) -> anyhow::Result<()> {
    // Hostname guard: CP can only queue dispatches for this agent, but
    // a misconfigured CP or a rogue intermediate could send the wrong
    // payload. Refuse early.
    if dispatch.hostname != cfg.machine_id {
        anyhow::bail!(
            "dispatch hostname={} != agent machine_id={}",
            dispatch.hostname,
            cfg.machine_id,
        );
    }

    // RFC-0008 §4.1 advisory-payload contract enforced by the
    // ManifestCache: fetches the rollout's signed manifest (cache hit
    // re-verifies disk bytes; miss fetches from CP, verifies, writes
    // through), then asserts the dispatched target_closure matches the
    // manifest's declaration for this host. Any divergence aborts the
    // dispatch before it reaches the reducer.
    let verified = manifest_cache
        .ensure_for_dispatch(
            client,
            &cfg.control_plane_url,
            dispatch.rollout_id.as_str(),
            &cfg.machine_id,
            &dispatch.target_closure,
        )
        .await
        .map_err(|err| {
            anyhow::anyhow!(
                "manifest verify (rollout={}): {}",
                dispatch.rollout_id,
                err.reason()
            )
        })?;

    // D-028: defense-in-depth freshness recheck at dispatch reception time.
    // ensure_for_dispatch's verify_artifact already enforces freshness at
    // fetch time, but a cache hit close to the freshness edge can cross the
    // boundary between fetch and dispatch arrival. Refuse stale dispatches
    // here so the agent doesn't fire a switch against a manifest that has
    // aged out — CP's planner will re-emit with a fresher manifest if CI
    // has signed one. The freshness window matches the hardcoded 3600s the
    // cache verify uses; channel-level operator-declared windows are a
    // v0.2.1 followup (currently the cache layer hardcodes 3600s too, so
    // matching values here keeps the two checks in lockstep).
    let now = chrono::Utc::now();
    match crate::freshness::check_signed_at(verified.signed_at(), 3600, now) {
        crate::freshness::FreshnessCheck::Fresh => {}
        crate::freshness::FreshnessCheck::Stale {
            signed_at,
            freshness_window_secs,
            age_secs,
        } => {
            anyhow::bail!(
                "dispatch refused: per-rollout manifest stale at dispatch time \
                 (rollout={}, signed_at={signed_at}, window={freshness_window_secs}s, age={age_secs}s)",
                dispatch.rollout_id,
            );
        }
    }

    let event = nixfleet_state_machine::Event::LocalActivate {
        current_closure_at_dispatch: dispatch.target_closure.clone(),
        // Validated dispatch target carried verbatim (D-024). The
        // ensure_for_dispatch call above asserted this value matches
        // the freshly-verified per-rollout manifest's declared
        // target_closure for this host; the reducer's bootstrap
        // consumes it without re-derivation (eliminates the TOCTOU
        // against the reducer's stale manifests snapshot).
        target_closure: dispatch.target_closure.clone(),
        received_at: dispatch.enqueued_at,
        // CP-resolved soak deadline (RFC-0011 §1 invariant 1 single
        // source of truth). Pre-D-019 the agent reducer hardcoded
        // `now + 5min` here, diverging from CP's policy-resolved value.
        soak_due_at: dispatch.soak_due_at,
        seq: 0, // reducer assigns
    };
    input_tx
        .send(ReducerInput::HostEvent {
            rollout_id: dispatch.rollout_id,
            event,
        })
        .await?;
    Ok(())
}
