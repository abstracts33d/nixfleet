//! Live `Observed` projection from in-memory checkin state.
//!
//! Default source of truth for the reconcile loop. The file-backed
//! input stays as `--observed` for offline-replay debugging (operator
//! dumps in-memory state, reproduces a tick) and as a dev/test
//! fallback when no agents are checking in yet.
//!
//! Active rollouts come from the DB snapshot (step 2 of gap #2 in
//! docs/roadmap/0002-v0.2-completeness-gaps.md): the caller queries
//! `Db::active_rollouts_snapshot()` and passes the result in. When
//! no DB is configured (offline replay, early-boot), pass `&[]` and
//! the projection emits an empty `active_rollouts` — same shape as
//! before this PR.

use std::collections::HashMap;

use nixfleet_reconciler::observed::{HostState, Observed, Rollout};

use crate::db::RolloutDbSnapshot;
use crate::server::HostCheckinRecord;

/// Build an `Observed` from the in-memory checkin records, the
/// channel-refs cache, and the DB-derived rollout snapshot. Pure
/// function — caller takes the read locks and runs the DB query.
pub fn project(
    host_checkins: &HashMap<String, HostCheckinRecord>,
    channel_refs: &HashMap<String, String>,
    rollouts: &[RolloutDbSnapshot],
) -> Observed {
    let mut host_state: HashMap<String, HostState> = HashMap::new();
    for (host, record) in host_checkins {
        host_state.insert(
            host.clone(),
            HostState {
                online: true,
                current_generation: Some(record.checkin.current_generation.closure_hash.clone()),
            },
        );
    }

    // Without a dedicated rollouts table the CP can't track
    // rollout-level state directly (Planning/Executing/Halted/...);
    // step 3's reconciler arm + an `Action::SoakHost` handler will
    // be the next layer to land. For now, every snapshotted
    // rollout is surfaced as Executing so `rollout_state.rs`'s
    // wave handling actually fires for it. `current_wave` defaults
    // to 0 — the lab fleet is single-wave; multi-wave dispatch
    // tracking is part of the Phase 4 follow-up that adds the
    // hosts + rollouts tables.
    let active_rollouts: Vec<Rollout> = rollouts
        .iter()
        .map(|snap| Rollout {
            id: snap.rollout_id.clone(),
            channel: snap.channel.clone(),
            target_ref: snap.target_channel_ref.clone(),
            state: "Executing".to_string(),
            current_wave: 0,
            host_states: snap.host_states.clone(),
            last_healthy_since: snap.last_healthy_since.clone(),
        })
        .collect();

    Observed {
        channel_refs: channel_refs.clone(),
        // Not yet tracked here; reconcile against the empty case is
        // fine — the dispatch loop is what populates last-rolled-refs.
        last_rolled_refs: HashMap::new(),
        host_state,
        active_rollouts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use nixfleet_proto::agent_wire::{CheckinRequest, GenerationRef};

    fn checkin_for(hostname: &str, closure: &str) -> HostCheckinRecord {
        HostCheckinRecord {
            last_checkin: Utc::now(),
            checkin: CheckinRequest {
                hostname: hostname.to_string(),
                agent_version: "0.2.0".to_string(),
                current_generation: GenerationRef {
                    closure_hash: closure.to_string(),
                    channel_ref: None,
                    boot_id: "boot".to_string(),
                },
                pending_generation: None,
                last_evaluated_target: None,
                last_fetch_outcome: None,
                uptime_secs: Some(1),
        last_confirmed_at: None,
            },
        }
    }

    #[test]
    fn projection_reflects_each_host_checkin() {
        let mut checkins = HashMap::new();
        checkins.insert("test-host".to_string(), checkin_for("test-host", "abc"));
        checkins.insert("ohm".to_string(), checkin_for("ohm", "def"));

        let channel_refs = HashMap::from([("dev".to_string(), "deadbeef".to_string())]);
        let observed = project(&checkins, &channel_refs, &[]);

        assert_eq!(observed.host_state.len(), 2);
        assert_eq!(
            observed.host_state["test-host"].current_generation.as_deref(),
            Some("abc")
        );
        assert!(observed.host_state["test-host"].online);
        assert_eq!(observed.channel_refs["dev"], "deadbeef");
    }

    #[test]
    fn projection_with_no_checkins_yields_empty_host_state() {
        let observed = project(&HashMap::new(), &HashMap::new(), &[]);
        assert!(observed.host_state.is_empty());
        assert!(observed.channel_refs.is_empty());
        assert!(observed.active_rollouts.is_empty());
    }

    #[test]
    fn projection_surfaces_active_rollouts_from_snapshot() {
        // The snapshot's host_states + last_healthy_since flow
        // through to the Rollout struct so step 3's reconciler arm
        // can read them on the next tick.
        let now = Utc::now();
        let mut host_states = HashMap::new();
        host_states.insert("ohm".to_string(), "Healthy".to_string());
        host_states.insert("krach".to_string(), "ConfirmWindow".to_string());
        let mut last_healthy = HashMap::new();
        last_healthy.insert("ohm".to_string(), now);

        let snap = RolloutDbSnapshot {
            rollout_id: "stable@abc12345".to_string(),
            channel: "stable".to_string(),
            target_closure_hash: "system-r1".to_string(),
            target_channel_ref: "stable@abc12345".to_string(),
            host_states,
            last_healthy_since: last_healthy,
        };
        let observed = project(&HashMap::new(), &HashMap::new(), std::slice::from_ref(&snap));
        assert_eq!(observed.active_rollouts.len(), 1);
        let r = &observed.active_rollouts[0];
        assert_eq!(r.id, "stable@abc12345");
        assert_eq!(r.channel, "stable");
        assert_eq!(r.target_ref, "stable@abc12345");
        assert_eq!(r.state, "Executing");
        assert_eq!(r.current_wave, 0);
        assert_eq!(r.host_states.get("ohm").map(String::as_str), Some("Healthy"));
        assert_eq!(
            r.host_states.get("krach").map(String::as_str),
            Some("ConfirmWindow"),
        );
        assert_eq!(r.last_healthy_since.len(), 1);
        assert_eq!(r.last_healthy_since["ohm"].timestamp(), now.timestamp());
    }
}
