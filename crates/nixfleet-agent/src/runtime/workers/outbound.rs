//! Outbound queue drainer. Periodically scans
//! `{state_dir}/outbound-queue/` and POSTs each pending event to
//! `/v1/agent/events`, deleting on success. Acts as the network-side
//! companion to the applier's `LocalEmitEvent` writes.
//!
//! On CP unreachable / network failure the file stays on disk; the
//! next drain tick retries. The heartbeat worker's
//! `X-Nixfleet-Replay-From` handler triggers an immediate drain pass
//! by sending into the [`DrainKick`] channel.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use super::super::outbound_queue::{OutboundQueue, QueuedEvent};
use super::super::{AgentConfig, ReducerInput, ShutdownToken};

/// Outer cadence. Plan 07's locked-in decision keeps the drain
/// hot-loop-free: a single tick per N seconds, kicked by the
/// `DrainKick` channel when something new lands or when the
/// heartbeat worker observed CP drift.
const DRAIN_INTERVAL: Duration = Duration::from_secs(15);

/// HTTP timeout per POST. CP returns 204 fast (in-memory dispatch
/// channel push); 10s is generous slack.
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

pub type DrainKick = watch::Sender<()>;

pub fn spawn(
    cfg: AgentConfig,
    queue: Arc<OutboundQueue>,
    mut kick_rx: watch::Receiver<()>,
    _input_tx: mpsc::Sender<ReducerInput>,
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
                    target: "agent_outbound",
                    error = %err,
                    "failed to build mTLS HTTP client; worker exits",
                );
                return;
            }
        };
        let url = format!(
            "{}/v1/agent/events",
            cfg.control_plane_url.trim_end_matches('/'),
        );
        let mut ticker = tokio::time::interval(DRAIN_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => {
                    tracing::info!(
                        target: "shutdown",
                        task = "agent_outbound",
                        "task shut down",
                    );
                    return;
                }
                _ = ticker.tick() => {
                    drain_once(&client, &url, &queue).await;
                }
                changed = kick_rx.changed() => {
                    if changed.is_err() {
                        // All senders dropped — runtime tearing down.
                        return;
                    }
                    drain_once(&client, &url, &queue).await;
                }
            }
        }
    })
}

async fn drain_once(client: &reqwest::Client, url: &str, queue: &OutboundQueue) {
    let pending = match queue.scan_pending() {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!(
                target: "agent_outbound",
                error = %err,
                "scan_pending failed; will retry next tick",
            );
            return;
        }
    };
    for event in pending {
        if let Err(err) = post_one(client, url, &event).await {
            tracing::warn!(
                target: "agent_outbound",
                seq = event.seq,
                kind = %event.event_kind,
                error = %err,
                "POST /v1/agent/events failed; leaving on disk for retry",
            );
            // Stop on first failure — CP is likely unreachable; later
            // events in the batch will fail for the same reason.
            return;
        }
        if let Err(err) = queue.mark_sent(&event) {
            tracing::warn!(
                target: "agent_outbound",
                seq = event.seq,
                error = %err,
                "mark_sent failed (file remove); will retry — POST is idempotent at CP via (host, rollout, seq) dedup",
            );
        }
    }
}

async fn post_one(client: &reqwest::Client, url: &str, event: &QueuedEvent) -> anyhow::Result<()> {
    // Construct the typed wire envelope. Both sides import the same
    // type from `nixfleet-proto`, so the camelCase / snake_case
    // discriminator drift that previously bit `rollout_id` vs
    // `rolloutId` is now resolved at the serde-derive layer rather
    // than at a hand-built JSON map.
    let envelope = nixfleet_proto::AgentEventEnvelope {
        hostname: event.hostname.clone(),
        rollout_id: event.rollout_id.clone(),
        event: event.payload.clone(),
        signature: None,
    };
    // Per-request timeout override: comms::build_client uses 30s default;
    // outbound POSTs to /v1/agent/events insist on the 10s fail-fast cap.
    let resp = client
        .post(url)
        .timeout(HTTP_TIMEOUT)
        .json(&envelope)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("CP returned {status}");
    }
    Ok(())
}
