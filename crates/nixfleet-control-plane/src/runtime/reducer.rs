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

    // RFC-0008 §7 reducer composition: per-host transitions feed the
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
        // transitions to Terminal (RFC-0008 §3 invariant: "Terminal ⇒
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

    // Boot-recovery retroactive confirmation (RFC-0005 §9.5).
    // Closes the "agent restart mid-Activating leaves CP forever stuck
    // at Activating" defect. The flow: an agent's
    // `nixfleet-agent.service` restart kills the in-flight verify_poll
    // before it can emit LocalActivationCompleted. The new agent's
    // boot-recovery handshake reports `current_closure` (read from
    // /run/current-system) but no rollout_id, so the steady-state
    // replay_from path can't match. Here we scan active
    // host_rollout_records for this hostname; if any record's
    // target_closure matches the agent's current_closure AND state is
    // Activating or Deferred, we synthesize
    // `Event::RemoteActivationCompleted` and feed it through
    // `handle_host_event` — same path the wire-borne version takes. CP
    // transitions Activating/Deferred → Soaking, populates
    // activation_completed_at, the planner unblocks, the cascade
    // continues. Recovery.rs:45-51 documented this design intent ("CP
    // synthesises an ActivationCompleted-shaped Replay-From event").
    //
    // Synthesis runs BEFORE bootstrap + reply (LIFT #3 ordering): the
    // bootstrap reflects post-synthesis state (e.g. Soaking, not
    // Activating). The reducer is single-threaded so the read-modify-
    // read is race-free; synthesis is in-process and well under the
    // route's REDUCER_REPLY_TIMEOUT.
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

    let replay_from = compute_replay_from(
        state,
        host,
        rollout_id.as_ref().map(|r| r.as_str()),
        current_closure.as_deref(),
    );

    // LIFT #3: when the agent's heartbeat carried no rollout_id (the
    // boot-recovery shape — agent's reducer is empty post-restart), but
    // CP holds non-terminal records for the host, build a bootstrap
    // snapshot per record. The agent's runtime applies each snapshot to
    // its in-memory HostRolloutState before workers spawn, restoring
    // the cache so probe runners + advance-ticker resume work
    // post-restart. Steady-state heartbeats (rollout_id populated)
    // skip this — the agent's reducer already knows.
    let bootstrap_rollouts = if rollout_id.is_none() {
        build_bootstrap_for_host(state, host)
    } else {
        Vec::new()
    };

    let _ = reply.send(HeartbeatReply {
        replay_from,
        bootstrap_rollouts,
    });
}

/// LIFT #3: scan active records for `host` and produce a
/// `HostRolloutSnapshot` per record. Called only on boot-recovery-shaped
/// heartbeats (rollout_id=None). Order is deterministic by
/// (rollout_id, hostname) PK in SQL; the agent applies them
/// in arrival order.
fn build_bootstrap_for_host(
    state: &Arc<AppState>,
    host: &str,
) -> Vec<nixfleet_proto::agent_wire::HostRolloutSnapshot> {
    let Some(db) = state.db.as_ref() else {
        return Vec::new();
    };
    let records = match db.host_rollout_records().active_for_host(host) {
        Ok(r) => r,
        Err(err) => {
            tracing::warn!(
                target: "cp_reducer",
                host,
                error = %err,
                "bootstrap build: active_for_host load failed; returning empty",
            );
            return Vec::new();
        }
    };
    records
        .into_iter()
        .map(host_rollout_state_to_snapshot)
        .collect()
}

fn host_rollout_state_to_snapshot(
    record: nixfleet_state_machine::HostRolloutState,
) -> nixfleet_proto::agent_wire::HostRolloutSnapshot {
    use nixfleet_proto::HostRolloutState as WireState;
    use nixfleet_state_machine::HostState;
    let wire_state = match record.state {
        HostState::Pending => WireState::Pending,
        HostState::Activating => WireState::Activating,
        HostState::Deferred => WireState::Deferred,
        HostState::Soaking => WireState::Soaking,
        HostState::Converged => WireState::Converged,
        HostState::Failed => WireState::Failed,
        HostState::Reverted => WireState::Reverted,
    };
    nixfleet_proto::agent_wire::HostRolloutSnapshot {
        rollout_id: record.rollout_id,
        hostname: record.hostname,
        channel: record.channel,
        state: wire_state,
        target_closure: record.target_closure,
        current_closure_at_dispatch: record.current_closure_at_dispatch,
        current_closure: record.current_closure,
        dispatched_at: record.dispatched_at,
        dispatch_acked_at: record.dispatch_acked_at,
        activation_started_at: record.activation_started_at,
        activation_completed_at: record.activation_completed_at,
        soak_due_at: record.soak_due_at,
        last_event_seq: record.last_event_seq,
    }
}

/// Scan active host_rollout_records for `host`; for each record whose
/// `target_closure` matches the agent's reported `current_closure`,
/// synthesize the event chain that advances the row to a state
/// consistent with the agent's observation. Idempotent: if the state
/// has already advanced (e.g. concurrent agent emit), the record won't
/// match and the synthesis is a no-op.
///
/// LOADBEARING: this is the CP-side half of architecture.md §305
/// acceptance gate 1 ("destroying the CP database and rebuilding from
/// empty state results in full fleet visibility within one reconcile
/// cycle, with zero operator intervention beyond restarting the
/// service"). The agent's heartbeat carries `current_closure` (LIFT
/// #5) on every tick; CP rebuilds soft-state HRR rows from those
/// inputs.
///
/// Three reachable starting states:
///   - `Activating` — LIFT #1: agent restarted mid-rollout, boot
///     observed `current == target`. Synthesise `RemoteActivationCompleted`.
///   - `Deferred`   — Option C: operator rebooted to finish a
///     critical-component activation. Same synthesis.
///   - `Pending`    — LIFT #5: CP itself was wiped, planner re-opened
///     the rollout in `Pending`, but the agent has been running the
///     target closure all along. Synthesise the full
///     `RemoteDispatchAck → RemoteActivationCompleted → RemoteConverged`
///     chain. `RemoteConverged`'s soak-elapsed invariant is satisfied
///     by stamping `converged_at = max(at, record.soak_due_at)` — the
///     soak window's purpose (give probes time to fail) was exercised
///     pre-wipe, so the post-wipe row's freshly-stamped `soak_due_at`
///     does not gate convergence.
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
        if record.target_closure != agent_current {
            continue;
        }
        let rollout_id = record.rollout_id.clone();
        match record.state {
            nixfleet_state_machine::HostState::Activating
            | nixfleet_state_machine::HostState::Deferred => {
                tracing::info!(
                    target: "cp_reducer",
                    host,
                    rollout_id = %record.rollout_id,
                    target = %record.target_closure,
                    prior_state = ?record.state,
                    "boot-recovery: synthesizing RemoteActivationCompleted (LIFT #1; RFC-0005 §9.5)",
                );
                let synth_event = nixfleet_state_machine::Event::RemoteActivationCompleted {
                    observed_current_closure: agent_current.to_string(),
                    exit_code: 0,
                    completed_at: at,
                    seq: record.last_event_seq + 1,
                };
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
            nixfleet_state_machine::HostState::Pending => {
                synthesize_pending_to_converged(
                    state,
                    clock,
                    event_log_tx,
                    rs,
                    &record,
                    agent_current,
                    at,
                )
                .await;
            }
            _ => continue,
        }
    }
}

/// LIFT #5: drive a `Pending` HRR row through the full lifecycle to
/// `Converged` when the agent reports `current_closure == target`.
/// The chain preserves the event-log audit trail (RFC-0004 §1):
/// every transition emits its usual `RemoteAppendEventLog` effect
/// flagged with the synthesis context via the `seq` ordering relative
/// to the pre-synthesis `last_event_seq`.
async fn synthesize_pending_to_converged(
    state: &Arc<AppState>,
    clock: &ClockHandle,
    event_log_tx: &EventLogTx,
    rs: &mut ReducerState,
    record: &nixfleet_state_machine::HostRolloutState,
    agent_current: &str,
    at: DateTime<Utc>,
) {
    let host = record.hostname.as_str();
    let rollout_id = &record.rollout_id;
    tracing::info!(
        target: "cp_reducer",
        host,
        rollout_id = %rollout_id,
        target = %record.target_closure,
        "post-wipe recovery: synthesizing Pending → Converged chain (LIFT #5; architecture.md §305)",
    );

    // 1. Pending → Activating. `current_closure_at_dispatch` is the
    //    pre-dispatch closure; CP has no way to know it post-wipe.
    //    Empty string is the documented placeholder — rollback never
    //    fires from a synthesis chain that lands at Converged (terminal),
    //    so the rollback-target ambiguity is inert.
    let dispatch_ack = nixfleet_state_machine::Event::RemoteDispatchAck {
        current_closure_at_dispatch: String::new(),
        received_at: at,
        seq: record.last_event_seq + 1,
    };
    handle_host_event(state, clock, event_log_tx, rs, host, rollout_id, dispatch_ack).await;

    // 2. Activating → Soaking.
    let activation_completed = nixfleet_state_machine::Event::RemoteActivationCompleted {
        observed_current_closure: agent_current.to_string(),
        exit_code: 0,
        completed_at: at,
        seq: record.last_event_seq + 2,
    };
    handle_host_event(
        state,
        clock,
        event_log_tx,
        rs,
        host,
        rollout_id,
        activation_completed,
    )
    .await;

    // 3. Soaking → Converged. `converged_at` is anchored to
    //    `soak_due_at` when the heartbeat arrives before soak has
    //    elapsed, so soaking.rs's `converged_at >= soak_due_at`
    //    invariant passes. The actual agent-side convergence happened
    //    pre-wipe; CP can't reconstruct that timestamp, so it stamps
    //    the post-wipe-floor instead.
    let Some(db) = state.db.as_ref() else {
        return;
    };
    let post_activation = match db.host_rollout_records().load(rollout_id.as_str(), host) {
        Ok(Some(r)) => r,
        _ => return,
    };
    let synth_converged_at = match post_activation.soak_due_at {
        Some(soak_due) if soak_due > at => soak_due,
        _ => at,
    };
    let converged = nixfleet_state_machine::Event::RemoteConverged {
        converged_at: synth_converged_at,
        current_closure: agent_current.to_string(),
        seq: record.last_event_seq + 3,
    };
    handle_host_event(state, clock, event_log_tx, rs, host, rollout_id, converged).await;
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
        // LOADBEARING: a wave is "done participating" when every host
        // is ordering-eligible — Converged OR Deferred (per
        // RFC-0005 §3 terminal-for-ordering). Deferred means
        // activation is staged but live-switch was skipped
        // (critical-component swap pending reboot); the host has done
        // what it can within the rollout step, so successor waves
        // should not stall waiting on it. Health verification (probes
        // + soak) still happens after operator reboot via
        // LIFT #1's `handle_heartbeat` synthesis (Deferred → Soaking).
        let all_ordering_eligible = current_wave.hosts.iter().all(|host| {
            fleet_state
                .host_states
                .get(&(rollout_id.clone(), host.clone()))
                .map(|s| {
                    matches!(
                        s.state,
                        nixfleet_state_machine::HostState::Converged
                            | nixfleet_state_machine::HostState::Deferred
                    )
                })
                .unwrap_or(false)
        });
        if all_ordering_eligible {
            bumps.push((rollout_id.clone(), summary.current_wave + 1));
        }
    }

    for (rollout_id, next_wave) in bumps {
        // FK is populated by the rollout reducer's
        // `RolloutEffect::UpdateCurrentWave`; the planner passes
        // None per RFC-0008 §6.1 item 3.
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
    let mut rollouts: HashMap<RolloutId, RolloutSummary> = HashMap::new();

    // For each channel with a verified rollout manifest, load all
    // host_rollout_records under the manifested rollout_id so the
    // planner can walk Pending hosts and run gates. Channels whose
    // rollout hasn't been opened yet (no DB row, no Pending records)
    // surface through the OpenRollout emission path above.
    for (channel, vm) in &manifests.rollouts {
        let manifest = vm.inner();
        // Canonical RolloutId construction (RFC-0008 §6.3): matches
        // the planner's `RolloutId::new(channel, channel_ref)` so
        // lookups by rollout_id succeed even when multiple channels
        // share a channel_ref.
        let rollout_id = nixfleet_proto::RolloutId::new(channel, &manifest.channel_ref);

        let rows = db
            .host_rollout_records()
            .all_for_rollout(rollout_id.as_str())?;
        for row in rows {
            host_states.insert((rollout_id.clone(), row.hostname.clone()), row);
        }

        // RolloutSummary metadata. Full row from the `rollouts` table
        // (RFC-0008 §6.3). Missing row ⇒ rollout not opened yet ⇒ omit
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

    // Distinct outstanding enforce-mode probe failures per
    // (rollout, host). Feeds the compliance_wave gate
    // (planner_gates::compliance_wave) per RFC-0007 §7.2.
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
