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
                    if let Err(err) = heartbeat_once(&client, &url, &cfg, &clock, &input_tx).await {
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
    input_tx: &mpsc::Sender<ReducerInput>,
) -> anyhow::Result<()> {
    // LIFT #5: read /run/current-system on each heartbeat. The agent's
    // observed closure is the load-bearing soft-state input the CP
    // needs to rebuild `host_rollout_records` from agent traffic alone
    // after a wipe-restart (architecture.md §305 acceptance gate 1).
    // `rollout_id` stays None: that keeps the heartbeat in the
    // boot-recovery shape so CP's LIFT #1 + LIFT #5 synthesis fires
    // when it sees `current_closure == record.target_closure`.
    let current_closure = crate::runtime::recovery::read_current_closure(&cfg.current_system_path);
    let req = HeartbeatRequest {
        hostname: cfg.machine_id.clone(),
        rollout_id: None,
        current_closure,
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
    let body: HeartbeatResponse = resp.json().await?;

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

    // LIFT #4: forward CP-supplied bootstrap snapshots into the reducer.
    // CP fills `bootstrap_rollouts` on every heartbeat where the agent
    // posted `rollout_id=None` (see CP reducer build_bootstrap_for_host).
    // The steady-state heartbeat currently posts None unconditionally,
    // so this path catches the case where the agent's in-memory cache
    // got out of sync with CP between boot-recovery and now (e.g.
    // LIFT #1 heartbeat synthesis completed post-recovery on CP). The
    // reducer routes the resulting LocalResetProbeCache effects so
    // probe runners re-spawn against the bootstrapped rollout_id
    // instead of holding tickers from a prior agent process.
    if !body.bootstrap_rollouts.is_empty() {
        tracing::info!(
            target: "agent_heartbeat",
            count = body.bootstrap_rollouts.len(),
            "heartbeat: CP returned bootstrap snapshots; forwarding to reducer",
        );
        for snapshot in body.bootstrap_rollouts {
            if let Err(err) = input_tx
                .send(ReducerInput::BootstrapHost(Box::new(snapshot)))
                .await
            {
                tracing::warn!(
                    target: "agent_heartbeat",
                    error = %err,
                    "bootstrap-snapshot forward failed (reducer not ready?); skipping",
                );
            }
        }
    }
    Ok(())
}
