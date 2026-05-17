//! Boot-recovery handshake (RFC-0008 §9.5 / Plan 07 open-question
//! resolution).
//!
//! Runs ONCE at runtime startup, before any worker is allowed to fire.
//! Goal: synchronise the (zero-in-memory) agent reducer with whatever
//! state CP holds, so a crash that drops in-memory state doesn't lead
//! the agent to either re-fire a switch that already took or skip a
//! switch that didn't.
//!
//! Five scenarios, all distinguished by what `/run/current-system`
//! points to vs what CP's record says:
//!
//!   1. Fresh install. /run/current-system = X, CP has no rollout
//!      record. First heartbeat reports current_closure=X; CP just
//!      records "agent present"; next Dispatch starts normal flow.
//!
//!   2. Crashed mid-Pending (before LocalActivate fired).
//!      /run/current-system = prior closure (switch never started).
//!      First heartbeat reports prior. CP's Pending record's
//!      current_closure_at_dispatch matches; CP re-queues Dispatch.
//!      Long-poll picks up; normal flow.
//!
//!   3. Crashed mid-Activating. /run/current-system = either prior OR
//!      target depending on whether switch-to-configuration had
//!      committed the new generation yet.
//!        - If target: switch took, agent crashed before posting
//!          ActivationCompleted. CP synthesises an
//!          ActivationCompleted-shaped Replay-From event; agent's
//!          reducer applies it as if it had arrived normally; probes
//!          start fresh; soak proceeds.
//!        - If prior: switch didn't take (or had only just started).
//!          CP treats as scenario 2.
//!
//!   4. Crashed mid-Soaking. /run/current-system = target. CP sees
//!      Soaking record's target matches; replies with a stream of
//!      synthesised LocalProbeResult events the reducer needs to
//!      know prior probe state (so the Fail→Pass transition detector
//!      doesn't double-count). Probes resume; soak completes;
//!      Converged fires.
//!
//!   5. Crashed post-Converged. /run/current-system = target. CP sees
//!      Converged record; nothing to do; wait for next Dispatch.
//!
//! For 7f, the agent issues the first heartbeat and reads the CP's
//! `X-Nixfleet-Replay-From` header. Full synthesised-event
//! reconstruction (scenario 3's ActivationCompleted synthesis, scenario
//! 4's probe-result stream) is documented here but the rich synthesis
//! lands in a follow-up — the architecture is in place; the wiring
//! needs CP-side route changes that aren't in scope this commit.
//! Until then, a non-zero `Replay-From` triggers a warn log; operator
//! can use `nixfleet trace` to inspect the in-flight rollout.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use nixfleet_proto::clock::ClockHandle;
use serde::{Deserialize, Serialize};

use super::wire::{HeartbeatRequest, HeartbeatResponse};

/// HTTP timeout for the boot-recovery heartbeat. Longer than the
/// steady-state heartbeat because boot recovery may run while CP is
/// itself starting up — 30s of slack covers normal CP cold-start.
const RECOVERY_HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Outcome of the boot-recovery handshake. Returned to `runtime::spawn`
/// so the caller can decide whether to feed synthesised events through
/// the reducer before workers start.
#[derive(Debug, Clone)]
pub struct RecoveryOutcome {
    pub current_closure: Option<String>,
    pub replay_from: Option<u64>,
    pub heartbeat_sent_at: DateTime<Utc>,
}

/// Best-effort read of `/run/current-system`'s store-path basename.
/// Returns `None` when the symlink doesn't exist (fresh install, or
/// non-NixOS host running the agent for the first time).
pub fn read_current_closure(path: &Path) -> Option<String> {
    let target = std::fs::read_link(path).ok()?;
    target
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
}

/// Issue the gated first-heartbeat. Returns the recovery outcome so
/// `runtime::spawn` can act on `replay_from` BEFORE starting workers.
///
/// `cp_url` is the agent's `--control-plane-url`. `machine_id` matches
/// the CN in the agent's mTLS cert. The three `ca_cert`/`client_cert`/
/// `client_key` paths are threaded straight into
/// `crate::comms::build_client` so the handshake rides the same mTLS
/// identity post-Phase-7c workers use (DEFECT-003 + D-005). `None`
/// for all three drops to TLS-only mode (no client cert) — acceptable
/// for the wiremock-driven tests but never production.
///
/// Failure to reach CP is non-fatal: we return an outcome with no
/// replay-from, and the steady-state heartbeat worker will keep
/// retrying. Better to start the agent and have its long-poll +
/// retries re-converge than to refuse to boot.
pub async fn handshake(
    cp_url: &str,
    machine_id: &str,
    clock: &ClockHandle,
    current_system_path: &Path,
    ca_cert: Option<&Path>,
    client_cert: Option<&Path>,
    client_key: Option<&Path>,
) -> RecoveryOutcome {
    let current_closure = read_current_closure(current_system_path);
    let now = clock.now();

    let url = format!("{}/v1/agent/heartbeat", cp_url.trim_end_matches('/'));
    let client = match crate::comms::build_client(ca_cert, client_cert, client_key) {
        Ok(c) => c,
        Err(err) => {
            tracing::warn!(
                target: "agent_recovery",
                error = %err,
                "boot-recovery: mTLS HTTP client build failed; skipping handshake",
            );
            return RecoveryOutcome {
                current_closure,
                replay_from: None,
                heartbeat_sent_at: now,
            };
        }
    };

    let req = HeartbeatRequest {
        hostname: machine_id.to_string(),
        rollout_id: None,
        current_closure: current_closure.clone(),
        at: now,
    };
    // Per-request timeout override: comms::build_client uses a 30s
    // default; boot-recovery wants the same 30s explicitly (matches
    // pre-D-005 behaviour even though the values coincide today — the
    // override makes the contract explicit if either constant drifts).
    let resp = match client
        .post(&url)
        .timeout(RECOVERY_HTTP_TIMEOUT)
        .json(&req)
        .send()
        .await
    {
        Ok(r) => r,
        Err(err) => {
            tracing::warn!(
                target: "agent_recovery",
                error = %err,
                "boot-recovery: heartbeat POST failed; steady-state heartbeat worker will retry",
            );
            return RecoveryOutcome {
                current_closure,
                replay_from: None,
                heartbeat_sent_at: now,
            };
        }
    };

    let replay_from = resp
        .headers()
        .get("X-Nixfleet-Replay-From")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());

    // Drain the body so the connection can be returned to the pool;
    // the response shape is `HeartbeatResponse` but we only need the
    // header for replay-from drift detection.
    if let Ok(_body) = resp.json::<HeartbeatResponse>().await {
        // Body parsed cleanly. No-op; logged below.
    }

    if let Some(seq) = replay_from {
        tracing::warn!(
            target: "agent_recovery",
            replay_from = seq,
            "boot-recovery: CP signaled drift; reducer in-memory state will re-converge \
             via the steady-state heartbeat + long-poll path (synthesised-event \
             reconstruction is documented in recovery.rs; full wiring lands in a \
             follow-up)",
        );
    }

    RecoveryOutcome {
        current_closure,
        replay_from,
        heartbeat_sent_at: now,
    }
}

/// Default location of NixOS's current-system symlink. Test fixtures
/// override via `handshake`'s `current_system_path` parameter.
pub fn default_current_system_path() -> PathBuf {
    PathBuf::from("/run/current-system")
}

/// Synthetic Replay-From payload shape. Documented for the
/// follow-up that wires CP to emit per-event replay bodies; today the
/// agent only sees the seq header.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayFromPayload {
    pub rollout_id: String,
    pub from_seq: u64,
    /// `agent_event_kind` strings the CP plans to synthesise for the
    /// agent. v0.2 emits seq-only; this field is forward-compat.
    pub kinds: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fake_clock() -> ClockHandle {
        std::sync::Arc::new(nixfleet_proto::clock::SystemClock::new())
    }

    #[test]
    fn read_current_closure_returns_basename_when_link_exists() {
        let dir = TempDir::new().unwrap();
        let store_path = dir.path().join("nixos-system-host-26.05");
        std::fs::create_dir(&store_path).unwrap();
        let link = dir.path().join("current-system");
        symlink(&store_path, &link).unwrap();
        let got = read_current_closure(&link).unwrap();
        assert_eq!(got, "nixos-system-host-26.05");
    }

    #[test]
    fn read_current_closure_returns_none_when_missing() {
        let dir = TempDir::new().unwrap();
        let link = dir.path().join("does-not-exist");
        assert!(read_current_closure(&link).is_none());
    }

    #[tokio::test]
    async fn handshake_returns_replay_from_when_cp_signals_drift() {
        // RFC-0008 §9.5 scenario 3: agent crashed mid-Activating;
        // /run/current-system points at the target closure; CP signals
        // Replay-From=42 so the agent knows it should rebuild from
        // that seq. We assert the handshake surfaces the seq.
        let cp = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/agent/heartbeat"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("X-Nixfleet-Replay-From", "42")
                    .set_body_json(serde_json::json!({})),
            )
            .mount(&cp)
            .await;

        let dir = TempDir::new().unwrap();
        let store_path = dir.path().join("target-closure-store-path");
        std::fs::create_dir(&store_path).unwrap();
        let link = dir.path().join("current-system");
        symlink(&store_path, &link).unwrap();

        // TLS-only mode (no mTLS cert) — wiremock listens on plain
        // HTTP; reqwest's rustls config only activates for https://
        // URLs, so the handshake succeeds against http:// wiremock.
        let outcome = handshake(
            &cp.uri(),
            "host-smoke",
            &fake_clock(),
            &link,
            None,
            None,
            None,
        )
        .await;
        assert_eq!(
            outcome.current_closure.as_deref(),
            Some("target-closure-store-path")
        );
        assert_eq!(outcome.replay_from, Some(42));
    }

    #[tokio::test]
    async fn handshake_handles_cp_unreachable_gracefully() {
        let dir = TempDir::new().unwrap();
        let outcome = handshake(
            "http://localhost:1",
            "host-smoke",
            &fake_clock(),
            &dir.path().join("missing-current-system"),
            None,
            None,
            None,
        )
        .await;
        assert!(outcome.current_closure.is_none());
        assert!(outcome.replay_from.is_none());
    }
}
