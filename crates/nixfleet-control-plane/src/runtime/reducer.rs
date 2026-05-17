//! Reducer task body: the single mutator of CP-mirror state.
//!
//! Owns the cached `SignedManifestSet`, the per-channel quarantine set,
//! and (transiently) the per-rollout `HostRolloutState` map it loads from
//! the DB. Calls into `nixfleet_state_machine::step` on every `HostEvent`
//! input, `nixfleet_reconciler::plan_next` on `ManifestSetUpdated` /
//! `PlanTick`, and the applier (`apply_plan_action`/`apply_effect`) for
//! every output.
//!
//! Invariant 1 from `runtime::mod`: this is the only place that calls
//! `step()` or `plan_next()`. Workers and HTTP route handlers emit
//! `ReducerInput` values; the applier executes the produced effects.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use nixfleet_proto::RolloutPolicy;
use nixfleet_proto::clock::ClockHandle;
use nixfleet_reconciler::planner_types::{
    FleetState, HostId, QuarantineSet, RolloutId, RolloutSummary, SignedManifestSet,
};
use nixfleet_state_machine::HostRolloutState;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::applier::{ApplierCtx, apply_effect, apply_plan_action};
use super::event_log_writer::EventLogTx;
use super::{HeartbeatReply, ReducerInput};
use crate::db::Db;
use crate::server::AppState;

/// Safety-net replan cadence. Triggers `plan_next` even when no event
/// arrives — covers manifest_poll crashes mid-cycle, missed kicks, etc.
const PLAN_TICK_INTERVAL: Duration = Duration::from_secs(15);

pub(super) async fn run(
    cancel: CancellationToken,
    state: Arc<AppState>,
    clock: ClockHandle,
    mut input_rx: mpsc::Receiver<ReducerInput>,
    event_log_tx: EventLogTx,
    shutdown_senders: Vec<oneshot::Sender<()>>,
) {
    let _shutdown_guard = ShutdownGuard(shutdown_senders);

    let mut reducer_state = ReducerState {
        manifests: None,
        quarantines: QuarantineSet::new(),
        last_heartbeat_at: HashMap::new(),
    };

    let mut plan_ticker = tokio::time::interval(PLAN_TICK_INTERVAL);
    plan_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!(target: "shutdown", task = "cp_reducer", "task shut down");
                return;
            }
            maybe_input = input_rx.recv() => {
                let Some(input) = maybe_input else { return };
                handle_input(&state, &clock, &event_log_tx, &mut reducer_state, input).await;
            }
            _ = plan_ticker.tick() => {
                run_plan(&state, &clock, &event_log_tx, &reducer_state).await;
            }
        }
    }
}

/// Reducer-task-private state. The DB-backed CP-mirror lives in
/// `host_rollout_records` (Phase 4); this struct holds only the data the
/// reducer can't or shouldn't go back to SQLite for on every input.
struct ReducerState {
    manifests: Option<SignedManifestSet>,
    quarantines: QuarantineSet,
    /// `last_heartbeat_at[hostname]` — in-memory only. Lost on CP restart;
    /// the agent's next heartbeat re-seeds within seconds. Operator-
    /// observable freshness, not gate input.
    last_heartbeat_at: HashMap<String, DateTime<Utc>>,
}

async fn handle_input(
    state: &Arc<AppState>,
    clock: &ClockHandle,
    event_log_tx: &EventLogTx,
    rs: &mut ReducerState,
    input: ReducerInput,
) {
    match input {
        ReducerInput::HostEvent {
            host,
            rollout_id,
            event,
        } => {
            handle_host_event(state, clock, event_log_tx, rs, &host, &rollout_id, event).await;
        }
        ReducerInput::ManifestSetUpdated(set) => {
            rs.manifests = Some(*set);
            run_plan(state, clock, event_log_tx, rs).await;
        }
        ReducerInput::HeartbeatReceived {
            host,
            rollout_id,
            current_closure,
            at,
            reply,
        } => {
            handle_heartbeat(
                state,
                clock,
                event_log_tx,
                rs,
                &host,
                rollout_id,
                current_closure,
                at,
                reply,
            )
            .await;
        }
        ReducerInput::PlanTick => {
            run_plan(state, clock, event_log_tx, rs).await;
        }
    }
}

async fn handle_host_event(
    state: &Arc<AppState>,
    clock: &ClockHandle,
    event_log_tx: &EventLogTx,
    rs: &mut ReducerState,
    host: &str,
    rollout_id: &nixfleet_proto::RolloutId,
    event: nixfleet_state_machine::Event,
) {
    let Some(db) = state.db.as_ref() else {
        tracing::warn!(
            target: "cp_reducer",
            host, rollout_id = %rollout_id,
            "HostEvent: no DB attached; skipping",
        );
        return;
    };

    // Load current state. Absence is legal — first event from an agent on
    // a fresh rollout. The state machine's transitions starting from
    // Pending require an existing record though; if absent, we drop +
    // log so the operator sees a "saw event for unknown host" alert.
    let prior = match db.host_rollout_records().load(rollout_id.as_str(), host) {
        Ok(Some(s)) => s,
        Ok(None) => {
            tracing::warn!(
                target: "cp_reducer",
                host, rollout_id = %rollout_id,
                "HostEvent: no host_rollout_records row; dropping (planner-side OpenRollout creates Pending records — was this an out-of-order arrival?)",
            );
            return;
        }
        Err(err) => {
            tracing::error!(
                target: "cp_reducer",
                host, rollout_id = %rollout_id,
                error = %err,
                "HostEvent: load failed",
            );
            return;
        }
    };

    // Dedup: drop seq <= last_event_seq. The events route does the same
    // check, but a race between two simultaneous POSTs can have both pass
    // the route check before either updates the DB.
    let incoming_seq = event.seq();
    if incoming_seq <= prior.last_event_seq {
        tracing::debug!(
            target: "cp_reducer",
            host, rollout_id = %rollout_id,
            incoming_seq,
            last_event_seq = prior.last_event_seq,
            "HostEvent: duplicate seq; dropping",
        );
        return;
    }

    let Some(manifests) = rs.manifests.as_ref() else {
        tracing::warn!(
            target: "cp_reducer",
            host, rollout_id = %rollout_id,
            "HostEvent: no cached SignedManifestSet; cannot resolve RolloutPolicy. Dropping (manifest_poll will warm the cache and a retry will succeed).",
        );
        return;
    };
    let policy = match resolve_policy(manifests, &prior.channel) {
        Some(p) => p.clone(),
        None => {
            tracing::warn!(
                target: "cp_reducer",
                host, rollout_id = %rollout_id,
                channel = %prior.channel,
                "HostEvent: rollout_policy not found in cached manifests; dropping",
            );
            return;
        }
    };

    let now = clock.now();
    let prior_host_state = prior.state;
    let (mut next_state, effects) = match nixfleet_state_machine::step(prior, event, now, &policy) {
        Ok(out) => out,
        Err(err) => {
            tracing::warn!(
                target: "cp_reducer",
                host, rollout_id = %rollout_id,
                error = %err,
                "HostEvent: step() rejected — illegal transition or out-of-order event",
            );
            return;
        }
    };
    next_state.last_event_seq = incoming_seq;
    let next_host_state = next_state.state;

    if let Err(err) = db.host_rollout_records().upsert(&next_state) {
        tracing::error!(
            target: "cp_reducer",
            host, rollout_id = %rollout_id,
            error = %err,
            "HostEvent: host_rollout_records upsert failed",
        );
        return;
    }

    let ctx = ApplierCtx {
        state,
        manifests,
        clock,
        event_log_tx,
    };
    for effect in effects {
        apply_effect(&ctx, effect).await;
    }

    // RFC-0012 §7 reducer composition: per-host transitions feed the
    // rollout reducer as `HostStateChanged`; aggregate signals (e.g.,
    // "all hosts in this rollout reached Converged") are computed
    // applier-side from `host_rollout_records` and emitted as
    // `RolloutTerminal`.
    if prior_host_state != next_host_state {
        super::applier::process_rollout_event(
            &ctx,
            db,
            now,
            nixfleet_state_machine::rollout::RolloutEvent::HostStateChanged {
                rollout_id: rollout_id.clone(),
                host_id: host.to_string(),
                from: prior_host_state,
                to: next_host_state,
                at: now,
            },
        )
        .await;

        // Terminal aggregate: if every host in this rollout has reached
        // Converged, emit `RolloutTerminal` so the rollout reducer
        // transitions to Terminal (RFC-0012 §3 invariant: "Terminal ⇒
        // ∀ host ∈ rollout: state == Converged").
        if next_host_state == nixfleet_state_machine::HostState::Converged
            && let Ok(rows) = db
                .host_rollout_records()
                .all_for_rollout(rollout_id.as_str())
            && !rows.is_empty()
            && rows
                .iter()
                .all(|r| r.state == nixfleet_state_machine::HostState::Converged)
        {
            super::applier::process_rollout_event(
                &ctx,
                db,
                now,
                nixfleet_state_machine::rollout::RolloutEvent::RolloutTerminal {
                    rollout_id: rollout_id.clone(),
                    at: now,
                },
            )
            .await;
        }
    }
}

// Threads the same reducer-task dependencies (state, clock,
// event_log_tx, rs) as handle_host_event, plus the heartbeat envelope
// (host, rollout_id, current_closure, at, reply). Refactoring to a
// context struct would obscure the call site at handle_input where the
// reducer dispatches inputs. The lint is fine to suppress here.
#[allow(clippy::too_many_arguments)]
async fn handle_heartbeat(
    state: &Arc<AppState>,
    clock: &ClockHandle,
    event_log_tx: &EventLogTx,
    rs: &mut ReducerState,
    host: &str,
    rollout_id: Option<nixfleet_proto::RolloutId>,
    current_closure: Option<String>,
    at: DateTime<Utc>,
    reply: oneshot::Sender<HeartbeatReply>,
) {
    rs.last_heartbeat_at.insert(host.to_string(), at);

    let replay_from = compute_replay_from(
        state,
        host,
        rollout_id.as_ref().map(|r| r.as_str()),
        current_closure.as_deref(),
    );

    // Reply BEFORE the synthesis call: the heartbeat HTTP handler is
    // waiting on this oneshot and the agent expects the response within
    // the route's REDUCER_REPLY_TIMEOUT. Synthesis runs after; it has
    // its own DB + event_log work that shouldn't block the agent.
    let _ = reply.send(HeartbeatReply { replay_from });

    // Boot-recovery retroactive confirmation (RFC-0008 §9.5).
    // Closes the "agent restart mid-Activating leaves CP forever stuck
    // at Activating" defect. The flow: an agent's
    // `nixfleet-agent.service` restart kills the in-flight verify_poll
    // before it can emit LocalActivationCompleted. The new agent's
    // boot-recovery handshake reports `current_closure` (read from
    // /run/current-system) but no rollout_id, so the steady-state
    // replay_from path above can't match. Here we scan active
    // host_rollout_records for this hostname; if any record's
    // target_closure matches the agent's current_closure AND state is
    // Activating, we synthesize `Event::RemoteActivationCompleted` and
    // feed it through `handle_host_event` — same path the wire-borne
    // version takes. CP transitions Activating → Soaking, populates
    // activation_completed_at, the planner unblocks, the cascade
    // continues. Recovery.rs:45-51 documented this design intent ("CP
    // synthesises an ActivationCompleted-shaped Replay-From event"); the
    // wiring was deferred to a follow-up that never landed pre-v0.2.
    if let Some(agent_current) = current_closure.as_deref() {
        maybe_synthesize_recovery_completion(
            state,
            clock,
            event_log_tx,
            rs,
            host,
            agent_current,
            at,
        )
        .await;
    }
}

/// Scan active host_rollout_records for `host`; for each record in
/// `Activating` state whose `target_closure` matches the agent's
/// reported `current_closure`, synthesize a `RemoteActivationCompleted`
/// event and feed it through `handle_host_event`. Idempotent: if the
/// state has already advanced past Activating (e.g. a concurrent agent
/// emit beat us to it), the record won't match and the synthesis is a
/// no-op. The synthesized event uses `last_event_seq + 1` so it lands
/// at the head of the per-host event stream; subsequent agent events
/// for the same rollout will use higher seqs on the agent's durable
/// queue and won't conflict.
async fn maybe_synthesize_recovery_completion(
    state: &Arc<AppState>,
    clock: &ClockHandle,
    event_log_tx: &EventLogTx,
    rs: &mut ReducerState,
    host: &str,
    agent_current: &str,
    at: DateTime<Utc>,
) {
    let Some(db) = state.db.as_ref() else {
        return;
    };
    let records = match db.host_rollout_records().active_for_host(host) {
        Ok(r) => r,
        Err(err) => {
            tracing::warn!(
                target: "cp_reducer",
                host,
                error = %err,
                "boot-recovery synthesis: active_for_host load failed; skipping",
            );
            return;
        }
    };
    for record in records {
        if record.state != nixfleet_state_machine::HostState::Activating {
            continue;
        }
        if record.target_closure != agent_current {
            continue;
        }
        tracing::info!(
            target: "cp_reducer",
            host,
            rollout_id = %record.rollout_id,
            target = %record.target_closure,
            "boot-recovery: synthesizing RemoteActivationCompleted (agent reports current_closure == target while CP records Activating; retroactive confirmation per RFC-0008 §9.5 scenario 3)",
        );
        let synth_event = nixfleet_state_machine::Event::RemoteActivationCompleted {
            observed_current_closure: agent_current.to_string(),
            exit_code: 0,
            completed_at: at,
            seq: record.last_event_seq + 1,
        };
        let rollout_id = record.rollout_id.clone();
        handle_host_event(
            state,
            clock,
            event_log_tx,
            rs,
            host,
            &rollout_id,
            synth_event,
        )
        .await;
    }
}

fn compute_replay_from(
    state: &Arc<AppState>,
    host: &str,
    rollout_id: Option<&str>,
    current_closure: Option<&str>,
) -> Option<u64> {
    let db = state.db.as_ref()?;
    let rollout_id = rollout_id?;
    let agent_closure = current_closure?;
    let record = db
        .host_rollout_records()
        .load(rollout_id, host)
        .ok()
        .flatten()?;

    let cp_closure = record.current_closure.as_deref();
    match cp_closure {
        Some(cp) if cp == agent_closure => None,
        // Drift: agent reports a closure CP didn't see acknowledged.
        // Replay-From = CP's last_event_seq + 1 (the next seq CP hasn't
        // seen yet); agent re-POSTs everything from there.
        _ => Some(record.last_event_seq),
    }
}

async fn run_plan(
    state: &Arc<AppState>,
    clock: &ClockHandle,
    event_log_tx: &EventLogTx,
    rs: &ReducerState,
) {
    let Some(manifests) = rs.manifests.as_ref() else {
        // Cold start: the manifest_poll worker hasn't primed yet.
        // Periodic ticks land here harmlessly until it does.
        return;
    };
    let Some(db) = state.db.as_ref() else {
        return;
    };

    let now = clock.now();
    let mut fleet_state = match build_fleet_state(db, manifests) {
        Ok(fs) => fs,
        Err(err) => {
            tracing::error!(
                target: "cp_reducer",
                error = %err,
                "run_plan: FleetState construction failed; skipping plan tick",
            );
            return;
        }
    };

    // Advance `rollouts.current_wave` for any rollout whose current
    // wave has fully Converged. Must happen BEFORE plan_next so the
    // wave_promotion gate sees the new value on the same tick (next
    // wave's hosts go through immediately rather than waiting on the
    // periodic safety-net replan).
    advance_current_waves(db, manifests, &mut fleet_state).await;

    let actions =
        nixfleet_reconciler::planner::plan_next(manifests, &fleet_state, &rs.quarantines, now);

    let ctx = ApplierCtx {
        state,
        manifests,
        clock,
        event_log_tx,
    };
    for action in actions {
        apply_plan_action(&ctx, action).await;
    }
}

/// Promote each rollout's `current_wave` when every host in the current
/// wave has reached `HostState::Converged`. The check reads the host
/// list from the verified manifest's `fleet.waves[channel][current_wave]`
/// — the same path the wave_promotion gate uses for `host_wave`, so the
/// bump and the gate stay in lock-step.
///
/// Empty waves are NOT auto-promoted. Same invariant as
/// `planner::maybe_mark_terminal`: an empty wave that produced no host
/// records vacuously satisfies "all converged"; treating that as
/// promotion-eligible would walk the wave pointer past content that
/// later arrives. Pin via the `has_any_host` guard.
async fn advance_current_waves(
    db: &Arc<Db>,
    manifests: &SignedManifestSet,
    fleet_state: &mut FleetState,
) {
    let fleet = manifests.fleet();
    let mut bumps: Vec<(RolloutId, u32)> = Vec::new();
    for (rollout_id, summary) in fleet_state.rollouts.iter() {
        if summary.terminal_at.is_some() {
            continue;
        }
        let Some(channel_waves) = fleet.waves.get(&summary.channel) else {
            continue;
        };
        let current = summary.current_wave as usize;
        if current + 1 >= channel_waves.len() {
            // No next wave to promote into.
            continue;
        }
        let Some(current_wave) = channel_waves.get(current) else {
            continue;
        };
        if current_wave.hosts.is_empty() {
            continue;
        }
        let all_converged = current_wave.hosts.iter().all(|host| {
            fleet_state
                .host_states
                .get(&(rollout_id.clone(), host.clone()))
                .map(|s| s.state == nixfleet_state_machine::HostState::Converged)
                .unwrap_or(false)
        });
        if all_converged {
            bumps.push((rollout_id.clone(), summary.current_wave + 1));
        }
    }

    for (rollout_id, next_wave) in bumps {
        // FK populated by Phase 10b's rollout reducer (RolloutEffect::
        // UpdateCurrentWave); 10a passes None per RFC-0012 §6.1 item 3.
        match db
            .rollouts()
            .set_current_wave(rollout_id.as_str(), next_wave, None)
        {
            Ok(_) => {
                if let Some(s) = fleet_state.rollouts.get_mut(&rollout_id) {
                    s.current_wave = next_wave;
                }
                tracing::info!(
                    target: "cp_reducer",
                    %rollout_id,
                    next_wave,
                    "advance_current_waves: bumped current_wave (every host in prior wave Converged)",
                );
            }
            Err(err) => {
                tracing::error!(
                    target: "cp_reducer",
                    %rollout_id,
                    next_wave,
                    error = %err,
                    "advance_current_waves: set_current_wave failed",
                );
            }
        }
    }
}

/// Build a fresh `FleetState` from the DB. Called on every plan tick;
/// at v0.2 scale (≤256 hosts, ≤8 active rollouts) the SELECTs are
/// negligible.
fn build_fleet_state(db: &Arc<Db>, manifests: &SignedManifestSet) -> anyhow::Result<FleetState> {
    let mut host_states: HashMap<(RolloutId, HostId), HostRolloutState> = HashMap::new();
    let mut active_rollout_per_channel: HashMap<String, RolloutId> = HashMap::new();
    let mut rollouts: HashMap<RolloutId, RolloutSummary> = HashMap::new();

    // active_rollout_per_channel must reflect ACTUAL state (the `rollouts`
    // table), not the manifest's view. The planner reads this to decide
    // whether to emit `OpenRollout`: if we pre-populated this from
    // `manifests.rollouts`, the guard `!contains_key(channel)` would
    // always be false and OpenRollout would never fire — the gate stays
    // architecturally-correct-but-never-called. (Bug caught by the 7b
    // smoke test before deletion of the legacy reconcile path.)
    let active_rollouts = db.rollouts().list_active()?;
    for ar in active_rollouts.iter() {
        if ar.terminal_at.is_none() {
            active_rollout_per_channel.insert(ar.channel.clone(), ar.rollout_id.clone());
        }
    }

    // For each channel with a verified rollout manifest, load all
    // host_rollout_records under the manifested rollout_id so the
    // planner can walk Pending hosts and run gates. Channels whose
    // rollout hasn't been opened yet (no DB row, no Pending records)
    // surface through the OpenRollout emission path above.
    for (channel, vm) in &manifests.rollouts {
        let manifest = vm.inner();
        // Canonical RolloutId construction (RFC-0012 §6.3 + D-007
        // amendment `0320c2fa`). Matches the planner's
        // `RolloutId::new(channel, channel_ref)` so lookups by
        // rollout_id succeed even when multiple channels share a
        // channel_ref.
        let rollout_id = nixfleet_proto::RolloutId::new(channel, &manifest.channel_ref);

        let rows = db
            .host_rollout_records()
            .all_for_rollout(rollout_id.as_str())?;
        for row in rows {
            host_states.insert((rollout_id.clone(), row.hostname.clone()), row);
        }

        // RolloutSummary metadata. Full row from the `rollouts` table
        // (RFC-0012 §6.3). Missing row ⇒ rollout not opened yet ⇒ omit
        // from `rollouts` map (gates that require RolloutSummary for
        // in-flight reasoning correctly see "not yet open" for this
        // channel).
        if let Ok(Some(row)) = db.rollouts().state(rollout_id.as_str()) {
            rollouts.insert(
                rollout_id.clone(),
                RolloutSummary {
                    rollout_id: rollout_id.clone(),
                    channel: channel.clone(),
                    target_ref: manifest.channel_ref.clone(),
                    opened_at: row.opened_at,
                    terminal_at: row.terminal_at,
                    current_wave: row.current_wave,
                    budgets: manifest.disruption_budgets.clone(),
                },
            );
        }
    }

    // Distinct outstanding enforce-mode probe failures per (rollout, host).
    // Feeds the compliance_wave gate (planner_gates::compliance_wave) per
    // RFC-0010 §7.2. **Phase 9a**: the source projection
    // (`probe_failures`) is unwritten — 9b's applier co-write turns it
    // on. Until then the map is always empty and the gate is
    // pass-through.
    let outstanding_failing_enforce_probes = db
        .probe_failures()
        .outstanding_failing_enforce_probes_by_rollout()
        .unwrap_or_else(|err| {
            tracing::warn!(
                target: "cp_reducer",
                error = %err,
                "build_fleet_state: outstanding_failing_enforce_probes query failed; \
                 falling back to empty map (compliance_wave gate inert this tick)",
            );
            HashMap::new()
        });

    Ok(FleetState {
        host_states,
        active_rollout_per_channel,
        rollouts,
        outstanding_failing_enforce_probes,
    })
}

fn resolve_policy<'a>(
    manifests: &'a SignedManifestSet,
    channel: &str,
) -> Option<&'a RolloutPolicy> {
    let fleet = manifests.fleet();
    let channel_entry = fleet.channels.get(channel)?;
    fleet.rollout_policies.get(&channel_entry.rollout_policy)
}

/// RAII container for per-worker shutdown senders. Holding it in scope
/// ensures workers receive shutdown signal exactly when this task exits.
struct ShutdownGuard(#[allow(dead_code)] Vec<oneshot::Sender<()>>);

#[cfg(test)]
mod tests {
    //! Regression coverage for the FleetState builder.
    //!
    //! Per-gate behaviour on populated fields is covered in
    //! `nixfleet_reconciler::planner_gates::tests`. The
    //! `outstanding_failing_enforce_probes` projection gets its own
    //! end-to-end test in 9b once the writer side lands.

    use super::*;
    use crate::db::Db;
    use chrono::Utc;
    use nixfleet_proto::testing::FleetBuilder;
    use nixfleet_reconciler::verify::Verified;

    fn fresh_db() -> Arc<Db> {
        let db = Db::open_in_memory().expect("open in-memory db");
        db.migrate().expect("apply migrations");
        Arc::new(db)
    }

    fn empty_manifests() -> SignedManifestSet {
        let fleet = FleetBuilder::new().build();
        SignedManifestSet {
            fleet: Verified::unverified_for_tests(fleet, Utc::now()),
            rollouts: HashMap::new(),
        }
    }

    #[test]
    fn outstanding_failing_enforce_probes_empty_in_9a() {
        let db = fresh_db();
        let manifests = empty_manifests();
        let fs = build_fleet_state(&db, &manifests).expect("build_fleet_state");
        assert!(
            fs.outstanding_failing_enforce_probes.is_empty(),
            "9a: probe_failures is unwritten ⇒ projection must be empty (got {:?})",
            fs.outstanding_failing_enforce_probes,
        );
    }
}
