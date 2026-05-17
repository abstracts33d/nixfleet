//! Pure planner (RFC-0009 §4.1).
//!
//! Emits `OpenRollout` actions for channels with a verified manifest but
//! no rollout row for the current target_ref yet, and walks each active
//! rollout's `Pending` hosts through the gate stack to produce
//! `QueueDispatch` (gates pass) or `DeferDispatch` (gates block) actions.
//!
//! Properties enforced by the signature:
//!
//! - **Pure.** No `chrono::Utc::now()`, no DB reads, no HTTP. `now` is
//!   a parameter so tests advance time deterministically.
//! - **Verified-only inputs.** `SignedManifestSet` carries `Verified<T>`;
//!   the planner cannot accidentally consume an unverified manifest.

use chrono::{DateTime, Duration, Utc};
use nixfleet_state_machine::HostState;

use crate::planner_gates;
use crate::planner_types::{FleetState, PlanAction, QuarantineSet, RolloutId, SignedManifestSet};

/// Pure reducer: given the trust-verified manifests, the current
/// per-host state aggregate, and the active quarantine table, produce a
/// list of `PlanAction`s for the applier to execute.
///
/// Determinism: `(manifests, fleet_state, quarantines, now)` →
/// `Vec<PlanAction>`. No hidden state, no I/O. Same inputs → same output.
pub fn plan_next(
    manifests: &SignedManifestSet,
    fleet_state: &FleetState,
    quarantines: &QuarantineSet,
    now: DateTime<Utc>,
) -> Vec<PlanAction> {
    let mut actions = Vec::new();

    // 1. Open rollouts for channels whose verified manifest advertises
    //    a target_ref that has no rollout row yet.
    //
    // Predicate is `rollouts.contains_key(&rollout_id)` — rollout-id-
    // keyed, not channel-keyed. `active_rollout_per_channel` filters by
    // `terminal_at.is_none()` (reducer.rs's build_fleet_state); using it
    // as the OpenRollout guard would cause two latent failures: (a)
    // re-firing OpenRollout against a Terminal rollout for the same
    // target_ref every reconcile tick — the applier's effect handler
    // is idempotent on the `rollouts` table but upserts
    // `host_rollout_records` back to Pending, clobbering Converged and
    // blocking the channel-edges gate (OBSERVATION-016 / D-018); and
    // (b) failing to open a successor rollout when a new target_ref
    // arrives while a predecessor is still Active — the channel-keyed
    // check stays true under the predecessor, so the successor is
    // never emitted, and supersession at applier.rs:114-180 never
    // triggers (OBSERVATION-015 steady-state).
    //
    // The rollout-id-keyed predicate splits the two intents: this site
    // asks "has this specific target_ref been opened?", channel-edges
    // asks "is anything in flight on this predecessor channel?" — the
    // latter remains keyed on `active_rollout_per_channel`.
    //
    // `rollout_id` is the canonical `"{channel}@{channel_ref}"` composite
    // (RFC-0012 §6.3 + D-007 amendment `0320c2fa`). `target_ref` stays
    // as the raw channel_ref since it identifies the channel pointer,
    // not the rollout.
    for (channel, rollout_manifest) in &manifests.rollouts {
        let channel_ref = rollout_manifest.inner().channel_ref.clone();
        let rollout_id = RolloutId::new(channel, &channel_ref);
        if !fleet_state.rollouts.contains_key(&rollout_id) {
            actions.push(PlanAction::OpenRollout {
                rollout_id,
                channel: channel.clone(),
                target_ref: channel_ref,
            });
        }
    }

    // 2. Per active rollout, walk hosts in `Pending` state and consult
    //    the gate stack. Pass → QueueDispatch. Block → DeferDispatch
    //    (telemetry). No state change either way — applier acts.
    //
    // **Within-tick budget accumulator (D-008 §2).** With `Pending`
    // removed from `is_in_flight` (D-008 §1), the gate would otherwise
    // wave through N Pending hosts on a `max_in_flight = 1` budget
    // because none have transitioned to Activating yet within the same
    // tick. `tick_dispatched` carries per-budget dispatch counts emitted
    // earlier in this loop; the gate adds them to the live in-flight
    // count before checking against `max`. Locks in Test B's regression.
    let mut tick_dispatched: std::collections::HashMap<
        planner_gates::disruption_budget::BudgetId,
        u32,
    > = std::collections::HashMap::new();
    for (rollout_id, summary) in &fleet_state.rollouts {
        if summary.terminal_at.is_some() {
            continue; // closed rollout — no more dispatches
        }
        let channel = &summary.channel;

        for ((rid, host), state) in &fleet_state.host_states {
            if rid != rollout_id {
                continue;
            }
            if state.state != HostState::Pending {
                continue;
            }
            // dispatch_acked_at != None means the agent has already
            // ack'd; that should have advanced the state past Pending
            // via the reducer. If we're still Pending, the dispatch is
            // queued and the agent hasn't pulled it yet — applier
            // skips queueing a duplicate.
            if state.dispatch_acked_at.is_some() {
                continue;
            }

            let target_closure = &state.target_closure;
            let block = planner_gates::evaluate_for_dispatch(
                fleet_state,
                manifests,
                quarantines,
                rollout_id,
                host,
                target_closure,
                channel,
                &tick_dispatched,
            );
            match block {
                Some(b) => {
                    actions.push(PlanAction::DeferDispatch {
                        host: host.clone(),
                        rollout: rollout_id.clone(),
                        gate: b.discriminator(),
                        reason: b.reason(),
                    });
                }
                None => {
                    // Increment the within-tick counter for every
                    // budget this host belongs to BEFORE pushing the
                    // QueueDispatch so the next host's gate-check sees
                    // the updated count.
                    for budget in &summary.budgets {
                        if budget.hosts.iter().any(|h| h == host) {
                            *tick_dispatched.entry(budget.selector.clone()).or_insert(0) += 1;
                        }
                    }
                    let soak_due_at = state.soak_due_at.unwrap_or(now);
                    actions.push(PlanAction::QueueDispatch {
                        host: host.clone(),
                        rollout: rollout_id.clone(),
                        target_closure: target_closure.clone(),
                        soak_due_at,
                    });
                }
            }
        }
    }

    // Terminal-transition emission moved to the rollout reducer in
    // Phase 10b (RFC-0012 §3 + §7). The planner no longer emits
    // `MarkChannelTerminal`; the rollout reducer's
    // `RolloutEffect::RecordRolloutTransition` drives the transition
    // when it consumes the last per-host `HostStateChanged → Converged`.

    actions
}

/// Compute `soak_due_at` for a freshly-dispatched host. Pure: takes
/// `dispatched_at` + the policy's wave soak duration; returns the time
/// at which the soak window elapses.
pub fn compute_soak_due_at(dispatched_at: DateTime<Utc>, soak_minutes: u32) -> DateTime<Utc> {
    dispatched_at + Duration::minutes(soak_minutes as i64)
}

/// Lookup helper: given a host_id, find its active rollout id (if any).
pub fn active_rollout_for_host<'a>(
    fleet_state: &'a FleetState,
    host_id: &str,
) -> Option<&'a RolloutId> {
    // We don't track host -> rollout directly; reverse-scan host_states.
    // The active set is small (≤fleet size); fine for v0.2 scale.
    fleet_state
        .host_states
        .iter()
        .find(|((_, h), _)| h == host_id)
        .map(|((rid, _), _)| rid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner_types::*;
    use chrono::TimeZone;
    use nixfleet_state_machine::HostRolloutState;
    use std::collections::HashMap;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 16, 1, 0, 0).unwrap()
    }

    fn empty_fleet_state() -> FleetState {
        FleetState {
            host_states: HashMap::new(),
            active_rollout_per_channel: HashMap::new(),
            rollouts: HashMap::new(),
            outstanding_failing_enforce_probes: HashMap::new(),
        }
    }

    // `rollout_summary` helper deleted in Phase 10a — only used by the
    // (now-removed) `maybe_mark_terminal_*` tests.

    fn host_in(rollout: &str, host: &str, state: HostState) -> HostRolloutState {
        let mut s = HostRolloutState::new_pending(
            rollout.into(),
            host.into(),
            "stable".into(),
            "target".into(),
            t0(),
            t0() + chrono::Duration::minutes(5),
        );
        s.state = state;
        s
    }

    // Phase 10a removed the `maybe_mark_terminal` helper + tests; the
    // rollout reducer (Phase 10b) drives terminal transitions via
    // `RolloutEffect::RecordRolloutTransition`.

    #[test]
    fn compute_soak_due_at_is_pure_addition() {
        assert_eq!(
            compute_soak_due_at(t0(), 5),
            t0() + chrono::Duration::minutes(5)
        );
        assert_eq!(compute_soak_due_at(t0(), 0), t0());
    }

    // plan_next integration tests
    //
    // These run plan_next() end-to-end against the new gate stack. The
    // SignedManifestSet is built via `Verified::unverified_for_tests`
    // (test-only constructor in verify.rs).

    use crate::planner_types::SignedManifestSet;
    use crate::verify::Verified;
    use nixfleet_proto::testing::FleetBuilder;

    fn signed_manifest_set(fleet: nixfleet_proto::FleetResolved) -> SignedManifestSet {
        SignedManifestSet {
            fleet: Verified::unverified_for_tests(fleet, t0()),
            rollouts: HashMap::new(),
        }
    }

    #[test]
    fn plan_next_queues_dispatch_for_pending_host_with_no_gates_blocking() {
        let fleet = FleetBuilder::new().host("h1", "stable").build();
        let manifests = signed_manifest_set(fleet);

        let mut fs = empty_fleet_state();
        fs.rollouts.insert(
            "r1".into(),
            RolloutSummary {
                rollout_id: "r1".into(),
                channel: "stable".into(),
                target_ref: "r1".into(),
                opened_at: t0(),
                terminal_at: None,
                current_wave: 0,
                budgets: Vec::new(),
            },
        );
        fs.active_rollout_per_channel
            .insert("stable".into(), "r1".into());
        fs.host_states.insert(
            ("r1".into(), "h1".into()),
            host_in("r1", "h1", HostState::Pending),
        );

        let quarantines = std::collections::HashMap::new();
        let actions = plan_next(&manifests, &fs, &quarantines, t0());

        assert!(actions.iter().any(|a| matches!(
            a,
            PlanAction::QueueDispatch { host, rollout, .. } if host == "h1" && rollout.as_str() == "r1"
        )));
    }

    #[test]
    fn plan_next_defers_dispatch_when_quarantined() {
        let fleet = FleetBuilder::new().host("h1", "stable").build();
        let manifests = signed_manifest_set(fleet);

        let mut fs = empty_fleet_state();
        fs.rollouts.insert(
            "r1".into(),
            RolloutSummary {
                rollout_id: "r1".into(),
                channel: "stable".into(),
                target_ref: "r1".into(),
                opened_at: t0(),
                terminal_at: None,
                current_wave: 0,
                budgets: Vec::new(),
            },
        );
        fs.active_rollout_per_channel
            .insert("stable".into(), "r1".into());
        let mut h1 = host_in("r1", "h1", HostState::Pending);
        h1.target_closure = "bad-hash".into();
        fs.host_states.insert(("r1".into(), "h1".into()), h1);

        let mut quarantines = std::collections::HashMap::new();
        let mut set = std::collections::HashSet::new();
        set.insert("bad-hash".to_string());
        quarantines.insert("stable".to_string(), set);

        let actions = plan_next(&manifests, &fs, &quarantines, t0());

        assert!(actions.iter().any(|a| matches!(
            a,
            PlanAction::DeferDispatch { host, gate, .. } if host == "h1" && *gate == "quarantine"
        )));
        // No QueueDispatch for this host.
        assert!(!actions.iter().any(|a| matches!(
            a,
            PlanAction::QueueDispatch { host, .. } if host == "h1"
        )));
    }

    #[test]
    fn plan_next_skips_acked_hosts() {
        // Host in Pending but with dispatch_acked_at set — applier has
        // already queued the Dispatch; reducer just hasn't seen the ack
        // yet. plan_next must not re-emit QueueDispatch.
        let fleet = FleetBuilder::new().host("h1", "stable").build();
        let manifests = signed_manifest_set(fleet);

        let mut fs = empty_fleet_state();
        fs.rollouts.insert(
            "r1".into(),
            RolloutSummary {
                rollout_id: "r1".into(),
                channel: "stable".into(),
                target_ref: "r1".into(),
                opened_at: t0(),
                terminal_at: None,
                current_wave: 0,
                budgets: Vec::new(),
            },
        );
        let mut h1 = host_in("r1", "h1", HostState::Pending);
        h1.dispatch_acked_at = Some(t0());
        fs.host_states.insert(("r1".into(), "h1".into()), h1);

        let quarantines = std::collections::HashMap::new();
        let actions = plan_next(&manifests, &fs, &quarantines, t0());

        assert!(!actions.iter().any(|a| matches!(
            a,
            PlanAction::QueueDispatch { host, .. } if host == "h1"
        )));
    }

    // `plan_next_emits_mark_terminal_when_rollout_converges` deleted
    // alongside `MarkChannelTerminal`. The rollout reducer's terminal
    // transition is covered by `tests/rollout_rederivability.rs` in 10b.

    #[test]
    fn plan_next_emits_open_rollout_for_unopened_channel() {
        // A channel with a verified rollout manifest but no entry in
        // active_rollout_per_channel — planner emits OpenRollout for
        // the applier to create the per-host records.
        //
        // **D-006 regression guard**: pin `rollout_id == channel_ref`
        // (NOT `channel`). The applier-side `build_fleet_state` reads
        // `manifest.channel_ref` to look up the rollout; if the planner
        // emits `channel` here, `fleet_state.host_states` will come
        // back empty for the rollout and Pending → QueueDispatch
        // iterates zero hosts. Fixture deliberately uses
        // `channel != channel_ref` to surface the mismatch.
        let fleet = FleetBuilder::new().host("h1", "stable").build();
        let mut manifests = signed_manifest_set(fleet);

        // Synthesise a verified rollout manifest.
        let rollout_manifest = nixfleet_proto::RolloutManifest {
            schema_version: 1,
            display_name: "stable@r1".into(),
            channel: "stable".into(),
            channel_ref: "r1".into(),
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
        manifests.rollouts.insert(
            "stable".to_string(),
            Verified::unverified_for_tests(rollout_manifest, t0()),
        );

        let fs = empty_fleet_state();
        let quarantines = std::collections::HashMap::new();
        let actions = plan_next(&manifests, &fs, &quarantines, t0());

        let open = actions
            .iter()
            .find_map(|a| match a {
                PlanAction::OpenRollout {
                    rollout_id,
                    channel,
                    target_ref,
                } if channel == "stable" => Some((rollout_id, target_ref)),
                _ => None,
            })
            .expect("OpenRollout for stable must be emitted");
        // D-007 regression guard: rollout_id is the canonical
        // `"{channel}@{channel_ref}"` composite, not channel_ref alone.
        // D-006's fix used channel_ref-only which collides when multiple
        // channels share a ref; D-007 lifted the identity contract.
        assert_eq!(
            open.0.as_str(),
            "stable@r1",
            "rollout_id MUST equal RolloutId::new(channel, channel_ref) per RFC-0012 §6.3 (D-007 guard)"
        );
        assert_eq!(
            open.1, "r1",
            "target_ref stays as raw channel_ref (the channel pointer)"
        );
    }

    #[test]
    fn plan_next_does_not_re_emit_open_rollout_for_terminal_rollout() {
        // D-018 regression guard. Terminal rollouts stay in the
        // `rollouts` table (per db::rollouts:263 + RFC-0012 §6.3) so the
        // channel-edges gate can read `terminal_at`. The pre-fix
        // predicate keyed OpenRollout on
        // `active_rollout_per_channel.contains_key(channel)`, which
        // EXCLUDES Terminal rollouts (reducer.rs:472 filters by
        // `terminal_at.is_none()`) — so a Terminal rollout for the same
        // target_ref fell through the guard and re-fired OpenRollout
        // every reconcile tick. Applier's effect handler is idempotent
        // on the `rollouts` table but upserts `host_rollout_records`
        // back to Pending, clobbering Converged and freezing the
        // channel-edges gate closed. The rollout-id-keyed predicate
        // closes the loop.
        //
        // Symptom in lab (2026-05-17, OBSERVATION-016): after
        // edge@f8c46e47 transitioned Active → Terminal, OpenRollout
        // re-fired at seq 174, 176, 183, 246, 253, 260, 267, 274 ...
        // every reconcile interval.
        let fleet = FleetBuilder::new().host("h1", "stable").build();
        let mut manifests = signed_manifest_set(fleet);

        let rollout_manifest = nixfleet_proto::RolloutManifest {
            schema_version: 1,
            display_name: "stable@r1".into(),
            channel: "stable".into(),
            channel_ref: "r1".into(),
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
        manifests.rollouts.insert(
            "stable".to_string(),
            Verified::unverified_for_tests(rollout_manifest, t0()),
        );

        // Terminal rollout for the SAME target_ref: present in
        // `rollouts` (channel-edges still needs to see terminal_at),
        // absent from `active_rollout_per_channel` (mirrors
        // reducer.rs's terminal_at-filtered population).
        let rollout_id = nixfleet_proto::RolloutId::new("stable", "r1");
        let mut fs = empty_fleet_state();
        fs.rollouts.insert(
            rollout_id.clone(),
            RolloutSummary {
                rollout_id: rollout_id.clone(),
                channel: "stable".into(),
                target_ref: "r1".into(),
                opened_at: t0(),
                terminal_at: Some(t0() + chrono::Duration::minutes(10)),
                current_wave: 0,
                budgets: Vec::new(),
            },
        );

        let quarantines = std::collections::HashMap::new();
        let actions = plan_next(&manifests, &fs, &quarantines, t0());

        assert!(
            !actions.iter().any(|a| matches!(
                a,
                PlanAction::OpenRollout { rollout_id: rid, .. }
                    if rid.as_str() == "stable@r1"
            )),
            "Terminal rollout for same target_ref MUST NOT re-fire OpenRollout; actions: {actions:?}",
        );
    }

    #[test]
    fn plan_next_emits_open_rollout_for_new_target_ref_while_predecessor_active() {
        // D-018 bonus closure for the steady-state half of
        // OBSERVATION-015 (supersession on new release while
        // predecessor in-flight). Under the pre-fix predicate
        // (`active_rollout_per_channel.contains_key(channel)`), a new
        // target_ref arriving while the predecessor was still Active
        // never triggered OpenRollout — the channel-keyed guard stayed
        // true. The applier's supersession path at
        // applier.rs:114-180 was thus unreachable in steady state.
        // The rollout-id-keyed predicate makes "new target_ref means
        // new rollout_id" the natural trigger.
        let fleet = FleetBuilder::new().host("h1", "stable").build();
        let mut manifests = signed_manifest_set(fleet);

        // Manifest now advertises the NEW target_ref `r2`.
        let rollout_manifest = nixfleet_proto::RolloutManifest {
            schema_version: 1,
            display_name: "stable@r2".into(),
            channel: "stable".into(),
            channel_ref: "r2".into(),
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
        manifests.rollouts.insert(
            "stable".to_string(),
            Verified::unverified_for_tests(rollout_manifest, t0()),
        );

        // Predecessor rollout `stable@r1` is Active.
        let pred_id = nixfleet_proto::RolloutId::new("stable", "r1");
        let mut fs = empty_fleet_state();
        fs.rollouts.insert(
            pred_id.clone(),
            RolloutSummary {
                rollout_id: pred_id.clone(),
                channel: "stable".into(),
                target_ref: "r1".into(),
                opened_at: t0(),
                terminal_at: None,
                current_wave: 0,
                budgets: Vec::new(),
            },
        );
        fs.active_rollout_per_channel
            .insert("stable".into(), pred_id);

        let quarantines = std::collections::HashMap::new();
        let actions = plan_next(&manifests, &fs, &quarantines, t0());

        assert!(
            actions.iter().any(|a| matches!(
                a,
                PlanAction::OpenRollout { rollout_id: rid, target_ref, .. }
                    if rid.as_str() == "stable@r2" && target_ref == "r2"
            )),
            "New target_ref MUST trigger OpenRollout for the new rollout_id even while predecessor is Active; actions: {actions:?}",
        );
    }
}
