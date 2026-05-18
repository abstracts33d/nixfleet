//! End-to-end smoke test for the new RFC-0006 runtime.
//!
//! Spins up `runtime::spawn` against an in-memory DB, feeds inputs through
//! the MPSC, observes the side effects (DB rows, event_log entries,
//! heartbeat replies). Proves the integration of:
//!   - manifest_poll ⇒ ManifestSetUpdated ⇒ plan_next ⇒ OpenRollout applier
//!   - HostEvent ⇒ state_machine::step ⇒ apply_effect ⇒ host_rollout_records
//!     + event_log writes
//!   - HeartbeatReceived ⇒ drift compare ⇒ Replay-From reply
//!   - compliance_wave gate ⇒ DeferDispatch ⇒ event_log GateDecision
//!
//! The runtime is async; assertions poll the DB on a tight timeout. If a
//! test hangs the harness times out — better than relying on hard-coded
//! sleeps.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use nixfleet_control_plane::db::Db;
use nixfleet_control_plane::runtime::{self, HeartbeatReply, ReducerInput};
use nixfleet_control_plane::server::AppState;
use nixfleet_proto::clock::SystemClock;
use nixfleet_proto::testing::FleetBuilder;
use nixfleet_proto::{HealthGate, HostWave, Meta, RolloutBudget, RolloutManifest, Selector};
use nixfleet_reconciler::planner_types::SignedManifestSet;
use nixfleet_reconciler::verify::Verified;
use nixfleet_state_machine::Event;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// Bounded wait for a predicate to become true. Returns on first satisfied
/// poll or panics on timeout. The runtime is async; without bounded polling
/// a flaky DB schedule could deadlock the test.
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

fn make_state() -> Arc<AppState> {
    let db = Db::open_in_memory().expect("open in-memory db");
    db.migrate().expect("migrate");
    Arc::new(AppState {
        db: Some(Arc::new(db)),
        ..Default::default()
    })
}

/// Build a fleet with one channel "stable" and one host "h1" closure-pinned.
/// Default compliance is "disabled" so gates won't intercept QueueDispatch.
fn fleet_one_host(host_closure: &str) -> nixfleet_proto::FleetResolved {
    FleetBuilder::new()
        .host("h1", "stable")
        .host_closure("h1", host_closure)
        .build()
}

/// Synthesise the rollout manifest for "stable" that manifest_poll would
/// have produced. This smoke harness doesn't exercise the real poller
/// (no forge in the test); we hand-build the equivalent SignedManifestSet.
fn rollout_manifest_one_host(channel: &str, host: &str, closure: &str) -> RolloutManifest {
    RolloutManifest {
        schema_version: 1,
        display_name: format!("{channel}@test"),
        channel: channel.to_string(),
        channel_ref: format!("{channel}-rollout"),
        fleet_resolved_hash: "test-hash".to_string(),
        host_set: vec![HostWave {
            hostname: host.to_string(),
            wave_index: 0,
            target_closure: closure.to_string(),
        }],
        health_gate: HealthGate::default(),
        disruption_budgets: Vec::new(),
        meta: Meta {
            schema_version: 1,
            signed_at: Some(Utc::now()),
            ci_commit: Some("test-ci-commit".to_string()),
            signature_algorithm: None,
        },
    }
}

fn signed_manifest_set_one_host(host_closure: &str) -> SignedManifestSet {
    let fleet = fleet_one_host(host_closure);
    let manifest = rollout_manifest_one_host("stable", "h1", host_closure);
    let mut rollouts = HashMap::new();
    rollouts.insert(
        "stable".to_string(),
        Verified::unverified_for_tests(manifest, Utc::now()),
    );
    SignedManifestSet {
        fleet: Verified::unverified_for_tests(fleet, Utc::now()),
        rollouts,
    }
}

#[tokio::test]
async fn manifest_set_updated_opens_rollout_and_creates_pending_record() {
    let state = make_state();
    let db = state.db.clone().unwrap();
    let cancel = CancellationToken::new();
    let clock = Arc::new(SystemClock::new());
    let rt = runtime::spawn(cancel.clone(), state.clone(), clock);

    // Feed a SignedManifestSet. Plan_next emits OpenRollout, the applier
    // creates a Pending host_rollout_records row for h1.
    let set = signed_manifest_set_one_host("target-closure");
    // RFC-0008 §6.3 + D-007: rollout_id is the canonical
    // `RolloutId::new(channel, channel_ref)` composite. Reconstruct
    // here so the test's lookups by rollout_id match what
    // `build_fleet_state` + the planner produce.
    let rollout_id = nixfleet_proto::RolloutId::new(
        "stable",
        &set.rollouts.get("stable").unwrap().inner().channel_ref,
    );
    rt.input_tx
        .send(ReducerInput::ManifestSetUpdated(Box::new(set)))
        .await
        .expect("send ManifestSetUpdated");

    // Poll for the Pending row.
    let db_for_poll = db.clone();
    let rollout_for_poll = rollout_id.clone();
    wait_for(Duration::from_secs(3), || {
        db_for_poll
            .host_rollout_records()
            .load(rollout_for_poll.as_str(), "h1")
            .ok()
            .flatten()
            .is_some()
    })
    .await;

    let loaded = db
        .host_rollout_records()
        .load(rollout_id.as_str(), "h1")
        .unwrap()
        .expect("Pending row must exist after OpenRollout applier");
    assert_eq!(loaded.target_closure, "target-closure");
    assert_eq!(
        loaded.state,
        nixfleet_state_machine::HostState::Pending,
        "first OpenRollout ⇒ host_rollout_records row at HostState::Pending",
    );

    cancel.cancel();
    drop(rt);
}

#[tokio::test]
async fn host_event_drives_state_transition_and_writes_event_log() {
    let state = make_state();
    let db = state.db.clone().unwrap();
    let cancel = CancellationToken::new();
    let clock = Arc::new(SystemClock::new());
    let rt = runtime::spawn(cancel.clone(), state.clone(), clock);

    // Same as the previous test: seed the rollout.
    let set = signed_manifest_set_one_host("target-closure");
    // RFC-0008 §6.3 + D-007: rollout_id is the canonical
    // `RolloutId::new(channel, channel_ref)` composite. Reconstruct
    // here so the test's lookups by rollout_id match what
    // `build_fleet_state` + the planner produce.
    let rollout_id = nixfleet_proto::RolloutId::new(
        "stable",
        &set.rollouts.get("stable").unwrap().inner().channel_ref,
    );
    rt.input_tx
        .send(ReducerInput::ManifestSetUpdated(Box::new(set)))
        .await
        .expect("send ManifestSetUpdated");

    {
        let db_for_poll = db.clone();
        let rollout_for_poll = rollout_id.clone();
        wait_for(Duration::from_secs(3), || {
            db_for_poll
                .host_rollout_records()
                .load(rollout_for_poll.as_str(), "h1")
                .ok()
                .flatten()
                .is_some()
        })
        .await;
    }

    // Drive a state transition via RemoteDispatchAck. From Pending this
    // moves the host to Activating + emits RecordTransition +
    // RemoteAppendEventLog effects.
    rt.input_tx
        .send(ReducerInput::HostEvent {
            host: "h1".to_string(),
            rollout_id: rollout_id.clone(),
            event: Event::RemoteDispatchAck {
                current_closure_at_dispatch: "previous-closure".to_string(),
                received_at: Utc::now(),
                seq: 1,
            },
        })
        .await
        .expect("send HostEvent");

    {
        let db_for_poll = db.clone();
        let rollout_for_poll = rollout_id.clone();
        wait_for(Duration::from_secs(3), || {
            db_for_poll
                .host_rollout_records()
                .load(rollout_for_poll.as_str(), "h1")
                .ok()
                .flatten()
                .map(|s| s.state == nixfleet_state_machine::HostState::Activating)
                .unwrap_or(false)
        })
        .await;
    }

    let after = db
        .host_rollout_records()
        .load(rollout_id.as_str(), "h1")
        .unwrap()
        .unwrap();
    assert_eq!(after.state, nixfleet_state_machine::HostState::Activating);
    assert_eq!(after.last_event_seq, 1);
    assert_eq!(
        after.current_closure_at_dispatch.as_deref(),
        Some("previous-closure"),
    );

    // Event log should have an AgentEvent (from RemoteAppendEventLog) and an
    // Effect (RecordTransition). Poll because the writer task is async.
    {
        let db_for_poll = db.clone();
        wait_for(Duration::from_secs(3), || {
            let rows = db_for_poll
                .event_log()
                .query_by_host("h1", 100)
                .unwrap_or_default();
            rows.iter().any(|r| r.kind == "agent_event") && rows.iter().any(|r| r.kind == "effect")
        })
        .await;
    }

    cancel.cancel();
    drop(rt);
}

#[tokio::test]
async fn heartbeat_with_closure_mismatch_returns_replay_from_seq() {
    let state = make_state();
    let db = state.db.clone().unwrap();
    let cancel = CancellationToken::new();
    let clock = Arc::new(SystemClock::new());
    let rt = runtime::spawn(cancel.clone(), state.clone(), clock);

    // Seed: rollout open, host advances to Activating (sets last_event_seq=1).
    let set = signed_manifest_set_one_host("target-closure");
    // RFC-0008 §6.3 + D-007: rollout_id is the canonical
    // `RolloutId::new(channel, channel_ref)` composite. Reconstruct
    // here so the test's lookups by rollout_id match what
    // `build_fleet_state` + the planner produce.
    let rollout_id = nixfleet_proto::RolloutId::new(
        "stable",
        &set.rollouts.get("stable").unwrap().inner().channel_ref,
    );
    rt.input_tx
        .send(ReducerInput::ManifestSetUpdated(Box::new(set)))
        .await
        .unwrap();
    {
        let db_for_poll = db.clone();
        let rollout_for_poll = rollout_id.clone();
        wait_for(Duration::from_secs(3), || {
            db_for_poll
                .host_rollout_records()
                .load(rollout_for_poll.as_str(), "h1")
                .ok()
                .flatten()
                .is_some()
        })
        .await;
    }
    rt.input_tx
        .send(ReducerInput::HostEvent {
            host: "h1".to_string(),
            rollout_id: rollout_id.clone(),
            event: Event::RemoteDispatchAck {
                current_closure_at_dispatch: "previous-closure".to_string(),
                received_at: Utc::now(),
                seq: 1,
            },
        })
        .await
        .unwrap();
    {
        let db_for_poll = db.clone();
        let rollout_for_poll = rollout_id.clone();
        wait_for(Duration::from_secs(3), || {
            db_for_poll
                .host_rollout_records()
                .load(rollout_for_poll.as_str(), "h1")
                .ok()
                .flatten()
                .map(|s| s.last_event_seq == 1)
                .unwrap_or(false)
        })
        .await;
    }

    // Heartbeat with a current_closure that disagrees with the CP-mirror
    // (the mirror has current_closure = None at this point; the drift
    // detector treats anything ≠ "what CP knows" as drift and replies
    // with last_event_seq for Replay-From). RFC-0005 §4.3 semantics.
    let (reply_tx, reply_rx) = oneshot::channel::<HeartbeatReply>();
    rt.input_tx
        .send(ReducerInput::HeartbeatReceived {
            host: "h1".to_string(),
            rollout_id: Some(rollout_id.clone()),
            current_closure: Some("agent-says-this-other-closure".to_string()),
            at: Utc::now(),
            reply: reply_tx,
        })
        .await
        .unwrap();

    let reply = tokio::time::timeout(Duration::from_secs(3), reply_rx)
        .await
        .expect("heartbeat reply within timeout")
        .expect("reducer must send the reply (oneshot Sender must not drop)");

    assert_eq!(
        reply.replay_from,
        Some(1),
        "drift detected (CP has no current_closure recorded yet) ⇒ Replay-From should equal last_event_seq",
    );

    cancel.cancel();
    drop(rt);
}

// Compliance-wave gate end-to-end test deferred to v0.2.1 (see
// `.claude/plans/v0.2.1-followups.md` item 10 — per-worker integration
// tests). Shape:
//
//   agent ProbeResult event → /v1/agent/events ingest →
//   applier RemoteAppendEventLog → event_log row + probe_failures rows
//   (one txn) → build_fleet_state.outstanding_failing_enforce_probes →
//   planner_gates::compliance_wave → PlanAction::DeferDispatch →
//   event_log GateDecision row.

/// LIFT #1: boot-recovery retroactive confirmation. The regression
/// pinned here: an agent restart mid-Activating (e.g. framework upgrade
/// restarts nixfleet-agent.service mid verify_poll) drops the in-memory
/// event stream. The new agent's boot-recovery heartbeat reports
/// `current_closure = target_closure` (read from /run/current-system)
/// but no rollout_id — so the steady-state replay_from drift detector
/// can't match. Pre-fix CP did nothing; host_rollout_records.state
/// stayed Activating forever, the planner never re-dispatched, the
/// cascade halted. Post-fix CP scans active_for_host, sees the match,
/// synthesizes RemoteActivationCompleted, transitions Activating →
/// Soaking, and the cascade resumes.
///
/// Test shape: open a rollout, advance h1 to Activating, then send a
/// heartbeat with current_closure == target_closure AND rollout_id =
/// None (the boot-recovery shape). Assert h1 is now in Soaking with
/// activation_completed_at populated and observed_current_closure
/// stamped on host_rollout_records.current_closure.
#[tokio::test]
async fn heartbeat_synthesizes_activation_completed_when_agent_reports_target_in_activating() {
    let state = make_state();
    let db = state.db.clone().unwrap();
    let cancel = CancellationToken::new();
    let clock = Arc::new(SystemClock::new());
    let rt = runtime::spawn(cancel.clone(), state.clone(), clock);

    // Seed: open rollout, advance h1 Pending → Activating via
    // RemoteDispatchAck. Same flow real CP-side mirror takes.
    let set = signed_manifest_set_one_host("target-closure-X");
    let rollout_id = nixfleet_proto::RolloutId::new(
        "stable",
        &set.rollouts.get("stable").unwrap().inner().channel_ref,
    );
    rt.input_tx
        .send(ReducerInput::ManifestSetUpdated(Box::new(set)))
        .await
        .unwrap();
    {
        let db_for_poll = db.clone();
        let rollout_for_poll = rollout_id.clone();
        wait_for(Duration::from_secs(3), || {
            db_for_poll
                .host_rollout_records()
                .load(rollout_for_poll.as_str(), "h1")
                .ok()
                .flatten()
                .is_some()
        })
        .await;
    }
    rt.input_tx
        .send(ReducerInput::HostEvent {
            host: "h1".to_string(),
            rollout_id: rollout_id.clone(),
            event: Event::RemoteDispatchAck {
                current_closure_at_dispatch: "previous-closure".to_string(),
                received_at: Utc::now(),
                seq: 1,
            },
        })
        .await
        .unwrap();
    {
        let db_for_poll = db.clone();
        let rollout_for_poll = rollout_id.clone();
        wait_for(Duration::from_secs(3), || {
            db_for_poll
                .host_rollout_records()
                .load(rollout_for_poll.as_str(), "h1")
                .ok()
                .flatten()
                .map(|s| s.state == nixfleet_state_machine::HostState::Activating)
                .unwrap_or(false)
        })
        .await;
    }

    // Boot-recovery shape: heartbeat carries current_closure but no
    // rollout_id (agent's reducer is fresh post-restart). Pre-fix this
    // would compute replay_from = None (no rollout_id → early return)
    // and do nothing else; host stays Activating forever.
    let (reply_tx, reply_rx) = oneshot::channel::<HeartbeatReply>();
    rt.input_tx
        .send(ReducerInput::HeartbeatReceived {
            host: "h1".to_string(),
            rollout_id: None,
            current_closure: Some("target-closure-X".to_string()),
            at: Utc::now(),
            reply: reply_tx,
        })
        .await
        .unwrap();

    // Post-LIFT-#3 ordering: synthesis runs BEFORE the reply, so the
    // reply reflects post-synthesis state (Soaking, with bootstrap
    // snapshots populated). replay_from is None because the agent
    // didn't supply a rollout_id.
    let reply = tokio::time::timeout(Duration::from_secs(3), reply_rx)
        .await
        .expect("heartbeat reply within timeout")
        .expect("reducer must send the reply");
    assert_eq!(
        reply.replay_from, None,
        "boot-recovery heartbeat with rollout_id=None ⇒ replay_from path can't match ⇒ None",
    );

    // The reducer has already transitioned to Soaking by the time the
    // reply was sent — the synthesis runs in-line before the reply.
    let db_for_poll = db.clone();
    let rollout_for_poll = rollout_id.clone();
    wait_for(Duration::from_secs(3), move || {
        db_for_poll
            .host_rollout_records()
            .load(rollout_for_poll.as_str(), "h1")
            .ok()
            .flatten()
            .map(|s| {
                s.state == nixfleet_state_machine::HostState::Soaking
                    && s.activation_completed_at.is_some()
                    && s.current_closure.as_deref() == Some("target-closure-X")
            })
            .unwrap_or(false)
    })
    .await;

    cancel.cancel();
    drop(rt);
}

/// LIFT #3 regression: when the agent's heartbeat has `rollout_id = None`
/// (the boot-recovery shape — agent's reducer is empty after restart)
/// AND CP holds non-terminal records for the host, the reply MUST carry
/// a `bootstrap_rollouts` vec with one snapshot per active record. The
/// snapshot fields reflect post-synthesis state (e.g. Soaking after
/// LIFT #1 fired). Pre-LIFT-#3 the reply only carried `replay_from`,
/// the agent couldn't rehydrate its in-memory state, and downstream
/// workers (probe runner, advance-ticker) saw an empty HashMap forever.
#[tokio::test]
async fn heartbeat_response_includes_bootstrap_for_active_rollouts_when_agent_supplies_no_rollout_id()
 {
    let state = make_state();
    let db = state.db.clone().unwrap();
    let cancel = CancellationToken::new();
    let clock = Arc::new(SystemClock::new());
    let rt = runtime::spawn(cancel.clone(), state.clone(), clock);

    // Seed: open rollout, advance h1 to Activating.
    let set = signed_manifest_set_one_host("target-closure-X");
    let rollout_id = nixfleet_proto::RolloutId::new(
        "stable",
        &set.rollouts.get("stable").unwrap().inner().channel_ref,
    );
    rt.input_tx
        .send(ReducerInput::ManifestSetUpdated(Box::new(set)))
        .await
        .unwrap();
    {
        let db_for_poll = db.clone();
        let rollout_for_poll = rollout_id.clone();
        wait_for(Duration::from_secs(3), || {
            db_for_poll
                .host_rollout_records()
                .load(rollout_for_poll.as_str(), "h1")
                .ok()
                .flatten()
                .is_some()
        })
        .await;
    }
    rt.input_tx
        .send(ReducerInput::HostEvent {
            host: "h1".to_string(),
            rollout_id: rollout_id.clone(),
            event: Event::RemoteDispatchAck {
                current_closure_at_dispatch: "previous-closure".to_string(),
                received_at: Utc::now(),
                seq: 1,
            },
        })
        .await
        .unwrap();
    {
        let db_for_poll = db.clone();
        let rollout_for_poll = rollout_id.clone();
        wait_for(Duration::from_secs(3), || {
            db_for_poll
                .host_rollout_records()
                .load(rollout_for_poll.as_str(), "h1")
                .ok()
                .flatten()
                .map(|s| s.state == nixfleet_state_machine::HostState::Activating)
                .unwrap_or(false)
        })
        .await;
    }

    // Boot-recovery heartbeat: rollout_id=None, current==target.
    // Triggers LIFT #1 synthesis (Activating→Soaking) AND LIFT #3
    // bootstrap (snapshot reflects post-synthesis Soaking state).
    let (reply_tx, reply_rx) = oneshot::channel::<HeartbeatReply>();
    rt.input_tx
        .send(ReducerInput::HeartbeatReceived {
            host: "h1".to_string(),
            rollout_id: None,
            current_closure: Some("target-closure-X".to_string()),
            at: Utc::now(),
            reply: reply_tx,
        })
        .await
        .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(3), reply_rx)
        .await
        .expect("heartbeat reply within timeout")
        .expect("reducer must send the reply");

    assert_eq!(
        reply.bootstrap_rollouts.len(),
        1,
        "MUST include one bootstrap snapshot for h1's active rollout",
    );
    let snapshot = &reply.bootstrap_rollouts[0];
    assert_eq!(snapshot.hostname, "h1");
    assert_eq!(snapshot.target_closure, "target-closure-X");
    assert_eq!(
        snapshot.state,
        nixfleet_proto::HostRolloutState::Soaking,
        "snapshot reflects POST-synthesis state — LIFT #1 advanced Activating → Soaking before the reply",
    );
    assert_eq!(
        snapshot.current_closure.as_deref(),
        Some("target-closure-X"),
        "current_closure reflects what LIFT #1 stamped at synthesis time",
    );
    assert!(snapshot.activation_completed_at.is_some());

    cancel.cancel();
    drop(rt);
}

/// LIFT #3 negative: when the agent's heartbeat carries a rollout_id
/// (steady-state shape), bootstrap snapshots are NOT included. The
/// agent already knows its rollout state; sending redundant snapshots
/// would be wire noise and risks clobbering local state.
#[tokio::test]
async fn heartbeat_response_omits_bootstrap_when_agent_supplies_rollout_id() {
    let state = make_state();
    let db = state.db.clone().unwrap();
    let cancel = CancellationToken::new();
    let clock = Arc::new(SystemClock::new());
    let rt = runtime::spawn(cancel.clone(), state.clone(), clock);

    let set = signed_manifest_set_one_host("target-closure-X");
    let rollout_id = nixfleet_proto::RolloutId::new(
        "stable",
        &set.rollouts.get("stable").unwrap().inner().channel_ref,
    );
    rt.input_tx
        .send(ReducerInput::ManifestSetUpdated(Box::new(set)))
        .await
        .unwrap();
    {
        let db_for_poll = db.clone();
        let rollout_for_poll = rollout_id.clone();
        wait_for(Duration::from_secs(3), || {
            db_for_poll
                .host_rollout_records()
                .load(rollout_for_poll.as_str(), "h1")
                .ok()
                .flatten()
                .is_some()
        })
        .await;
    }

    // Steady-state heartbeat: agent supplies rollout_id. NO bootstrap.
    let (reply_tx, reply_rx) = oneshot::channel::<HeartbeatReply>();
    rt.input_tx
        .send(ReducerInput::HeartbeatReceived {
            host: "h1".to_string(),
            rollout_id: Some(rollout_id.clone()),
            current_closure: None,
            at: Utc::now(),
            reply: reply_tx,
        })
        .await
        .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(3), reply_rx)
        .await
        .expect("heartbeat reply within timeout")
        .expect("reducer must send the reply");

    assert!(
        reply.bootstrap_rollouts.is_empty(),
        "steady-state heartbeat (rollout_id populated) MUST NOT include bootstrap snapshots; got {:?}",
        reply.bootstrap_rollouts,
    );

    cancel.cancel();
    drop(rt);
}

/// Negative case for LIFT #1: when the agent's `current_closure` does
/// NOT match the host_rollout_records.target_closure, no synthesis
/// happens. Pinned because the synthesis MUST be idempotent on
/// non-matching states and MUST NOT corrupt state when the agent reports
/// an unrelated closure (e.g. a host that's at the wrong closure
/// entirely — operator-level intervention case).
#[tokio::test]
async fn heartbeat_does_not_synthesize_when_current_closure_does_not_match_target() {
    let state = make_state();
    let db = state.db.clone().unwrap();
    let cancel = CancellationToken::new();
    let clock = Arc::new(SystemClock::new());
    let rt = runtime::spawn(cancel.clone(), state.clone(), clock);

    let set = signed_manifest_set_one_host("target-closure-X");
    let rollout_id = nixfleet_proto::RolloutId::new(
        "stable",
        &set.rollouts.get("stable").unwrap().inner().channel_ref,
    );
    rt.input_tx
        .send(ReducerInput::ManifestSetUpdated(Box::new(set)))
        .await
        .unwrap();
    {
        let db_for_poll = db.clone();
        let rollout_for_poll = rollout_id.clone();
        wait_for(Duration::from_secs(3), || {
            db_for_poll
                .host_rollout_records()
                .load(rollout_for_poll.as_str(), "h1")
                .ok()
                .flatten()
                .is_some()
        })
        .await;
    }
    rt.input_tx
        .send(ReducerInput::HostEvent {
            host: "h1".to_string(),
            rollout_id: rollout_id.clone(),
            event: Event::RemoteDispatchAck {
                current_closure_at_dispatch: "previous-closure".to_string(),
                received_at: Utc::now(),
                seq: 1,
            },
        })
        .await
        .unwrap();
    {
        let db_for_poll = db.clone();
        let rollout_for_poll = rollout_id.clone();
        wait_for(Duration::from_secs(3), || {
            db_for_poll
                .host_rollout_records()
                .load(rollout_for_poll.as_str(), "h1")
                .ok()
                .flatten()
                .map(|s| s.state == nixfleet_state_machine::HostState::Activating)
                .unwrap_or(false)
        })
        .await;
    }

    let (reply_tx, reply_rx) = oneshot::channel::<HeartbeatReply>();
    rt.input_tx
        .send(ReducerInput::HeartbeatReceived {
            host: "h1".to_string(),
            rollout_id: None,
            // Agent reports a DIFFERENT closure than the target. This is
            // the "operator forced a wrong closure" case; no synthesis.
            current_closure: Some("unrelated-closure".to_string()),
            at: Utc::now(),
            reply: reply_tx,
        })
        .await
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(3), reply_rx)
        .await
        .expect("heartbeat reply within timeout")
        .expect("reducer must send the reply");

    // Settle: drain any pending tasks.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let state_after = db
        .host_rollout_records()
        .load(rollout_id.as_str(), "h1")
        .unwrap()
        .expect("h1 record still present");
    assert_eq!(
        state_after.state,
        nixfleet_state_machine::HostState::Activating,
        "non-matching current_closure must NOT advance state",
    );
    assert!(
        state_after.activation_completed_at.is_none(),
        "non-matching current_closure must NOT stamp activation_completed_at",
    );

    cancel.cancel();
    drop(rt);
}

/// LIFT #5 regression: when CP is wiped + restarted while agents stay
/// up, the planner re-opens the rollout in `Pending` and the agent's
/// next heartbeat carries `current_closure == target_closure`. CP MUST
/// drive the row through the full `Pending → Activating → Soaking →
/// Converged` synthesis chain on that one heartbeat so the §305
/// "one reconcile cycle, zero operator intervention" property holds.
#[tokio::test]
async fn heartbeat_synthesizes_pending_to_converged_when_agent_already_at_target() {
    let state = make_state();
    let db = state.db.clone().unwrap();
    let cancel = CancellationToken::new();
    let clock = Arc::new(SystemClock::new());
    let rt = runtime::spawn(cancel.clone(), state.clone(), clock);

    let set = signed_manifest_set_one_host("target-closure-X");
    let rollout_id = nixfleet_proto::RolloutId::new(
        "stable",
        &set.rollouts.get("stable").unwrap().inner().channel_ref,
    );
    rt.input_tx
        .send(ReducerInput::ManifestSetUpdated(Box::new(set)))
        .await
        .unwrap();
    // Wait for planner-driven OpenRollout to create the Pending row.
    {
        let db_for_poll = db.clone();
        let rollout_for_poll = rollout_id.clone();
        wait_for(Duration::from_secs(3), || {
            db_for_poll
                .host_rollout_records()
                .load(rollout_for_poll.as_str(), "h1")
                .ok()
                .flatten()
                .map(|s| s.state == nixfleet_state_machine::HostState::Pending)
                .unwrap_or(false)
        })
        .await;
    }

    // Post-wipe heartbeat: rollout_id=None, current==target. The
    // agent's reducer pre-wipe was at Converged; CP has just re-opened
    // the rollout in Pending. The synthesis chain must close the gap
    // without any operator action on the agent.
    let (reply_tx, reply_rx) = oneshot::channel::<HeartbeatReply>();
    rt.input_tx
        .send(ReducerInput::HeartbeatReceived {
            host: "h1".to_string(),
            rollout_id: None,
            current_closure: Some("target-closure-X".to_string()),
            at: Utc::now(),
            reply: reply_tx,
        })
        .await
        .unwrap();
    let _reply = tokio::time::timeout(Duration::from_secs(3), reply_rx)
        .await
        .expect("heartbeat reply within timeout")
        .expect("reducer must send the reply");

    // The synthesis chain is in-process and runs before the reply
    // returns, but the upserts go through the reducer's MPSC. Poll
    // briefly until the row reaches Converged.
    {
        let db_for_poll = db.clone();
        let rollout_for_poll = rollout_id.clone();
        wait_for(Duration::from_secs(3), || {
            db_for_poll
                .host_rollout_records()
                .load(rollout_for_poll.as_str(), "h1")
                .ok()
                .flatten()
                .map(|s| s.state == nixfleet_state_machine::HostState::Converged)
                .unwrap_or(false)
        })
        .await;
    }

    let final_state = db
        .host_rollout_records()
        .load(rollout_id.as_str(), "h1")
        .unwrap()
        .expect("h1 record present after synthesis");
    assert_eq!(
        final_state.state,
        nixfleet_state_machine::HostState::Converged,
        "Pending → Converged synthesis chain must land at Converged",
    );
    assert!(
        final_state.converged_at.is_some(),
        "Converged transition must stamp converged_at (the v0.2.0 teardown harness gates on this)",
    );
    assert!(
        final_state.activation_completed_at.is_some(),
        "synthesised Activating → Soaking transition must stamp activation_completed_at",
    );
    assert!(
        final_state.dispatch_acked_at.is_some(),
        "synthesised Pending → Activating transition must stamp dispatch_acked_at",
    );

    cancel.cancel();
    drop(rt);
}

#[tokio::test]
async fn current_wave_advances_when_every_wave_zero_host_converges() {
    // Regression for the wave-promotion bump. Without it,
    // `rollouts.current_wave` stays at 0 forever — wave_promotion
    // blocks every wave-1+ host on every plan_next, multi-wave rollouts
    // never progress.
    //
    // Setup: 2-wave fleet, h1 in wave 0, h2 in wave 1. After OpenRollout
    // creates Pending rows for both, force h1 to Converged via a direct
    // host_rollout_records.upsert (the test isolates the applier wiring,
    // not the full state-machine path which is covered separately). Fire
    // a PlanTick. Expect:
    //   1. rollouts.current_wave bumps from 0 → 1.
    //   2. plan_next emits QueueDispatch for h2 (wave_promotion no longer
    //      blocks h2 since current_wave is now 1, matching h2's wave_index).

    let state = make_state();
    let db = state.db.clone().unwrap();
    let cancel = CancellationToken::new();
    let clock = Arc::new(SystemClock::new());
    let rt = runtime::spawn(cancel.clone(), state.clone(), clock);

    let fleet = FleetBuilder::new()
        .host("h1", "stable")
        .host_closure("h1", "h1-closure")
        .host("h2", "stable")
        .host_closure("h2", "h2-closure")
        .wave("stable", &["h1"])
        .wave("stable", &["h2"])
        .build();

    let manifest = RolloutManifest {
        schema_version: 1,
        display_name: "stable@wave-test".into(),
        channel: "stable".into(),
        channel_ref: "stable".into(),
        fleet_resolved_hash: "test-hash".into(),
        host_set: vec![
            HostWave {
                hostname: "h1".into(),
                wave_index: 0,
                target_closure: "h1-closure".into(),
            },
            HostWave {
                hostname: "h2".into(),
                wave_index: 1,
                target_closure: "h2-closure".into(),
            },
        ],
        health_gate: HealthGate::default(),
        disruption_budgets: Vec::new(),
        meta: Meta {
            schema_version: 1,
            signed_at: Some(Utc::now()),
            ci_commit: Some("test-ci-commit".to_string()),
            signature_algorithm: None,
        },
    };
    let mut rollouts = HashMap::new();
    rollouts.insert(
        "stable".to_string(),
        Verified::unverified_for_tests(manifest, Utc::now()),
    );
    let set = SignedManifestSet {
        fleet: Verified::unverified_for_tests(fleet, Utc::now()),
        rollouts,
    };

    rt.input_tx
        .send(ReducerInput::ManifestSetUpdated(Box::new(set)))
        .await
        .unwrap();

    // RFC-0008 §6.3 + D-007: rollout_id is the canonical
    // `RolloutId::new(channel, channel_ref)` composite. This fixture
    // uses `channel: "stable"` and `channel_ref: "stable"` so the
    // composite is `"stable@stable"`.
    let rollout_id = nixfleet_proto::RolloutId::new("stable", "stable");
    {
        let db_for_poll = db.clone();
        let rollout_id_clone = rollout_id.clone();
        wait_for(Duration::from_secs(3), || {
            db_for_poll
                .host_rollout_records()
                .load(rollout_id_clone.as_str(), "h1")
                .ok()
                .flatten()
                .is_some()
        })
        .await;
    }

    // Force h1 to Converged. Bypasses the agent-driven state machine to
    // isolate the applier's wave-promotion logic.
    let mut h1 = db
        .host_rollout_records()
        .load(rollout_id.as_str(), "h1")
        .unwrap()
        .expect("h1 Pending row");
    h1.state = nixfleet_state_machine::HostState::Converged;
    h1.current_closure = Some("h1-closure".into());
    h1.converged_at = Some(Utc::now());
    db.host_rollout_records().upsert(&h1).unwrap();

    rt.input_tx.send(ReducerInput::PlanTick).await.unwrap();

    let db_for_poll = db.clone();
    let rollout_id_clone = rollout_id.clone();
    wait_for(Duration::from_secs(3), || {
        db_for_poll
            .rollouts()
            .current_wave(rollout_id_clone.as_str())
            .ok()
            .flatten()
            .map(|w| w == 1)
            .unwrap_or(false)
    })
    .await;

    let db_for_poll = db.clone();
    wait_for(Duration::from_secs(3), || {
        db_for_poll
            .dispatch_queue()
            .peek_for_host("h2")
            .unwrap_or(false)
    })
    .await;

    cancel.cancel();
    drop(rt);
}

/// **D-027 regression test.** The other-direction pin for wave-promotion:
/// when wave-0 hosts are still Pending (not yet Converged), wave-1 hosts
/// MUST NOT be dispatched. Pre-fix, `HostJoined` events bumped
/// `rollouts.current_wave` to the joiner's wave_index; with a multi-wave
/// rollout the first plan tick fired HostJoined for every host
/// (including wave-1 ones), leaving `current_wave = waves.len() - 1`. The
/// wave-promotion gate's `host_wave > current_wave` predicate then
/// vacuously passed every host. Lab observed: ohm (wave 1) activated 35ms
/// after krach (wave 0), same planner tick.
///
/// Post-fix, `HostJoined` does NOT mutate current_wave; the cursor stays
/// at 0 after OpenRollout and the gate correctly blocks wave-1 hosts
/// until `advance_current_waves` bumps the cursor (which only happens
/// when every wave-0 host reaches Converged).
#[tokio::test]
async fn wave_one_hosts_do_not_dispatch_while_wave_zero_hosts_are_pending() {
    let state = make_state();
    let db = state.db.clone().unwrap();
    let cancel = CancellationToken::new();
    let clock = Arc::new(SystemClock::new());
    let rt = runtime::spawn(cancel.clone(), state.clone(), clock);

    // 2-wave fleet: h1 in wave 0, h2 in wave 1.
    let fleet = FleetBuilder::new()
        .host("h1", "stable")
        .host_closure("h1", "h1-closure")
        .host("h2", "stable")
        .host_closure("h2", "h2-closure")
        .wave("stable", &["h1"])
        .wave("stable", &["h2"])
        .build();

    let manifest = RolloutManifest {
        schema_version: 1,
        display_name: "stable@wave-gating-test".into(),
        channel: "stable".into(),
        channel_ref: "stable".into(),
        fleet_resolved_hash: "test-hash".into(),
        host_set: vec![
            HostWave {
                hostname: "h1".into(),
                wave_index: 0,
                target_closure: "h1-closure".into(),
            },
            HostWave {
                hostname: "h2".into(),
                wave_index: 1,
                target_closure: "h2-closure".into(),
            },
        ],
        health_gate: HealthGate::default(),
        disruption_budgets: Vec::new(),
        meta: Meta {
            schema_version: 1,
            signed_at: Some(Utc::now()),
            ci_commit: Some("test-ci-commit".to_string()),
            signature_algorithm: None,
        },
    };
    let mut rollouts = HashMap::new();
    rollouts.insert(
        "stable".to_string(),
        Verified::unverified_for_tests(manifest, Utc::now()),
    );
    let set = SignedManifestSet {
        fleet: Verified::unverified_for_tests(fleet, Utc::now()),
        rollouts,
    };

    rt.input_tx
        .send(ReducerInput::ManifestSetUpdated(Box::new(set)))
        .await
        .unwrap();

    let rollout_id = nixfleet_proto::RolloutId::new("stable", "stable");

    // Wait for OpenRollout to materialize both Pending rows.
    {
        let db_for_poll = db.clone();
        let rollout_id_clone = rollout_id.clone();
        wait_for(Duration::from_secs(3), || {
            db_for_poll
                .host_rollout_records()
                .load(rollout_id_clone.as_str(), "h2")
                .ok()
                .flatten()
                .is_some()
        })
        .await;
    }

    // Fire a plan tick. Wave-0 host (h1) should be dispatched; wave-1
    // host (h2) should NOT, because current_wave should still be 0.
    rt.input_tx.send(ReducerInput::PlanTick).await.unwrap();

    // Wait for h1's dispatch to land.
    {
        let db_for_poll = db.clone();
        wait_for(Duration::from_secs(3), || {
            db_for_poll
                .dispatch_queue()
                .peek_for_host("h1")
                .unwrap_or(false)
        })
        .await;
    }

    // Sanity: post-OpenRollout, the rollout's current_wave MUST be 0.
    // Pre-fix this read returned 1 (= waves.len() - 1).
    let observed_current_wave = db
        .rollouts()
        .current_wave(rollout_id.as_str())
        .unwrap()
        .expect("rollout row present");
    assert_eq!(
        observed_current_wave, 0,
        "Multi-wave rollout's current_wave MUST be 0 post-OpenRollout. \
         Pre-D-027 fix, HostJoined events leaked the wave cursor forward \
         to max(host wave_indices) = waves.len() - 1 = 1, vacuously \
         passing wave-1 hosts through the wave-promotion gate.",
    );

    // The critical assertion: wave-1 host h2 MUST NOT be dispatched.
    // Give the planner a generous window to (incorrectly) emit a
    // dispatch — pre-fix this would have already happened.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let h2_dispatched = db.dispatch_queue().peek_for_host("h2").unwrap_or(false);
    assert!(
        !h2_dispatched,
        "h2 (wave 1) MUST NOT be dispatched while h1 (wave 0) is still \
         Pending. wave-promotion gate must block.",
    );

    cancel.cancel();
    drop(rt);
}

/// **D-007 regression test.** D-006's fix made the planner use
/// `manifest.channel_ref` as the rollout_id, which fixed the immediate
/// mismatch but introduced a new bug: two channels sharing a
/// `channel_ref` (the architectural point of multi-channel cascading
/// from a single git push) would collide on the rollout PK. D-007
/// lifted `RolloutId` to a newtype with canonical
/// `"{channel}@{channel_ref}"` construction (RFC-0008 §6.3 amendment
/// `0320c2fa`).
///
/// This test deliberately uses two channels (`stable`, `edge`) sharing
/// a single `channel_ref` (`"deadbeef"`) — the topology D-006's
/// fixtures missed. The fix's payoff is two distinct
/// `rollouts.rollout_id` rows + per-host-per-rollout dispatches with
/// no collision. Without the fix, the second `OpenRollout` would
/// `INSERT OR IGNORE` against the first's row and h2 (on `edge`) would
/// never get a Pending record.
#[tokio::test]
async fn plan_next_distinguishes_rollouts_when_two_channels_share_channel_ref() {
    let state = make_state();
    let db = state.db.clone().unwrap();
    let cancel = CancellationToken::new();
    let clock = Arc::new(SystemClock::new());
    let rt = runtime::spawn(cancel.clone(), state.clone(), clock);

    let fleet = FleetBuilder::new()
        .host("h1", "stable")
        .host_closure("h1", "h1-closure")
        .host("h2", "edge")
        .host_closure("h2", "h2-closure")
        .build();

    // The load-bearing piece: two channels share one channel_ref
    // (`"deadbeef"`). D-007's RolloutId::new(channel, channel_ref)
    // disambiguates: rollout_id becomes `"stable@deadbeef"` for one
    // and `"edge@deadbeef"` for the other.
    let stable_manifest = RolloutManifest {
        schema_version: 1,
        display_name: "stable@deadbeef".into(),
        channel: "stable".into(),
        channel_ref: "deadbeef".into(),
        fleet_resolved_hash: "test-hash".into(),
        host_set: vec![HostWave {
            hostname: "h1".into(),
            wave_index: 0,
            target_closure: "h1-closure".into(),
        }],
        health_gate: HealthGate::default(),
        disruption_budgets: Vec::new(),
        meta: Meta {
            schema_version: 1,
            signed_at: Some(Utc::now()),
            ci_commit: Some("test-ci-commit".to_string()),
            signature_algorithm: None,
        },
    };
    let edge_manifest = RolloutManifest {
        schema_version: 1,
        display_name: "edge@deadbeef".into(),
        channel: "edge".into(),
        channel_ref: "deadbeef".into(),
        fleet_resolved_hash: "test-hash".into(),
        host_set: vec![HostWave {
            hostname: "h2".into(),
            wave_index: 0,
            target_closure: "h2-closure".into(),
        }],
        health_gate: HealthGate::default(),
        disruption_budgets: Vec::new(),
        meta: Meta {
            schema_version: 1,
            signed_at: Some(Utc::now()),
            ci_commit: Some("test-ci-commit".to_string()),
            signature_algorithm: None,
        },
    };
    let mut rollouts = HashMap::new();
    rollouts.insert(
        "stable".to_string(),
        Verified::unverified_for_tests(stable_manifest, Utc::now()),
    );
    rollouts.insert(
        "edge".to_string(),
        Verified::unverified_for_tests(edge_manifest, Utc::now()),
    );
    let set = SignedManifestSet {
        fleet: Verified::unverified_for_tests(fleet, Utc::now()),
        rollouts,
    };

    rt.input_tx
        .send(ReducerInput::ManifestSetUpdated(Box::new(set)))
        .await
        .unwrap();

    // Both Pending rows must land — one per rollout, no collision.
    // Pre-D-007 the second INSERT would `INSERT OR IGNORE` against the
    // first (same rollout_id="deadbeef") and h2 would never get a
    // Pending row.
    let stable_rid = nixfleet_proto::RolloutId::new("stable", "deadbeef");
    let edge_rid = nixfleet_proto::RolloutId::new("edge", "deadbeef");
    {
        let db_for_poll = db.clone();
        let stable_rid_c = stable_rid.clone();
        let edge_rid_c = edge_rid.clone();
        wait_for(Duration::from_secs(3), move || {
            db_for_poll
                .host_rollout_records()
                .load(stable_rid_c.as_str(), "h1")
                .ok()
                .flatten()
                .is_some()
                && db_for_poll
                    .host_rollout_records()
                    .load(edge_rid_c.as_str(), "h2")
                    .ok()
                    .flatten()
                    .is_some()
        })
        .await;
    }

    // Distinct `rollouts` rows under the canonical `channel@channel_ref`
    // identity (RFC-0008 §6.3): two channels sharing a channel_ref still
    // map to disjoint rollout_id values, so each gets its own row.
    let stable_row = db
        .rollouts()
        .state(stable_rid.as_str())
        .unwrap()
        .expect("stable@deadbeef row exists");
    let edge_row = db
        .rollouts()
        .state(edge_rid.as_str())
        .unwrap()
        .expect("edge@deadbeef row exists");
    assert_eq!(stable_row.rollout_id.as_str(), "stable@deadbeef");
    assert_eq!(edge_row.rollout_id.as_str(), "edge@deadbeef");
    assert_eq!(stable_row.channel, "stable");
    assert_eq!(edge_row.channel, "edge");

    // PlanTick produces QueueDispatch for both hosts.
    rt.input_tx.send(ReducerInput::PlanTick).await.unwrap();

    let db_for_poll = db.clone();
    wait_for(Duration::from_secs(3), move || {
        db_for_poll
            .dispatch_queue()
            .peek_for_host("h1")
            .unwrap_or(false)
            && db_for_poll
                .dispatch_queue()
                .peek_for_host("h2")
                .unwrap_or(false)
    })
    .await;

    cancel.cancel();
    drop(rt);
}

/// **D-008 Test A — cascade-deadlock regression (end-to-end).**
///
/// Topology: two channels chained by `channel_edges` (`stable → edge`),
/// each owning one host. Both hosts share tag `"ws"` and a single
/// disruption budget with `maxInFlight = 1`.
///
/// Pre-fix shape: after `OpenRollout` creates Pending rows for h1 and h2,
/// `disruption_budget` counted `Pending` as in-flight. h1's gate check
/// saw `in_flight = 2 > max = 1` and deferred — even though h1 is the
/// predecessor channel's first wave-0 host and nothing else is blocking
/// it. h1 never advanced past Pending, channel stable never converged,
/// channel_edges kept h2 blocked too: full cascade deadlock.
///
/// Post-fix (`planner_gates::disruption_budget` §1): Pending is excluded
/// from `is_in_flight`. h1 sees `in_flight = 0`, dispatches on the first
/// tick. h2 stays deferred via `channel_edges` until stable converges —
/// the *correct* gate, not the spurious self-block.
#[tokio::test]
async fn d008_cascade_predecessor_dispatches_on_first_tick() {
    let state = make_state();
    let db = state.db.clone().unwrap();
    let cancel = CancellationToken::new();
    let clock = Arc::new(SystemClock::new());
    let rt = runtime::spawn(cancel.clone(), state.clone(), clock);

    let fleet = FleetBuilder::new()
        .host("h1", "stable")
        .host_closure("h1", "h1-closure")
        .host_tag("h1", "ws")
        .host("h2", "edge")
        .host_closure("h2", "h2-closure")
        .host_tag("h2", "ws")
        .channel_edge("stable", "edge")
        .build();

    // Same Selector across both manifests so the gate's cross-rollout
    // in-flight sum (matched by selector equality) picks up both hosts
    // when computing budget exhaustion (the load-bearing part of the
    // pre-fix deadlock).
    let selector = Selector {
        tags: vec!["ws".into()],
        ..Default::default()
    };
    let budgets = vec![RolloutBudget {
        selector: selector.clone(),
        hosts: vec!["h1".into(), "h2".into()],
        max_in_flight: Some(1),
        max_in_flight_pct: None,
    }];

    let stable_manifest = RolloutManifest {
        schema_version: 1,
        display_name: "stable@d008a".into(),
        channel: "stable".into(),
        channel_ref: "stable-ref".into(),
        fleet_resolved_hash: "test-hash".into(),
        host_set: vec![HostWave {
            hostname: "h1".into(),
            wave_index: 0,
            target_closure: "h1-closure".into(),
        }],
        health_gate: HealthGate::default(),
        disruption_budgets: budgets.clone(),
        meta: Meta {
            schema_version: 1,
            signed_at: Some(Utc::now()),
            ci_commit: Some("test-ci-commit".into()),
            signature_algorithm: None,
        },
    };
    let edge_manifest = RolloutManifest {
        schema_version: 1,
        display_name: "edge@d008a".into(),
        channel: "edge".into(),
        channel_ref: "edge-ref".into(),
        fleet_resolved_hash: "test-hash".into(),
        host_set: vec![HostWave {
            hostname: "h2".into(),
            wave_index: 0,
            target_closure: "h2-closure".into(),
        }],
        health_gate: HealthGate::default(),
        disruption_budgets: budgets,
        meta: Meta {
            schema_version: 1,
            signed_at: Some(Utc::now()),
            ci_commit: Some("test-ci-commit".into()),
            signature_algorithm: None,
        },
    };
    let mut rollouts = HashMap::new();
    rollouts.insert(
        "stable".to_string(),
        Verified::unverified_for_tests(stable_manifest, Utc::now()),
    );
    rollouts.insert(
        "edge".to_string(),
        Verified::unverified_for_tests(edge_manifest, Utc::now()),
    );
    let set = SignedManifestSet {
        fleet: Verified::unverified_for_tests(fleet, Utc::now()),
        rollouts,
    };

    rt.input_tx
        .send(ReducerInput::ManifestSetUpdated(Box::new(set)))
        .await
        .unwrap();

    let stable_rid = nixfleet_proto::RolloutId::new("stable", "stable-ref");
    let edge_rid = nixfleet_proto::RolloutId::new("edge", "edge-ref");
    {
        let db_for_poll = db.clone();
        let stable_rid_c = stable_rid.clone();
        let edge_rid_c = edge_rid.clone();
        wait_for(Duration::from_secs(3), move || {
            db_for_poll
                .host_rollout_records()
                .load(stable_rid_c.as_str(), "h1")
                .ok()
                .flatten()
                .is_some()
                && db_for_poll
                    .host_rollout_records()
                    .load(edge_rid_c.as_str(), "h2")
                    .ok()
                    .flatten()
                    .is_some()
        })
        .await;
    }

    rt.input_tx.send(ReducerInput::PlanTick).await.unwrap();

    // Post-fix payoff: h1 dispatches on the first tick. Pre-fix it
    // deferred forever (in_flight = 2 from Pending(h1) + Pending(h2)).
    {
        let db_for_poll = db.clone();
        wait_for(Duration::from_secs(3), move || {
            db_for_poll
                .dispatch_queue()
                .peek_for_host("h1")
                .unwrap_or(false)
        })
        .await;
    }

    // h2 is correctly blocked by channel_edges (stable hasn't converged
    // yet) — proves the post-fix gate stack still cascades properly,
    // not just that we widened the budget into a no-op.
    assert!(
        !db.dispatch_queue().peek_for_host("h2").unwrap_or(true),
        "h2 (gated channel) must remain undispatched until predecessor converges",
    );

    cancel.cancel();
    drop(rt);
}

/// **D-008 Test B — within-tick over-commit regression (end-to-end).**
///
/// Three Pending hosts on one budget with `maxInFlight = 1` in a single
/// `plan_next` tick. With D-008 §1's `Pending`-not-in-flight fix alone,
/// all three would have seen `in_flight = 0` (none have transitioned
/// to Activating yet — the applier hasn't run inside the same plan_next)
/// and all three would have been QueueDispatch'd: a 3× over-commit, a
/// regression worse than the original deadlock.
///
/// The paired fix is the within-tick accumulator (D-008 §2 /
/// `planner.rs` host loop). The first QueueDispatch increments
/// `tick_dispatched[selector] → 1`; the next host's gate-check sees
/// `tick_count + in_flight = 1 ≥ max = 1` and defers. Same for the
/// third. Exactly one QueueDispatch + two DeferDispatch{gate=
/// disruption-budget}.
#[tokio::test]
async fn d008_within_tick_accumulator_prevents_over_commit() {
    let state = make_state();
    let db = state.db.clone().unwrap();
    let cancel = CancellationToken::new();
    let clock = Arc::new(SystemClock::new());
    let rt = runtime::spawn(cancel.clone(), state.clone(), clock);

    let fleet = FleetBuilder::new()
        .host("h1", "stable")
        .host_closure("h1", "h1-closure")
        .host_tag("h1", "ws")
        .host("h2", "stable")
        .host_closure("h2", "h2-closure")
        .host_tag("h2", "ws")
        .host("h3", "stable")
        .host_closure("h3", "h3-closure")
        .host_tag("h3", "ws")
        .build();

    let selector = Selector {
        tags: vec!["ws".into()],
        ..Default::default()
    };
    let budgets = vec![RolloutBudget {
        selector,
        hosts: vec!["h1".into(), "h2".into(), "h3".into()],
        max_in_flight: Some(1),
        max_in_flight_pct: None,
    }];

    let manifest = RolloutManifest {
        schema_version: 1,
        display_name: "stable@d008b".into(),
        channel: "stable".into(),
        channel_ref: "stable-ref".into(),
        fleet_resolved_hash: "test-hash".into(),
        host_set: vec![
            HostWave {
                hostname: "h1".into(),
                wave_index: 0,
                target_closure: "h1-closure".into(),
            },
            HostWave {
                hostname: "h2".into(),
                wave_index: 0,
                target_closure: "h2-closure".into(),
            },
            HostWave {
                hostname: "h3".into(),
                wave_index: 0,
                target_closure: "h3-closure".into(),
            },
        ],
        health_gate: HealthGate::default(),
        disruption_budgets: budgets,
        meta: Meta {
            schema_version: 1,
            signed_at: Some(Utc::now()),
            ci_commit: Some("test-ci-commit".into()),
            signature_algorithm: None,
        },
    };
    let mut rollouts = HashMap::new();
    rollouts.insert(
        "stable".to_string(),
        Verified::unverified_for_tests(manifest, Utc::now()),
    );
    let set = SignedManifestSet {
        fleet: Verified::unverified_for_tests(fleet, Utc::now()),
        rollouts,
    };

    rt.input_tx
        .send(ReducerInput::ManifestSetUpdated(Box::new(set)))
        .await
        .unwrap();

    let rollout_id = nixfleet_proto::RolloutId::new("stable", "stable-ref");
    {
        let db_for_poll = db.clone();
        let rid_c = rollout_id.clone();
        wait_for(Duration::from_secs(3), move || {
            ["h1", "h2", "h3"].iter().all(|h| {
                db_for_poll
                    .host_rollout_records()
                    .load(rid_c.as_str(), h)
                    .ok()
                    .flatten()
                    .is_some()
            })
        })
        .await;
    }

    rt.input_tx.send(ReducerInput::PlanTick).await.unwrap();

    // Wait until all three hosts have been handled (queued or deferred).
    // The total queued + budget-deferred must reach 3.
    {
        let db_for_poll = db.clone();
        wait_for(Duration::from_secs(3), move || {
            let queued = ["h1", "h2", "h3"]
                .iter()
                .filter(|h| {
                    db_for_poll
                        .dispatch_queue()
                        .peek_for_host(h)
                        .unwrap_or(false)
                })
                .count();
            let deferred_budget = db_for_poll
                .event_log()
                .query_by_kind(
                    nixfleet_control_plane::db::event_log::EventLogKind::GateDecision,
                    100,
                )
                .map(|rows| {
                    rows.iter()
                        .filter(|r| r.payload.contains("\"gate\":\"disruption-budget\""))
                        .count()
                })
                .unwrap_or(0);
            queued + deferred_budget >= 3
        })
        .await;
    }

    let queued: Vec<&str> = ["h1", "h2", "h3"]
        .into_iter()
        .filter(|h| db.dispatch_queue().peek_for_host(h).unwrap_or(false))
        .collect();
    assert_eq!(
        queued.len(),
        1,
        "exactly one QueueDispatch expected (D-008 §2 within-tick accumulator); got {queued:?}",
    );

    let gate_rows = db
        .event_log()
        .query_by_kind(
            nixfleet_control_plane::db::event_log::EventLogKind::GateDecision,
            100,
        )
        .expect("query gate_decision rows");
    let deferred_budget: Vec<_> = gate_rows
        .iter()
        .filter(|r| r.payload.contains("\"gate\":\"disruption-budget\""))
        .collect();
    assert_eq!(
        deferred_budget.len(),
        2,
        "exactly two DeferDispatch{{gate=disruption-budget}} expected; got payloads={:?}",
        deferred_budget
            .iter()
            .map(|r| &r.payload)
            .collect::<Vec<_>>(),
    );

    cancel.cancel();
    drop(rt);
}
