//! End-to-end soak-loop integration test (gap #2 closing the
//! cycle).
//!
//! Exercises every piece the cycle wired up in one scenario:
//!
//! 1. `record_pending_confirm` + `confirm_pending` simulate the
//!    confirm handler's success path.
//! 2. `record_host_healthy` stamps `last_healthy_since` (gap #2
//!    step 1).
//! 3. `active_rollouts_snapshot` projects the DB into the
//!    reconciler's observed-state shape (step 2).
//! 4. `observed_projection::project` builds the Observed struct.
//! 5. `nixfleet_reconciler::reconcile` decides + emits
//!    `Action::SoakHost` because the soak window has elapsed
//!    (step 3).
//! 6. `mark_host_soaked` applies the SoakHost action (step 3 CP
//!    handler).
//! 7. The next snapshot reflects `host_state = 'Soaked'` and the
//!    next reconcile tick fires `ConvergeRollout` (single-wave
//!    fleet, so promotion = convergence).
//!
//! Each piece has its own unit / fixture coverage; this test
//! proves they compose. If a future refactor breaks the chain at
//! any join, this test fires.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use nixfleet_control_plane::db::Db;
use nixfleet_control_plane::observed_projection;
use nixfleet_proto::fleet_resolved::Meta;
use nixfleet_proto::{Channel, Compliance, FleetResolved, Host, Wave};
use nixfleet_reconciler::{reconcile, Action};
use tempfile::TempDir;

fn fleet_with_single_wave_host(hostname: &str, closure: &str, soak_minutes: u32) -> FleetResolved {
    let mut hosts = HashMap::new();
    hosts.insert(
        hostname.to_string(),
        Host {
            system: "x86_64-linux".to_string(),
            tags: vec![],
            channel: "stable".to_string(),
            closure_hash: Some(closure.to_string()),
            pubkey: None,
        },
    );
    let mut channels = HashMap::new();
    channels.insert(
        "stable".to_string(),
        Channel {
            rollout_policy: "default".to_string(),
            reconcile_interval_minutes: 5,
            freshness_window: 60,
            signing_interval_minutes: 30,
            compliance: Compliance {
                frameworks: vec![],
                mode: "disabled".to_string(),
            },
        },
    );
    let mut waves = HashMap::new();
    waves.insert(
        "stable".to_string(),
        vec![Wave {
            hosts: vec![hostname.to_string()],
            soak_minutes,
        }],
    );
    FleetResolved {
        schema_version: 1,
        hosts,
        channels,
        rollout_policies: HashMap::new(),
        waves,
        edges: vec![],
        disruption_budgets: vec![],
        meta: Meta {
            schema_version: 1,
            signed_at: Some(Utc::now()),
            ci_commit: Some("abc12345".to_string()),
            signature_algorithm: None,
        },
    }
}

#[test]
fn soak_loop_end_to_end_healthy_to_soaked_to_converged() {
    let tmp = TempDir::new().unwrap();
    let db = Arc::new(Db::open(&tmp.path().join("state.db")).unwrap());
    db.migrate().unwrap();

    // Step A: simulate the confirm handler's success path.
    // record_pending_confirm + confirm_pending mark the row
    // confirmed; record_host_healthy stamps the soak marker
    // 10 minutes in the past so the wave's 5-minute soak window
    // is elapsed at reconcile time.
    let host = "ohm";
    let rollout_id = "stable@abc12345";
    let target_closure = "deadbeef-system";
    let confirm_deadline = Utc::now() + chrono::Duration::seconds(120);
    let healthy_at = Utc::now() - chrono::Duration::minutes(10);
    let now = Utc::now();

    db.record_pending_confirm(host, rollout_id, 0, target_closure, rollout_id, confirm_deadline)
        .unwrap();
    let n = db.confirm_pending(host, rollout_id).unwrap();
    assert_eq!(n, 1, "confirm_pending must mark the row confirmed");
    db.record_host_healthy(host, rollout_id, healthy_at).unwrap();

    // Step B: project the DB state into the reconciler's
    // observed-state struct.
    let rollouts = db.active_rollouts_snapshot().unwrap();
    assert_eq!(rollouts.len(), 1, "snapshot must surface the rollout");
    assert_eq!(
        rollouts[0].host_states.get(host).map(String::as_str),
        Some("Healthy"),
        "host should be Healthy in the snapshot",
    );
    assert!(
        rollouts[0].last_healthy_since.contains_key(host),
        "soak marker must surface for projection",
    );

    let observed = observed_projection::project(&HashMap::new(), &HashMap::new(), &rollouts, HashMap::new());
    assert_eq!(observed.active_rollouts.len(), 1);

    // Step C: reconcile against a fleet whose wave has soak_minutes
    // = 5. The host has been Healthy for 10m → SoakHost emits.
    let fleet = fleet_with_single_wave_host(host, target_closure, 5);
    let actions = reconcile(&fleet, &observed, now);
    assert_eq!(actions.len(), 1, "expected exactly one action: {actions:?}");
    match &actions[0] {
        Action::SoakHost {
            rollout: r,
            host: h,
        } => {
            assert_eq!(r, rollout_id);
            assert_eq!(h, host);
        }
        other => panic!("expected Action::SoakHost, got {other:?}"),
    }

    // Step D: apply the SoakHost action — what the CP-side
    // action processor does on each tick.
    let n = db.mark_host_soaked(host, rollout_id).unwrap();
    assert_eq!(n, 1, "mark_host_soaked must transition Healthy → Soaked");

    // Step E: re-project + re-reconcile. The host now appears as
    // Soaked, the wave's `wave_all_soaked` check fires, and the
    // single-wave fleet emits ConvergeRollout.
    let rollouts2 = db.active_rollouts_snapshot().unwrap();
    assert_eq!(
        rollouts2[0].host_states.get(host).map(String::as_str),
        Some("Soaked"),
        "host must surface as Soaked after the action processor",
    );
    let observed2 = observed_projection::project(&HashMap::new(), &HashMap::new(), &rollouts2, HashMap::new());
    let actions2 = reconcile(&fleet, &observed2, now);
    assert!(
        actions2
            .iter()
            .any(|a| matches!(a, Action::ConvergeRollout { rollout } if rollout == rollout_id)),
        "single-wave Soaked host must promote to ConvergeRollout: {actions2:?}",
    );
}

#[test]
fn soak_loop_skips_when_window_not_elapsed() {
    // Companion negative-side test: same chain, but the host has
    // only been Healthy for 1m against a 5m soak window. The
    // reconciler must NOT emit SoakHost (or any other action for
    // this rollout) — the soak gate stays closed.
    let tmp = TempDir::new().unwrap();
    let db = Arc::new(Db::open(&tmp.path().join("state.db")).unwrap());
    db.migrate().unwrap();

    let host = "ohm";
    let rollout_id = "stable@abc12345";
    let target_closure = "deadbeef-system";
    let healthy_at = Utc::now() - chrono::Duration::minutes(1);
    let now = Utc::now();

    db.record_pending_confirm(
        host,
        rollout_id,
        0,
        target_closure,
        rollout_id,
        Utc::now() + chrono::Duration::seconds(120),
    )
    .unwrap();
    db.confirm_pending(host, rollout_id).unwrap();
    db.record_host_healthy(host, rollout_id, healthy_at).unwrap();

    let rollouts = db.active_rollouts_snapshot().unwrap();
    let observed = observed_projection::project(&HashMap::new(), &HashMap::new(), &rollouts, HashMap::new());
    let fleet = fleet_with_single_wave_host(host, target_closure, 5);
    let actions = reconcile(&fleet, &observed, now);
    assert!(
        actions.is_empty(),
        "soak window not elapsed; reconciler must defer: {actions:?}",
    );
}
