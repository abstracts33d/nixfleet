//! Heartbeat worker: every 60s posts to `/v1/agent/heartbeat` with
//! `current_closure` + `uptime_secs` + `last_event_seq_by_rollout`.
//! Reads CP's `X-Nixfleet-Replay-From` response header; on drift the
//! intent is to walk the durable outbound queue from that seq and
//! re-POST the missed events.
//!
//! v0.2 scope: real POST loop only. The Replay-From walk-and-replay
//! is intentionally deferred — when CP signals drift today we log a
//! warning so operators see it. The durable queue (7d) and the
//! outbound drainer (7c) already give us crash-safe at-least-once
//! delivery for forward progress; Replay-From is a recovery
//! optimization for the case where CP lost state, which the
//! recovery handshake (7f) also covers on agent restart.

use std::time::Duration;

use nixfleet_proto::clock::ClockHandle;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::super::wire::{HeartbeatRequest, HeartbeatResponse};
use super::super::{AgentConfig, ReducerInput, ShutdownToken};

/// 60s heartbeat cadence. Plan 06 + RFC-0008 §4.3 — same window as the
/// long-poll's `wait` window so a stuck agent stops heartbeating within
/// roughly one polling interval.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

const ERROR_BACKOFF: Duration = Duration::from_secs(5);

pub fn spawn(
    cfg: AgentConfig,
    clock: ClockHandle,
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
                    target: "agent_heartbeat",
                    error = %err,
                    "failed to build mTLS HTTP client; worker exits",
                );
                return;
            }
        };
        let url = format!(
            "{}/v1/agent/heartbeat",
            cfg.control_plane_url.trim_end_matches('/'),
        );
        let mut ticker = tokio::time::interval(HEARTBEAT_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => {
                    tracing::info!(
                        target: "shutdown",
                        task = "agent_heartbeat",
                        "task shut down",
                    );
                    return;
                }
                _ = ticker.tick() => {
                    if let Err(err) = heartbeat_once(&client, &url, &cfg, &clock).await {
                        tracing::warn!(
                            target: "agent_heartbeat",
                            error = %err,
                            "heartbeat POST failed; backing off",
                        );
                        tokio::time::sleep(ERROR_BACKOFF).await;
                    }
                }
            }
        }
    })
}

async fn heartbeat_once(
    client: &reqwest::Client,
    url: &str,
    cfg: &AgentConfig,
    clock: &ClockHandle,
) -> anyhow::Result<()> {
    // For 7c the heartbeat carries the agent's identity + current
    // wallclock; the `current_closure` + `rollout_id` payload that the
    // reducer maintains land in 7f's boot-recovery wiring (the
    // reducer publishes them via a shared snapshot the heartbeat
    // worker reads on each tick).
    let req = HeartbeatRequest {
        hostname: cfg.machine_id.clone(),
        rollout_id: None,
        current_closure: None,
        at: clock.now(),
    };
    // Per-request timeout override: comms::build_client uses a 30s
    // default; heartbeat insists on a tighter 10s fail-fast.
    let resp = client
        .post(url)
        .timeout(HTTP_TIMEOUT)
        .json(&req)
        .send()
        .await?;
    let status = resp.status();

    let replay_from = resp
        .headers()
        .get("X-Nixfleet-Replay-From")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());

    if !status.is_success() {
        anyhow::bail!("CP returned {status}");
    }
    let _body: HeartbeatResponse = resp.json().await?;

    if let Some(seq) = replay_from {
        // v0.2 deferred: walk-and-replay from durable queue. Logged so
        // operators see drift; recovery handshake (7f) covers the
        // forward path on agent restart.
        tracing::warn!(
            target: "agent_heartbeat",
            replay_from = seq,
            "CP signaled Replay-From (walk-and-replay deferred for v0.2)",
        );
    }
    Ok(())
}
