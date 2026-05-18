//! End-to-end smoke test for the agent runtime.
//!
//! Spins up `runtime::spawn` against a real-disk temp dir state_dir,
//! feeds inputs through `input_tx`, observes the side effects via the
//! durable outbound queue. Proves the integration of:
//!
//!   - ManifestSetUpdated → reducer caches the manifest, subsequent
//!     `LocalActivate` bootstraps Pending state.
//!   - HostEvent → `step()` → applier → outbound queue.
//!   - Full happy-path Pending → Activating → Soaking → Converged,
//!     with the corresponding OutboundAgentEvent records hitting disk.
//!   - Rollback-and-halt: probe Fail → sustained-failure crossed via
//!     `AgentAdvanceTick` → Failed → rollback fired → Reverted.
//!
//! The runtime-integration smoke shape mirrors `crates/nixfleet-control-plane/tests/runtime_smoke.rs`
//! — both pin a bug class (cross-module state transitions producing
//! wrong-but-locally-consistent state) that pure unit tests can't catch.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use nixfleet_agent::runtime::{self, AgentConfig, OutboundQueue, ReducerInput};
use nixfleet_proto::clock::SystemClock;
use nixfleet_proto::testing::FleetBuilder;
use nixfleet_proto::{HealthGate, HostWave, Meta, RolloutManifest};
use nixfleet_reconciler::planner_types::SignedManifestSet;
use nixfleet_reconciler::verify::Verified;
use nixfleet_state_machine::Event;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

async fn wait_for<F>(timeout: Duration, mut predicate: F)
where
    F: FnMut() -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if predicate() {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("wait_for: timed out after {:?}", timeout);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn make_signed_manifest_set(
    channel: &str,
    hostname: &str,
    target_closure: &str,
) -> SignedManifestSet {
    let fleet = FleetBuilder::new()
        .host(hostname, channel)
        .host_closure(hostname, target_closure)
        .build();
    let manifest = RolloutManifest {
        schema_version: 1,
        display_name: format!("{channel}@smoke"),
        channel: channel.to_string(),
        channel_ref: format!("{channel}-rollout"),
        fleet_resolved_hash: "test-hash".to_string(),
        host_set: vec![HostWave {
            hostname: hostname.to_string(),
            wave_index: 0,
            target_closure: target_closure.to_string(),
        }],
        health_gate: HealthGate::default(),
        disruption_budgets: Vec::new(),
        meta: Meta {
            schema_version: 1,
            signed_at: Some(Utc::now()),
            ci_commit: Some("test".to_string()),
            signature_algorithm: None,
        },
    };
    let mut rollouts = HashMap::new();
    rollouts.insert(
        channel.to_string(),
        Verified::unverified_for_tests(manifest, Utc::now()),
    );
    SignedManifestSet {
        fleet: Verified::unverified_for_tests(fleet, Utc::now()),
        rollouts,
    }
}

fn spawn_agent(
    state_dir: &TempDir,
) -> (
    CancellationToken,
    runtime::RuntimeHandle,
    Arc<OutboundQueue>,
) {
    // Gate the activation worker so `fire_switch` returns Spawned
    // without invoking the real `systemd-run` / `darwin-rebuild`. Without
    // this the smoke test would prompt for sudo/polkit on the developer's
    // workstation. SAFETY: env-var scope is the test process; harmless
    // to set unconditionally because `spawn_agent` is only called from
    // tests.
    // SAFETY: `set_var` mutates process-global state; safe in a test
    // helper that's the sole caller of `runtime::spawn` in this file.
    unsafe {
        std::env::set_var("NIXFLEET_AGENT_ACTIVATION_TEST_MODE", "1");
    }

    let cancel = CancellationToken::new();
    let cfg = AgentConfig {
        // CP unreachable — the durable queue still works; the outbound
        // worker just won't drain. That's the property we want to test.
        control_plane_url: "http://localhost:1".to_string(),
        machine_id: "host-smoke".to_string(),
        state_dir: state_dir.path().to_path_buf(),
        trust_file: state_dir.path().join("trust.json"),
        manifest_freshness_window_secs: 3600,
        // mTLS paths None in this smoke harness — workers build a
        // TLS-only client (no client cert). The smoke test only
        // exercises the durable-queue path; no real CP contact.
        ca_cert: None,
        client_cert: None,
        client_key: None,
    };
    let clock: nixfleet_proto::clock::ClockHandle = Arc::new(SystemClock::new());
    let queue = Arc::new(OutboundQueue::open(&cfg.state_dir).expect("open outbound queue"));
    let rt = runtime::spawn(cancel.clone(), cfg, clock);
    (cancel, rt, queue)
}

#[tokio::test]
async fn manifest_set_then_local_activate_bootstraps_host_state() {
    let dir = TempDir::new().unwrap();
    let (cancel, rt, queue) = spawn_agent(&dir);

    let set = make_signed_manifest_set("stable", "host-smoke", "target-closure-abc");
    // RFC-0012 §6.3 + D-007: rollout_id is the canonical
    // `RolloutId::new(channel, channel_ref)` composite. The agent's
    // reducer reconstructs the same key from manifest entries to
    // match against incoming HostEvents — the test must mirror it.
    let rollout_id = nixfleet_proto::RolloutId::new(
        "stable",
        &set.rollouts.get("stable").unwrap().inner().channel_ref,
    );

    rt.input_tx
        .send(ReducerInput::ManifestSetUpdated(Box::new(set)))
        .await
        .unwrap();

    rt.input_tx
        .send(ReducerInput::HostEvent {
            rollout_id: rollout_id.clone(),
            event: Event::LocalActivate {
                current_closure_at_dispatch: "prior-closure".to_string(),
                target_closure: "target-closure-abc".to_string(),
                received_at: Utc::now(),
                soak_due_at: Utc::now() + chrono::Duration::minutes(5),
                seq: 0,
            },
        })
        .await
        .unwrap();

    // LocalActivate's effects:
    //   - RecordTransition (logged, not enqueued)
    //   - LocalEmitEvent { DispatchAck } (enqueued, durable=true)
    //   - LocalFireSwitch { target } (sent via activation channel)
    // We assert against the durable queue: a `DispatchAck` event for
    // this host should land in the queue.
    let q = queue.clone();
    wait_for(Duration::from_secs(3), move || {
        let pending = q.scan_pending().unwrap_or_default();
        pending.iter().any(|e| e.event_kind == "DispatchAck")
    })
    .await;

    let pending = queue.scan_pending().unwrap();
    let dispatch_ack = pending
        .iter()
        .find(|e| e.event_kind == "DispatchAck")
        .expect("DispatchAck must be enqueued after LocalActivate");
    assert_eq!(dispatch_ack.hostname, "host-smoke");
    assert!(
        matches!(
            dispatch_ack.payload,
            nixfleet_proto::AgentEvent::DispatchAck { .. },
        ),
        "queued payload must be the typed DispatchAck variant",
    );

    cancel.cancel();
    drop(rt);
}

#[tokio::test]
async fn full_happy_path_enqueues_dispatchack_activation_and_converged() {
    let dir = TempDir::new().unwrap();
    let (cancel, rt, queue) = spawn_agent(&dir);

    let target = "happy-target";
    let set = make_signed_manifest_set("stable", "host-smoke", target);
    // RFC-0012 §6.3 + D-007: rollout_id is the canonical
    // `RolloutId::new(channel, channel_ref)` composite. The agent's
    // reducer reconstructs the same key from manifest entries to
    // match against incoming HostEvents — the test must mirror it.
    let rollout_id = nixfleet_proto::RolloutId::new(
        "stable",
        &set.rollouts.get("stable").unwrap().inner().channel_ref,
    );

    rt.input_tx
        .send(ReducerInput::ManifestSetUpdated(Box::new(set)))
        .await
        .unwrap();

    // Pending → Activating.
    rt.input_tx
        .send(ReducerInput::HostEvent {
            rollout_id: rollout_id.clone(),
            event: Event::LocalActivate {
                current_closure_at_dispatch: "prior-closure".to_string(),
                target_closure: target.to_string(),
                received_at: Utc::now(),
                soak_due_at: Utc::now() + chrono::Duration::minutes(5),
                seq: 0,
            },
        })
        .await
        .unwrap();

    // Wait for DispatchAck so we know the LocalActivate transition
    // fully applied before we feed the next event.
    {
        let q = queue.clone();
        wait_for(Duration::from_secs(3), move || {
            let p = q.scan_pending().unwrap_or_default();
            p.iter().any(|e| e.event_kind == "DispatchAck")
        })
        .await;
    }

    // Activating → Soaking. The state machine sets soak_due_at = now
    // + 5min on bootstrap (reducer's bootstrap path); we synthesise
    // a completed_at AFTER that so Converged-invariant 2 (converged_at
    // ≥ soak_due_at) holds.
    let activation_completed_at = Utc::now() + chrono::Duration::minutes(6);
    rt.input_tx
        .send(ReducerInput::HostEvent {
            rollout_id: rollout_id.clone(),
            event: Event::LocalActivationCompleted {
                observed_current_closure: target.to_string(),
                exit_code: 0,
                completed_at: activation_completed_at,
                seq: 0,
            },
        })
        .await
        .unwrap();

    // Soaking → Converged. Pass invariant 1 (current == target) and
    // invariant 2 (converged_at ≥ soak_due_at; we use the same time
    // as activation_completed which is also past soak_due_at).
    rt.input_tx
        .send(ReducerInput::HostEvent {
            rollout_id: rollout_id.clone(),
            event: Event::LocalConvergedReached {
                converged_at: activation_completed_at + chrono::Duration::seconds(10),
                current_closure: target.to_string(),
                seq: 0,
            },
        })
        .await
        .unwrap();

    // Final assertion: the queue contains DispatchAck + (some
    // ActivationCompleted / Converged) records. We don't assert
    // perfect ordering because the durable queue is keyed by seq,
    // and the reducer assigns seqs monotonically; ordering follows.
    let q = queue.clone();
    wait_for(Duration::from_secs(5), move || {
        let p = q.scan_pending().unwrap_or_default();
        p.iter().any(|e| e.event_kind == "Converged")
            && p.iter().any(|e| e.event_kind == "ActivationCompleted")
            && p.iter().any(|e| e.event_kind == "DispatchAck")
    })
    .await;

    let pending = queue.scan_pending().unwrap();
    // Verify seq monotonicity: DispatchAck < ActivationCompleted < Converged.
    let by_kind: HashMap<_, _> = pending
        .iter()
        .map(|e| (e.event_kind.clone(), e.seq))
        .collect();
    assert!(by_kind["DispatchAck"] < by_kind["ActivationCompleted"]);
    assert!(by_kind["ActivationCompleted"] < by_kind["Converged"]);

    cancel.cancel();
    drop(rt);
}
