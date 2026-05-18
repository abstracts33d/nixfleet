//! Coverage for the new-shape gates. Per-gate happy path + critical
//! blocked path. Integration with `plan_next` lives in
//! `crate::planner::tests`.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, TimeZone, Utc};
use nixfleet_proto::testing::FleetBuilder;
use nixfleet_proto::{FleetResolved, RolloutBudget, Selector};
use nixfleet_state_machine::{HostRolloutState, HostState};

use crate::planner_gates;
use crate::planner_gates::GateBlock;
use crate::planner_types::{FleetState, RolloutSummary, SignedManifestSet};
use crate::verify::Verified;

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, 16, 1, 0, 0).unwrap()
}

fn signed_manifest_set(fleet: FleetResolved) -> SignedManifestSet {
    SignedManifestSet {
        fleet: Verified::unverified_for_tests(fleet, t0()),
        rollouts: HashMap::new(),
    }
}

fn empty_fleet_state() -> FleetState {
    FleetState {
        host_states: HashMap::new(),
        rollouts: HashMap::new(),
        outstanding_failing_enforce_probes: HashMap::new(),
    }
}

fn pending_host(rollout_id: &str, host: &str, channel: &str) -> HostRolloutState {
    HostRolloutState::new_pending(
        rollout_id.into(),
        host.into(),
        channel.into(),
        format!("hash-{host}"),
        t0(),
        t0() + chrono::Duration::minutes(5),
    )
}

fn summary(
    id: &str,
    channel: &str,
    current_wave: u32,
    budgets: Vec<RolloutBudget>,
    terminal: bool,
) -> RolloutSummary {
    RolloutSummary {
        rollout_id: id.into(),
        channel: channel.into(),
        target_ref: id.into(),
        opened_at: t0(),
        terminal_at: terminal.then(|| t0() + chrono::Duration::minutes(10)),
        current_wave,
        budgets,
    }
}

// ─────────────────────────────────────────────────────────────────────────
// quarantine
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn quarantine_blocks_when_target_in_set() {
    let mut q = HashMap::new();
    let mut set = HashSet::new();
    set.insert("bad".to_string());
    q.insert("stable".to_string(), set);

    let block = planner_gates::quarantine::check(&q, &"stable".to_string(), &"bad".to_string());
    assert!(matches!(block, Some(GateBlock::Quarantined { .. })));
}

#[test]
fn quarantine_passes_when_target_clean() {
    let q = HashMap::new();
    let block =
        planner_gates::quarantine::check(&q, &"stable".to_string(), &"anything".to_string());
    assert!(block.is_none());
}

// ─────────────────────────────────────────────────────────────────────────
// channel_edges
// ─────────────────────────────────────────────────────────────────────────

/// Build a rollout-manifest-bearing SignedManifestSet from a fleet builder.
/// Used by the channel-edges tests below — the gate's predicate keys
/// on `manifests.rollouts.get(predecessor).inner().channel_ref` to
/// construct the canonical RolloutId for `fleet_state.rollouts` lookup.
fn signed_set_with_rollout(
    fleet: FleetResolved,
    channel: &str,
    channel_ref: &str,
) -> SignedManifestSet {
    let mut set = signed_manifest_set(fleet);
    let rollout = nixfleet_proto::RolloutManifest {
        schema_version: 1,
        display_name: format!("{channel}@{channel_ref}"),
        channel: channel.into(),
        channel_ref: channel_ref.into(),
        fleet_resolved_hash: String::new(),
        host_set: Vec::new(),
        health_gate: nixfleet_proto::HealthGate::default(),
        disruption_budgets: Vec::new(),
        meta: nixfleet_proto::Meta {
            schema_version: 1,
            signed_at: Some(t0()),
            ci_commit: None,
            signature_algorithm: Some("ed25519".into()),
        },
    };
    set.rollouts.insert(
        channel.into(),
        Verified::unverified_for_tests(rollout, t0()),
    );
    set
}

#[test]
fn channel_edges_blocks_when_predecessor_active() {
    let fleet = FleetBuilder::new()
        .host("h1", "stable")
        .host("h2", "infra")
        .channel_edge("infra", "stable") // gates="infra", gated="stable"
        .build();
    let manifests = signed_set_with_rollout(fleet, "infra", "r-infra");

    let mut fs = empty_fleet_state();
    let rollout_id = nixfleet_proto::RolloutId::new("infra", "r-infra");
    fs.rollouts.insert(
        rollout_id.clone(),
        summary(rollout_id.as_str(), "infra", 0, vec![], false),
    );

    let block = planner_gates::channel_edges::check(&fs, &manifests, &"stable".to_string());
    assert!(matches!(block, Some(GateBlock::ChannelEdges { .. })));
}

#[test]
fn channel_edges_passes_when_predecessor_terminal() {
    // LOADBEARING: a Terminal predecessor MUST NOT block its successor.
    // The gate reads `terminal_at` directly off the `RolloutSummary` in
    // `fleet_state.rollouts`; a `Some(_)` value means converged → pass.
    let fleet = FleetBuilder::new()
        .host("h1", "stable")
        .host("h2", "infra")
        .channel_edge("infra", "stable")
        .build();
    let manifests = signed_set_with_rollout(fleet, "infra", "r-infra");

    let mut fs = empty_fleet_state();
    let rollout_id = nixfleet_proto::RolloutId::new("infra", "r-infra");
    fs.rollouts.insert(
        rollout_id.clone(),
        summary(rollout_id.as_str(), "infra", 0, vec![], true),
    );

    let block = planner_gates::channel_edges::check(&fs, &manifests, &"stable".to_string());
    assert!(
        block.is_none(),
        "Terminal predecessor MUST NOT block successor; got: {block:?}",
    );
}

#[test]
fn channel_edges_blocks_when_predecessor_manifest_present_but_rollout_not_opened() {
    // Fresh-boot protection: predecessor's manifest is published but
    // the planner hasn't opened the rollout yet (e.g. cold-start
    // window between manifest verify and the first PlanTick that
    // emits OpenRollout). Successor must block conservatively.
    let fleet = FleetBuilder::new()
        .host("h1", "stable")
        .host("h2", "infra")
        .channel_edge("infra", "stable")
        .build();
    let manifests = signed_set_with_rollout(fleet, "infra", "r-infra");

    // fs.rollouts is empty: no RolloutSummary for `infra@r-infra` yet.
    let fs = empty_fleet_state();

    let block = planner_gates::channel_edges::check(&fs, &manifests, &"stable".to_string());
    assert!(
        matches!(block, Some(GateBlock::ChannelEdges { .. })),
        "fresh-boot protection: predecessor manifest present but rollout not opened MUST block; got: {block:?}",
    );
}

#[test]
fn channel_edges_passes_when_no_predecessor_manifest() {
    // No verified manifest for the predecessor channel ⇒ no work
    // declared on it ⇒ no ordering constraint to enforce.
    let fleet = FleetBuilder::new()
        .host("h1", "stable")
        .host("h2", "infra")
        .channel_edge("infra", "stable")
        .build();
    let manifests = signed_manifest_set(fleet); // empty rollouts map

    let fs = empty_fleet_state();

    let block = planner_gates::channel_edges::check(&fs, &manifests, &"stable".to_string());
    assert!(
        block.is_none(),
        "no predecessor manifest ⇒ no work declared ⇒ gate passes; got: {block:?}",
    );
}

// ─────────────────────────────────────────────────────────────────────────
// wave_promotion
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn wave_promotion_blocks_when_host_wave_exceeds_current() {
    let fleet = FleetBuilder::new()
        .host("h1", "stable")
        .host("h2", "stable")
        .wave("stable", &["h1"])
        .wave("stable", &["h2"])
        .build();
    let manifests = signed_manifest_set(fleet);

    let mut fs = empty_fleet_state();
    fs.rollouts
        .insert("r1".into(), summary("r1", "stable", 0, vec![], false));

    let block =
        planner_gates::wave_promotion::check(&fs, &manifests, &"h2".to_string(), &"r1".into());
    assert!(matches!(block, Some(GateBlock::WavePromotion { .. })));
}

#[test]
fn wave_promotion_passes_when_in_current_wave() {
    let fleet = FleetBuilder::new()
        .host("h1", "stable")
        .wave("stable", &["h1"])
        .build();
    let manifests = signed_manifest_set(fleet);

    let mut fs = empty_fleet_state();
    fs.rollouts
        .insert("r1".into(), summary("r1", "stable", 0, vec![], false));

    let block =
        planner_gates::wave_promotion::check(&fs, &manifests, &"h1".to_string(), &"r1".into());
    assert!(block.is_none());
}

// ─────────────────────────────────────────────────────────────────────────
// host_edges
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn host_edges_blocks_when_gating_host_not_converged() {
    let fleet = FleetBuilder::new()
        .host("h1", "stable")
        .host("h2", "stable")
        .edge("h1", "h2") // gates="h1", gated="h2"
        .build();
    let manifests = signed_manifest_set(fleet);

    let mut fs = empty_fleet_state();
    fs.rollouts
        .insert("r1".into(), summary("r1", "stable", 0, vec![], false));
    let mut h1 = pending_host("r1", "h1", "stable");
    h1.state = HostState::Soaking;
    fs.host_states.insert(("r1".into(), "h1".into()), h1);

    let block = planner_gates::host_edges::check(&fs, &manifests, &"h2".to_string(), &"r1".into());
    assert!(matches!(block, Some(GateBlock::HostEdge { .. })));
}

#[test]
fn host_edges_passes_when_gating_host_converged() {
    let fleet = FleetBuilder::new()
        .host("h1", "stable")
        .host("h2", "stable")
        .edge("h1", "h2")
        .build();
    let manifests = signed_manifest_set(fleet);

    let mut fs = empty_fleet_state();
    fs.rollouts
        .insert("r1".into(), summary("r1", "stable", 0, vec![], false));
    let mut h1 = pending_host("r1", "h1", "stable");
    h1.state = HostState::Converged;
    fs.host_states.insert(("r1".into(), "h1".into()), h1);

    let block = planner_gates::host_edges::check(&fs, &manifests, &"h2".to_string(), &"r1".into());
    assert!(block.is_none());
}

// ─────────────────────────────────────────────────────────────────────────
// disruption_budget
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn disruption_budget_blocks_at_cap() {
    let selector = Selector {
        tags: vec!["workstation".into()],
        tags_any: vec![],
        hosts: vec![],
        channel: None,
        all: false,
    };
    let budget = RolloutBudget {
        selector: selector.clone(),
        hosts: vec!["h1".into(), "h2".into()],
        max_in_flight: Some(1),
        max_in_flight_pct: None,
    };

    let mut fs = empty_fleet_state();
    fs.rollouts.insert(
        "r1".into(),
        summary("r1", "stable", 0, vec![budget.clone()], false),
    );
    // h1 is in-flight
    let mut h1 = pending_host("r1", "h1", "stable");
    h1.state = HostState::Activating;
    fs.host_states.insert(("r1".into(), "h1".into()), h1);

    let block = planner_gates::disruption_budget::check(
        &fs,
        &"r1".into(),
        &"h2".to_string(),
        &std::collections::HashMap::new(),
    );
    assert!(matches!(block, Some(GateBlock::DisruptionBudget { .. })));
}

#[test]
fn disruption_budget_passes_under_cap() {
    let selector = Selector {
        tags: vec!["workstation".into()],
        tags_any: vec![],
        hosts: vec![],
        channel: None,
        all: false,
    };
    let budget = RolloutBudget {
        selector,
        hosts: vec!["h1".into(), "h2".into()],
        max_in_flight: Some(2),
        max_in_flight_pct: None,
    };

    let mut fs = empty_fleet_state();
    fs.rollouts
        .insert("r1".into(), summary("r1", "stable", 0, vec![budget], false));
    let mut h1 = pending_host("r1", "h1", "stable");
    h1.state = HostState::Activating;
    fs.host_states.insert(("r1".into(), "h1".into()), h1);

    let block = planner_gates::disruption_budget::check(
        &fs,
        &"r1".into(),
        &"h2".to_string(),
        &std::collections::HashMap::new(),
    );
    assert!(block.is_none());
}

#[test]
fn disruption_budget_counts_only_in_flight_states() {
    // Converged hosts must NOT count against the budget — they're done.
    let selector = Selector {
        tags: vec!["w".into()],
        tags_any: vec![],
        hosts: vec![],
        channel: None,
        all: false,
    };
    let budget = RolloutBudget {
        selector,
        hosts: vec!["h1".into(), "h2".into()],
        max_in_flight: Some(1),
        max_in_flight_pct: None,
    };

    let mut fs = empty_fleet_state();
    fs.rollouts
        .insert("r1".into(), summary("r1", "stable", 0, vec![budget], false));
    let mut h1 = pending_host("r1", "h1", "stable");
    h1.state = HostState::Converged;
    fs.host_states.insert(("r1".into(), "h1".into()), h1);

    // h2 should pass: h1 is Converged, not in-flight.
    let block = planner_gates::disruption_budget::check(
        &fs,
        &"r1".into(),
        &"h2".to_string(),
        &std::collections::HashMap::new(),
    );
    assert!(block.is_none());
}

// ─────────────────────────────────────────────────────────────────────────
// compliance_wave
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn compliance_wave_blocks_when_earlier_wave_has_outstanding_failures() {
    let fleet = FleetBuilder::new()
        .host("h1", "stable")
        .host("h2", "stable")
        .wave("stable", &["h1"])
        .wave("stable", &["h2"])
        .build();
    let manifests = signed_manifest_set(fleet);

    let mut fs = empty_fleet_state();
    let mut per_host = HashMap::new();
    per_host.insert("h1".to_string(), 2_usize);
    fs.outstanding_failing_enforce_probes
        .insert("r1".into(), per_host);

    let block =
        planner_gates::compliance_wave::check(&fs, &manifests, &"h2".to_string(), &"r1".into());
    assert!(matches!(block, Some(GateBlock::ComplianceWave { .. })));
}

#[test]
fn compliance_wave_passes_when_no_failing_enforce_probes() {
    let fleet = FleetBuilder::new()
        .host("h1", "stable")
        .host("h2", "stable")
        .wave("stable", &["h1"])
        .wave("stable", &["h2"])
        .build();
    let manifests = signed_manifest_set(fleet);

    // Empty outstanding_failing_enforce_probes -> gate is pass-through.
    let fs = empty_fleet_state();

    let block =
        planner_gates::compliance_wave::check(&fs, &manifests, &"h2".to_string(), &"r1".into());
    assert!(block.is_none());
}

// ─────────────────────────────────────────────────────────────────────────
// D-008 regression: `Pending` is NOT in-flight; within-tick accumulator
// prevents over-commit
// ─────────────────────────────────────────────────────────────────────────

/// **D-008 Test A (gate-level cascade unblock).** Pre-D-008, a freshly
/// `OpenRollout`'d host whose first state is `Pending` counted as
/// in-flight against its own budget. With `max_in_flight = 1`, the
/// host self-blocked: the gate returned `DisruptionBudget` for the
/// same host that was being checked, because its Pending status
/// saturated the budget. The end-to-end manifestation was a
/// cascade-deadlock on chained channels (Test A in the integration
/// suite); this unit test pins the gate-level fix.
#[test]
fn disruption_budget_pending_host_does_not_self_block() {
    let selector = Selector {
        tags: vec!["workstation".into()],
        tags_any: vec![],
        hosts: vec![],
        channel: None,
        all: false,
    };
    let budget = RolloutBudget {
        selector: selector.clone(),
        hosts: vec!["h1".into()],
        max_in_flight: Some(1),
        max_in_flight_pct: None,
    };

    let mut fs = empty_fleet_state();
    fs.rollouts
        .insert("r1".into(), summary("r1", "stable", 0, vec![budget], false));
    // h1 is Pending — the exact pre-D-008 self-block scenario.
    let h1 = pending_host("r1", "h1", "stable");
    fs.host_states.insert(("r1".into(), "h1".into()), h1);

    let block = planner_gates::disruption_budget::check(
        &fs,
        &"r1".into(),
        &"h1".to_string(),
        &std::collections::HashMap::new(),
    );
    assert!(
        block.is_none(),
        "Pending host MUST NOT count as in-flight (D-008 root-cause guard)"
    );
}

/// **D-008 Test B (within-tick accumulator).** Removing `Pending`
/// from `is_in_flight` is a partial fix: within one `plan_next()`
/// tick, the planner would otherwise wave through N Pending hosts on
/// a `max_in_flight = 1` budget because none have transitioned to
/// Activating yet. The accumulator is the second half of the fix —
/// each emitted `QueueDispatch` increments a per-budget counter that
/// the next iteration's gate-check consults. This test exercises the
/// accumulator directly.
#[test]
fn disruption_budget_within_tick_accumulator_blocks_over_commit() {
    let selector = Selector {
        tags: vec!["workstation".into()],
        tags_any: vec![],
        hosts: vec![],
        channel: None,
        all: false,
    };
    let budget = RolloutBudget {
        selector: selector.clone(),
        hosts: vec!["h2".into()],
        max_in_flight: Some(1),
        max_in_flight_pct: None,
    };

    let mut fs = empty_fleet_state();
    fs.rollouts
        .insert("r1".into(), summary("r1", "stable", 0, vec![budget], false));
    // No live in-flight host — the gate sees in_flight_count = 0.
    // But the accumulator says one dispatch has already been emitted
    // for this budget in the current tick. Gate must block h2.
    let mut tick_dispatched = std::collections::HashMap::new();
    tick_dispatched.insert(selector.clone(), 1u32);

    let block = planner_gates::disruption_budget::check(
        &fs,
        &"r1".into(),
        &"h2".to_string(),
        &tick_dispatched,
    );
    assert!(
        matches!(block, Some(GateBlock::DisruptionBudget { .. })),
        "within-tick count 1 + live 0 ≥ max 1 ⇒ block (D-008 §2 over-commit guard)"
    );
}
