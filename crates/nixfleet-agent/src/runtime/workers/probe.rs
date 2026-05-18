//! Probe worker (RFC-0010 §8). On each `LocalResetProbeCache`:
//!
//! 1. Aborts every per-probe ticker spawned for the previous rollout
//!    (the `JoinHandle::abort` invariant: no probe ticker from rollout
//!    N runs after `LocalResetProbeCache { rollout_id: N }` is observed).
//! 2. Reads `/etc/nixfleet/agent/health-checks.json` (rendered by the
//!    host's NixOS module from the mkFleet-resolved effective set —
//!    closure-driven, transitively signed via the closure hash chain
//!    per RFC-0010 §4). Path is hardcoded; no `--health-checks-config`
//!    flag.
//! 3. Emits one `LocalProbeTopologyDeclared` event into the reducer
//!    input MPSC, then spawns one ticker per probe with
//!    `mode != "disabled"`.
//!
//! Each ticker fires every `intervalSeconds` (or once, if
//! `runOnce = true`). On each tick it dispatches to the kind-specific
//! `probe_runners::run` and emits `LocalProbeResult`. The ticker holds
//! its own first-observation + first-failure flags to drive the
//! `LocalProbeObservedFirst` / `LocalProbeFailureFirst` events RFC-0008
//! §3.2 requires.
//!
//! Test-mode escape hatch: `NIXFLEET_AGENT_PROBE_TEST_MODE=1` skips
//! reading the JSON file and the spawn loop — the smoke test sets it
//! so the runtime can boot without a /etc/ file or actual probes.

use std::collections::HashMap;
use std::path::PathBuf;

use nixfleet_proto::clock::ClockHandle;
use nixfleet_state_machine::{Event, ProbeMode, ProbeStatus, ProbeTopologyEntry};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::super::wire::ProbeResetCommand;
use super::super::{AgentConfig, ReducerInput, ShutdownToken};
use super::probe_runners::{self, ProbeDecl};

const HEALTH_CHECKS_PATH: &str = "/etc/nixfleet/agent/health-checks.json";

pub fn spawn(
    _cfg: AgentConfig,
    clock: ClockHandle,
    input_tx: mpsc::Sender<ReducerInput>,
    mut reset_rx: mpsc::Receiver<ProbeResetCommand>,
    shutdown: ShutdownToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut shutdown_rx = shutdown.into_inner();
        // Per-probe ticker handles for the CURRENT rollout. On reset we
        // .abort() all of them — RFC-0010 §6 invariant.
        let mut tickers: HashMap<String, JoinHandle<()>> = HashMap::new();
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => {
                    abort_all(&mut tickers);
                    tracing::info!(
                        target: "shutdown",
                        task = "agent_probe",
                        "task shut down",
                    );
                    return;
                }
                maybe = reset_rx.recv() => {
                    let Some(cmd) = maybe else {
                        abort_all(&mut tickers);
                        return;
                    };
                    handle_reset(
                        cmd,
                        &mut tickers,
                        &input_tx,
                        &clock,
                    ).await;
                }
            }
        }
    })
}

fn abort_all(tickers: &mut HashMap<String, JoinHandle<()>>) {
    for (_, h) in tickers.drain() {
        h.abort();
    }
}

async fn handle_reset(
    cmd: ProbeResetCommand,
    tickers: &mut HashMap<String, JoinHandle<()>>,
    input_tx: &mpsc::Sender<ReducerInput>,
    clock: &ClockHandle,
) {
    abort_all(tickers);
    let rollout_id = cmd.rollout_id;
    tracing::info!(
        target: "agent_probe",
        %rollout_id,
        "probe cache reset; reloading declarations",
    );

    if std::env::var("NIXFLEET_AGENT_PROBE_TEST_MODE").is_ok() {
        tracing::info!(
            target: "agent_probe",
            "test mode: skipping probe declaration read + ticker spawn",
        );
        return;
    }

    let path = PathBuf::from(HEALTH_CHECKS_PATH);
    let raw = match tokio::fs::read_to_string(&path).await {
        Ok(s) => s,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!(
                target: "agent_probe",
                path = %path.display(),
                "no probe declarations file present; running with empty probe set",
            );
            return;
        }
        Err(err) => {
            tracing::warn!(
                target: "agent_probe",
                path = %path.display(),
                error = %err,
                "failed to read probe declarations; running with empty probe set",
            );
            return;
        }
    };
    let decls: HashMap<String, ProbeDecl> = match serde_json::from_str(&raw) {
        Ok(d) => d,
        Err(err) => {
            tracing::warn!(
                target: "agent_probe",
                error = %err,
                "failed to parse probe declarations; running with empty probe set",
            );
            return;
        }
    };

    // Emit topology declaration BEFORE spawning tickers so the CP's
    // event_log has the authoritative declared-probe set on record
    // before any per-probe result lands (RFC-0010 §8).
    let topology: Vec<ProbeTopologyEntry> = decls
        .iter()
        .map(|(name, d)| ProbeTopologyEntry {
            probe_name: name.clone(),
            kind: d.kind.clone(),
            mode: parse_mode(&d.mode),
        })
        .collect();
    if input_tx
        .send(ReducerInput::HostEvent {
            rollout_id: rollout_id.clone(),
            event: Event::LocalProbeTopologyDeclared {
                probes: topology,
                declared_at: clock.now(),
                seq: 0,
            },
        })
        .await
        .is_err()
    {
        tracing::warn!(
            target: "agent_probe",
            "reducer input channel closed; aborting reset",
        );
        return;
    }

    for (name, decl) in decls.into_iter() {
        let mode = parse_mode(&decl.mode);
        if matches!(mode, ProbeMode::Disabled) {
            continue;
        }
        let handle = spawn_ticker(
            name.clone(),
            decl,
            mode,
            rollout_id.clone(),
            input_tx.clone(),
            clock.clone(),
        );
        tickers.insert(name, handle);
    }
}

fn parse_mode(s: &str) -> ProbeMode {
    match s {
        "enforce" => ProbeMode::Enforce,
        "observe" => ProbeMode::Observe,
        "disabled" => ProbeMode::Disabled,
        // Honest fail-closed: unknown mode treats the probe as gating
        // (enforce). Operator typo is loud, not silent.
        _ => ProbeMode::Enforce,
    }
}

fn spawn_ticker(
    name: String,
    decl: ProbeDecl,
    mode: ProbeMode,
    rollout_id: nixfleet_proto::RolloutId,
    input_tx: mpsc::Sender<ReducerInput>,
    clock: ClockHandle,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        // LOADBEARING: floor probe interval at MIN_INTERVAL_SECS (5s)
        // — guards against a misconfigured 0/1-second probe DOSing
        // the host.
        let interval = std::time::Duration::from_secs(
            decl.interval_seconds
                .max(super::probe_runners::MIN_INTERVAL_SECS),
        );
        let mut first_observed = false;
        let mut last_status: Option<ProbeStatus> = None;
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let outcome = probe_runners::run(&decl, clock.now()).await;
            let status = outcome.status;
            let observed_at = outcome.observed_at;

            if !first_observed {
                first_observed = true;
                let _ = input_tx
                    .send(ReducerInput::HostEvent {
                        rollout_id: rollout_id.clone(),
                        event: Event::LocalProbeObservedFirst {
                            probe_name: name.clone(),
                            mode,
                            observed_at,
                            seq: 0,
                        },
                    })
                    .await;
            }
            let was_fail = matches!(last_status, Some(ProbeStatus::Fail));
            let now_fail = matches!(status, ProbeStatus::Fail);
            if now_fail && !was_fail {
                let _ = input_tx
                    .send(ReducerInput::HostEvent {
                        rollout_id: rollout_id.clone(),
                        event: Event::LocalProbeFailureFirst {
                            probe_name: name.clone(),
                            mode,
                            first_failed_at: observed_at,
                            seq: 0,
                        },
                    })
                    .await;
            }
            last_status = Some(status);

            // Per-tick ProbeResult. Carries sub_results for evidence kind.
            let _ = input_tx
                .send(ReducerInput::HostEvent {
                    rollout_id: rollout_id.clone(),
                    event: Event::LocalProbeResult {
                        probe_name: name.clone(),
                        mode,
                        status,
                        observed_at,
                        failure_reason: outcome.failure_reason,
                        seq: 0,
                    },
                })
                .await;
            // sub_results are dropped between event and effect for now;
            // they're attached in the applier when emitting the
            // OutboundAgentEvent::ProbeResult. The reducer's event
            // shape doesn't carry sub_results (the gate doesn't need
            // them — it only consults aggregate status). 9b's applier
            // co-write reads sub_results off the OutboundAgentEvent
            // payload, not the inbound event.
            //
            // TODO(v0.2.1): plumb sub_results through the agent reducer
            // so the applier can stamp them onto the outbound payload
            // without a side-channel. For 9b they only land on the
            // CP-side ProbeResult event via the wire serde path; the
            // agent's local reducer doesn't need them.
            let _ = &outcome.sub_results;

            if decl.run_once {
                return;
            }
        }
    })
}
