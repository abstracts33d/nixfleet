//! Live `Observed` projection from in-memory checkin state.
//! `--observed` (file-backed) stays as a dev/test + offline-replay
//! fallback. Pass `&[]` for `rollouts` when no DB is configured.

use std::collections::HashMap;

use nixfleet_reconciler::observed::{HostState, Observed, Rollout};
use nixfleet_reconciler::{HostRolloutState, RolloutState};

use crate::db::RolloutDbSnapshot;
use crate::server::HostCheckinRecord;

/// Pure: caller holds the read locks and runs the DB query.
pub fn project(
    host_checkins: &HashMap<String, HostCheckinRecord>,
    channel_refs: &HashMap<String, String>,
    rollouts: &[RolloutDbSnapshot],
    compliance_failures_by_rollout: HashMap<String, HashMap<String, usize>>,
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

    // No dedicated rollouts table yet; every snapshotted rollout
    // surfaces as Executing. Multi-wave tracking is a Phase 4
    // follow-up.
    let active_rollouts: Vec<Rollout> = rollouts
        .iter()
        .map(|snap| Rollout {
            id: snap.rollout_id.clone(),
            channel: snap.channel.clone(),
            target_ref: snap.target_channel_ref.clone(),
            state: RolloutState::Executing,
            current_wave: 0,
            // Unknown SQL strings fall back to `Failed` (not
            // `Queued`!) — `Queued` would re-dispatch the host
            // every tick, defeating resolution-by-replacement.
            // The variant set is canonical for V003 CHECK; drift
            // is caught by `host_rollout_state_check_matches_enum`.
            host_states: snap
                .host_states
                .iter()
                .map(|(h, s)| {
                    let parsed = HostRolloutState::from_db_str(s).unwrap_or_else(|_| {
                        tracing::warn!(
                            rollout = %snap.rollout_id,
                            host = %h,
                            unknown_state = %s,
                            "host_rollout_state value not in HostRolloutState enum — \
                             halting rollout (Failed fallback). Likely a SQL CHECK \
                             extension that wasn't propagated to the typed enum.",
                        );
                        HostRolloutState::Failed
                    });
                    (h.clone(), parsed)
                })
                .collect(),
            last_healthy_since: snap.last_healthy_since.clone(),
        })
        .collect();

    Observed {
        channel_refs: channel_refs.clone(),
        // Dispatch loop populates last_rolled_refs; empty case fine.
        last_rolled_refs: HashMap::new(),
        host_state,
        active_rollouts,
        compliance_failures_by_rollout,
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
        let observed = project(&checkins, &channel_refs, &[], HashMap::new());

        assert_eq!(observed.host_state.len(), 2);
        assert_eq!(
            observed.host_state["test-host"]
                .current_generation
                .as_deref(),
            Some("abc")
        );
        assert!(observed.host_state["test-host"].online);
        assert_eq!(observed.channel_refs["dev"], "deadbeef");
    }

    #[test]
    fn projection_with_no_checkins_yields_empty_host_state() {
        let observed = project(&HashMap::new(), &HashMap::new(), &[], HashMap::new());
        assert!(observed.host_state.is_empty());
        assert!(observed.channel_refs.is_empty());
        assert!(observed.active_rollouts.is_empty());
    }

    #[test]
    fn host_rollout_state_check_matches_enum() {
        // Drift detector: every value in the V003 CHECK list must
        // be parseable by `HostRolloutState::from_db_str`. If the SQL
        // gets a new value without the enum being extended (or
        // vice versa), the projection's `Failed` fallback fires
        // for live rows and silently halts rollouts. Catch it at
        // test time instead.
        let migration = include_str!("../migrations/V003__host_rollout_state.sql");
        // Extract the parenthesised list after `host_state IN (`.
        let needle = "host_state IN (";
        let start = migration.find(needle).expect("CHECK clause present");
        let after = &migration[start + needle.len()..];
        let end = after.find(')').expect("CHECK clause closes");
        let list = &after[..end];
        let values: Vec<&str> = list
            .split(',')
            .map(|s| s.trim().trim_matches('\'').trim())
            .filter(|s| !s.is_empty())
            .collect();
        assert!(!values.is_empty(), "expected ≥1 value in CHECK clause");
        for v in &values {
            HostRolloutState::from_db_str(v).unwrap_or_else(|_| {
                panic!(
                    "V003 CHECK list value {v:?} is not in HostRolloutState. \
                     Either extend the enum or remove the value from the CHECK."
                )
            });
        }
    }

    #[test]
    fn projection_falls_back_to_failed_on_unknown_host_state() {
        // The projection's defense-in-depth: an unrecognised SQL
        // value must surface as Failed (halt the rollout) rather
        // than Queued (re-dispatch loop). The current schema
        // doesn't permit this in steady state, but a future CHECK
        // extension that lands before the enum update would
        // otherwise re-dispatch every "Reverted" host on every
        // tick.
        let mut host_states = HashMap::new();
        host_states.insert("ohm".to_string(), "TotallyBogus".to_string());
        let snap = RolloutDbSnapshot {
            rollout_id: "stable@deadbeef".to_string(),
            channel: "stable".to_string(),
            target_closure_hash: "system-r1".to_string(),
            target_channel_ref: "stable@deadbeef".to_string(),
            host_states,
            last_healthy_since: HashMap::new(),
        };
        let observed = project(
            &HashMap::new(),
            &HashMap::new(),
            std::slice::from_ref(&snap),
            HashMap::new(),
        );
        assert_eq!(
            observed.active_rollouts[0].host_states.get("ohm").copied(),
            Some(HostRolloutState::Failed),
        );
    }

    #[test]
    fn projection_round_trips_reverted_state() {
        // V003 reserves Reverted; the typed enum carries it.
        // Confirm the projection round-trips the wire string into
        // the typed variant rather than misclassifying it.
        let mut host_states = HashMap::new();
        host_states.insert("ohm".to_string(), "Reverted".to_string());
        let snap = RolloutDbSnapshot {
            rollout_id: "stable@deadbeef".to_string(),
            channel: "stable".to_string(),
            target_closure_hash: "system-r1".to_string(),
            target_channel_ref: "stable@deadbeef".to_string(),
            host_states,
            last_healthy_since: HashMap::new(),
        };
        let observed = project(
            &HashMap::new(),
            &HashMap::new(),
            std::slice::from_ref(&snap),
            HashMap::new(),
        );
        assert_eq!(
            observed.active_rollouts[0].host_states.get("ohm").copied(),
            Some(HostRolloutState::Reverted),
        );
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
        let observed = project(
            &HashMap::new(),
            &HashMap::new(),
            std::slice::from_ref(&snap),
            HashMap::new(),
        );
        assert_eq!(observed.active_rollouts.len(), 1);
        let r = &observed.active_rollouts[0];
        assert_eq!(r.id, "stable@abc12345");
        assert_eq!(r.channel, "stable");
        assert_eq!(r.target_ref, "stable@abc12345");
        assert_eq!(r.state, RolloutState::Executing);
        assert_eq!(r.current_wave, 0);
        assert_eq!(
            r.host_states.get("ohm").copied(),
            Some(HostRolloutState::Healthy),
        );
        assert_eq!(
            r.host_states.get("krach").copied(),
            Some(HostRolloutState::ConfirmWindow),
        );
        assert_eq!(r.last_healthy_since.len(), 1);
        assert_eq!(r.last_healthy_since["ohm"].timestamp(), now.timestamp());
    }
}
