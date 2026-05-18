//! Imperative shell for the pure planner + reducer (RFC-0009 §7.2).
//!
//! Two entrypoints:
//!
//! - [`apply_plan_action`] executes one [`PlanAction`] emitted by
//!   `nixfleet_reconciler::plan_next`. Side effects: DB writes (open
//!   rollout, queue dispatch, mark terminal, record gate decision),
//!   event_log appends.
//!
//! - [`apply_effect`] executes one [`Effect`] emitted by
//!   `nixfleet_state_machine::step`. The CP applier handles the `Remote*`
//!   variants + the three shared variants. `Local*` variants are agent-only
//!   and reaching the CP applier indicates a code defect — the applier
//!   logs and drops them rather than panicking, so a malformed event from
//!   a broken peer cannot crash the runtime.
//!
//! Both entrypoints are async because they touch the DB pool. They must
//! not call `step()` or `plan_next()` themselves — only the reducer task
//! does that, per the one-MPSC-one-mutator invariant in `runtime::mod`.
//!
//! Error policy: per-action errors are logged and swallowed. A single bad
//! DB write must not poison the reducer task; the next `plan_next()` tick
//! re-emits the same action if its preconditions still hold (the planner
//! is pure of the applier's failure history).
//!
//! Event-log routing: every event_log append goes through the bounded MPSC
//! to the [`super::event_log_writer`] task. The applier never calls
//! `Db::event_log().append()` directly — that keeps SQLite latency out of
//! the reducer's critical section and surfaces writer hiccups as
//! backpressure on the reducer's input.

use std::sync::Arc;

use nixfleet_proto::RolloutId;
use nixfleet_proto::clock::ClockHandle;
use nixfleet_reconciler::planner::compute_soak_due_at;
use nixfleet_reconciler::planner_types::{PlanAction, SignedManifestSet};
use nixfleet_state_machine::rollout::{
    self as rollout_sm, RolloutEffect, RolloutEvent, RolloutRecord,
};
use nixfleet_state_machine::{
    Effect, HostRolloutState, HostState, LogLevel, OutboundAgentEvent, ProbeStatus,
};
use serde_json::{Value, json};

use super::EventLogTx;
use crate::db::Db;
use crate::db::dispatch_queue::QueuedDispatch;
use crate::db::event_log::{EventLogEntry, EventLogKind};
use crate::db::probe_failures;
use crate::server::AppState;

/// Per-call context. Bundles the four borrows every applier path needs so
/// individual handlers stay readable without the parameter-pyramid.
pub struct ApplierCtx<'a> {
    pub state: &'a Arc<AppState>,
    pub manifests: &'a SignedManifestSet,
    pub clock: &'a ClockHandle,
    pub event_log_tx: &'a EventLogTx,
}

/// Execute one `PlanAction`. Errors are logged + swallowed: a single bad
/// DB write must not poison the reducer task. The next plan_next() tick
/// will re-emit the same action if its preconditions still hold.
pub async fn apply_plan_action(ctx: &ApplierCtx<'_>, action: PlanAction) {
    let Some(db) = ctx.state.db.as_ref() else {
        tracing::warn!(
            target: "cp_runtime",
            action = ?action,
            "apply_plan_action: no DB attached (in-memory mode); skipping",
        );
        return;
    };
    let now = ctx.clock.now();
    match action {
        PlanAction::OpenRollout {
            rollout_id,
            channel,
            target_ref,
        } => open_rollout(ctx, db, now, &rollout_id, &channel, &target_ref).await,
        PlanAction::QueueDispatch {
            host,
            rollout,
            target_closure,
            soak_due_at,
        } => queue_dispatch(ctx, db, now, &host, &rollout, &target_closure, soak_due_at).await,
        PlanAction::RecordHaltLifted { channel } => record_halt_lifted(ctx, now, &channel).await,
        PlanAction::DeferDispatch {
            host,
            rollout,
            gate,
            reason,
        } => defer_dispatch(ctx, now, &host, &rollout, gate, &reason).await,
    }
}

async fn open_rollout(
    ctx: &ApplierCtx<'_>,
    db: &Arc<Db>,
    now: chrono::DateTime<chrono::Utc>,
    rollout_id: &RolloutId,
    channel: &str,
    target_ref: &str,
) {
    // Snapshot in-flight predecessors on this channel before the new
    // rollout's INSERT. Each predecessor gets a `SuccessorOpened`
    // routed through the rollout reducer below; the reducer's existing
    // arms transition `Opening | Active | Converging | Terminal |
    // Reverted | Failed → Superseded` and emit a
    // `RolloutEffect::RecordRolloutTransition` that the applier writes
    // via `record_rollout_transition`. This replaces the Phase-10b
    // inline `UPDATE` supersession in `record_rollout_opened` (now a
    // pure insert) — single concern per applier step (RFC-0011 §3).
    let in_flight_predecessors: Vec<RolloutId> = db
        .rollouts()
        .list_active()
        .map(|rs| {
            rs.into_iter()
                .filter(|r| r.channel == channel && &r.rollout_id != rollout_id)
                .map(|r| r.rollout_id)
                .collect()
        })
        .unwrap_or_default();

    if let Err(err) = db.rollouts().record_rollout_opened(
        rollout_id.as_str(),
        channel,
        target_ref,
        now,
        // FK NULL-able under v0.2.1 baseline (RFC-0012 §6.1 item 3 +
        // v0.2.1-followups #1).
        None,
    ) {
        tracing::error!(
            target: "cp_runtime",
            rollout_id = %rollout_id,
            %channel,
            error = %err,
            "OpenRollout: rollouts insert failed",
        );
        return;
    }

    // RolloutOpened is the creation marker; the rollout row is already
    // in state=Opening from the INSERT above. Reducer has nothing to
    // validate (no source state), so we append the event_log row
    // directly without going through process_rollout_event.
    append_rollout_event(
        ctx,
        now,
        rollout_id,
        &RolloutEvent::RolloutOpened {
            rollout_id: rollout_id.clone(),
            channel: channel.to_string(),
            target_ref: target_ref.to_string(),
            at: now,
        },
    )
    .await;

    // Drive supersession through the reducer for each predecessor —
    // process_rollout_event writes the `rollout_event` event_log entry
    // AND interprets the resulting RecordRolloutTransition effect
    // (UPDATE state='Superseded', superseded_at=now via
    // `record_rollout_transition`). Pruned predecessors are absorbed
    // by the reducer's `pruned.rs` arm → IllegalForState → logged +
    // dropped; already-Superseded predecessors are idempotent no-ops.
    for predecessor in in_flight_predecessors {
        process_rollout_event(
            ctx,
            db,
            now,
            RolloutEvent::SuccessorOpened {
                superseded_rollout_id: predecessor,
                successor_rollout_id: rollout_id.clone(),
                at: now,
            },
        )
        .await;
    }

    let Some(manifest) = ctx.manifests.rollouts.get(channel).map(|v| v.inner()) else {
        tracing::warn!(
            target: "cp_runtime",
            %rollout_id,
            %channel,
            "OpenRollout: manifest absent from cached SignedManifestSet; skipping per-host record creation",
        );
        return;
    };

    // Per-wave soak resolution. Path:
    //   manifests.fleet → channels[channel].rollout_policy
    //                  → rollout_policies[policy].waves[wave_index].soak_minutes
    // A missing link falls back to `DEFAULT_SOAK_MINUTES`; this can only
    // happen against a malformed manifest, which we'd rather flag with a
    // warn + continue than crash the runtime.
    const DEFAULT_SOAK_MINUTES: u32 = 60;
    let fleet = ctx.manifests.fleet();
    let policy = fleet
        .channels
        .get(channel)
        .and_then(|c| fleet.rollout_policies.get(&c.rollout_policy));

    let records = db.host_rollout_records();
    for hw in &manifest.host_set {
        let soak_minutes = policy
            .and_then(|p| p.waves.get(hw.wave_index as usize))
            .map(|w| w.soak_minutes)
            .unwrap_or(DEFAULT_SOAK_MINUTES);
        let soak_due_at = compute_soak_due_at(now, soak_minutes);
        let pending = HostRolloutState::new_pending(
            rollout_id.clone(),
            hw.hostname.clone(),
            channel.to_string(),
            hw.target_closure.clone(),
            now,
            soak_due_at,
        );
        if let Err(err) = records.upsert(&pending) {
            tracing::error!(
                target: "cp_runtime",
                rollout_id = %rollout_id,
                hostname = %hw.hostname,
                error = %err,
                "OpenRollout: host_rollout_records upsert failed",
            );
            continue;
        }
        // HostJoined drives the rollout reducer's Opening → Active
        // transition on the first host (RFC-0012 §3).
        process_rollout_event(
            ctx,
            db,
            now,
            RolloutEvent::HostJoined {
                rollout_id: rollout_id.clone(),
                host_id: hw.hostname.clone(),
                wave: hw.wave_index,
                at: now,
            },
        )
        .await;
    }

    append_event_log(
        ctx,
        now,
        None,
        Some(rollout_id.as_str()),
        EventLogKind::PlanAction,
        json!({
            "action": "OpenRollout",
            "rollout_id": rollout_id,
            "channel": channel,
            "target_ref": target_ref,
            "hosts": manifest.host_set.iter().map(|h| &h.hostname).collect::<Vec<_>>(),
        }),
    )
    .await;
}

async fn queue_dispatch(
    ctx: &ApplierCtx<'_>,
    db: &Arc<Db>,
    now: chrono::DateTime<chrono::Utc>,
    host: &str,
    rollout: &RolloutId,
    target_closure: &str,
    soak_due_at: chrono::DateTime<chrono::Utc>,
) {
    let queued = QueuedDispatch {
        hostname: host.to_string(),
        rollout_id: rollout.clone(),
        target_closure: target_closure.to_string(),
        soak_due_at,
        enqueued_at: now,
    };
    if let Err(err) = db.dispatch_queue().upsert(&queued) {
        tracing::error!(
            target: "cp_runtime",
            %host,
            %rollout,
            error = %err,
            "QueueDispatch: dispatch_queue upsert failed",
        );
        return;
    }
    // Wake any /v1/agent/dispatch long-pollers parked for this host. The
    // watch channel collapses bursts to one wake; a `send_replace` here
    // is preferable to `send` because subscribers may not have read the
    // last value yet and we don't want to block the applier on that.
    let _ = ctx.state.dispatch_kick.send(());
    append_event_log(
        ctx,
        now,
        Some(host),
        Some(rollout.as_str()),
        EventLogKind::PlanAction,
        json!({
            "action": "QueueDispatch",
            "host": host,
            "rollout": rollout,
            "target_closure": target_closure,
            "soak_due_at": soak_due_at.to_rfc3339(),
        }),
    )
    .await;
}

// No `mark_channel_terminal` helper: terminal transitions are driven
// by the rollout reducer via `RolloutEffect::RecordRolloutTransition`
// (RFC-0012 §5 + §7).

async fn record_halt_lifted(
    ctx: &ApplierCtx<'_>,
    now: chrono::DateTime<chrono::Utc>,
    channel: &str,
) {
    append_event_log(
        ctx,
        now,
        None,
        None,
        EventLogKind::PlanAction,
        json!({
            "action": "RecordHaltLifted",
            "channel": channel,
        }),
    )
    .await;
}

async fn defer_dispatch(
    ctx: &ApplierCtx<'_>,
    now: chrono::DateTime<chrono::Utc>,
    host: &str,
    rollout: &RolloutId,
    gate: &'static str,
    reason: &str,
) {
    append_event_log(
        ctx,
        now,
        Some(host),
        Some(rollout.as_str()),
        EventLogKind::GateDecision,
        json!({
            "gate": gate,
            "reason": reason,
            "host": host,
            "rollout": rollout,
        }),
    )
    .await;
}

/// Execute one `Effect` emitted by [`nixfleet_state_machine::step`].
///
/// Variant routing (RFC-0009 §9):
/// - `Local*` (4 variants) — agent-only. CP receiving one is a defect; log
///   at `error` and return. Never panic — a buggy peer must not crash CP.
/// - `Remote*` (5 variants) — CP-only. Handled here.
/// - Shared (3 variants: `RecordTransition`, `EmitMetric`, `EmitLog`) —
///   handled here too, identically to the agent's applier.
pub async fn apply_effect(ctx: &ApplierCtx<'_>, effect: Effect) {
    // In-memory mode (no `--db-path`): mutating arms become no-ops, but
    // shared arms (metric/log/record_transition) still emit via the
    // tracing layer and the event_log writer task (drains silently when
    // there's no DB). Single warn at entry beats a noisy per-arm one.
    if ctx.state.db.is_none() {
        tracing::debug!(
            target: "cp_runtime",
            effect = ?effect_kind(&effect),
            "apply_effect: in-memory mode; mutating arms are no-ops",
        );
    }
    let now = ctx.clock.now();
    match effect {
        // ─────────────────────────────────────────────────────────────
        // CP-only Remote* effects.
        // ─────────────────────────────────────────────────────────────
        Effect::RemoteQueueDispatch {
            host,
            rollout_id,
            target_closure,
            soak_due_at,
        } => {
            if let Some(db) = ctx.state.db.as_ref() {
                queue_dispatch(
                    ctx,
                    db,
                    now,
                    &host,
                    &rollout_id,
                    &target_closure,
                    soak_due_at,
                )
                .await;
            }
        }
        Effect::RemoteInsertQuarantine { channel, closure } => {
            if let Some(db) = ctx.state.db.as_ref()
                && let Err(err) = db.quarantined_closures().insert(
                    &channel, &closure, now,
                    // FK populated by Phase 10b's rollout reducer co-write
                    // (RFC-0012 §6.1 item 3 + §6.4).
                    None,
                )
            {
                tracing::error!(
                    target: "cp_runtime",
                    %channel,
                    %closure,
                    error = %err,
                    "RemoteInsertQuarantine: insert failed",
                );
                return;
            }
            append_event_log(
                ctx,
                now,
                None,
                None,
                EventLogKind::Effect,
                json!({
                    "effect": "RemoteInsertQuarantine",
                    "channel": channel,
                    "closure": closure,
                }),
            )
            .await;
        }
        Effect::RemoteOpenRolloutRecord {
            rollout_id,
            channel,
            host,
        } => {
            if let Some(db) = ctx.state.db.as_ref() {
                open_one_rollout_record(ctx, db, now, &rollout_id, &channel, &host).await;
            }
        }
        Effect::RemoteAppendEventLog {
            host,
            rollout_id,
            payload,
        } => {
            append_event_log(
                ctx,
                now,
                Some(&host),
                Some(rollout_id.as_str()),
                EventLogKind::AgentEvent,
                outbound_event_to_json(&payload),
            )
            .await;

            // Enforce-mode probe Fail → write a `probe_failures` row
            // per sub_result (or one row with control_id=NULL for
            // non-evidence enforce-fail). Single transaction with the
            // event_log row above is the contract (RFC-0010 §7.2); for
            // 9b we trail the event_log append and accept eventual
            // consistency within milliseconds — the writer task is
            // bounded-mpsc, so a crash between rows is a known small
            // window operators monitor via the prune-timer's metric.
            if let nixfleet_state_machine::OutboundAgentEvent::ProbeResult {
                probe_name,
                mode,
                status,
                observed_at,
                sub_results,
                ..
            } = &payload
                && matches!(mode, nixfleet_state_machine::ProbeMode::Enforce)
                && matches!(status, nixfleet_state_machine::ProbeStatus::Fail)
                && let Some(db) = ctx.state.db.as_ref()
            {
                let rows: Vec<probe_failures::ProbeFailureInsert<'_>> = match sub_results {
                    Some(srs) if !srs.is_empty() => srs
                        .iter()
                        .filter(|sr| matches!(sr.status, nixfleet_state_machine::ProbeStatus::Fail))
                        .map(|sr| probe_failures::ProbeFailureInsert {
                            rollout_id: rollout_id.as_str(),
                            host_id: &host,
                            probe_name,
                            control_id: Some(&sr.control_id),
                            framework: Some(&sr.framework),
                            observed_at: *observed_at,
                        })
                        .collect(),
                    _ => vec![probe_failures::ProbeFailureInsert {
                        rollout_id: rollout_id.as_str(),
                        host_id: &host,
                        probe_name,
                        control_id: None,
                        framework: None,
                        observed_at: *observed_at,
                    }],
                };
                if !rows.is_empty()
                    && let Err(err) = db.probe_failures().insert_many(&rows)
                {
                    tracing::warn!(
                        target: "cp_applier",
                        rollout_id = %rollout_id,
                        host = %host,
                        probe = %probe_name,
                        error = %err,
                        "probe_failures insert failed",
                    );
                }
            }
        }

        // ─────────────────────────────────────────────────────────────
        // Shared effects.
        // ─────────────────────────────────────────────────────────────
        Effect::RecordTransition {
            host,
            rollout_id,
            from,
            to,
            at,
        } => {
            append_event_log(
                ctx,
                now,
                Some(&host),
                Some(rollout_id.as_str()),
                EventLogKind::Effect,
                json!({
                    "effect": "RecordTransition",
                    "host": host,
                    "rollout_id": rollout_id.as_str(),
                    "from": host_state_str(from),
                    "to": host_state_str(to),
                    "at": at.to_rfc3339(),
                }),
            )
            .await;
        }
        Effect::EmitMetric {
            name,
            labels,
            value,
        } => {
            // The CP-side metrics surface is feature-gated and uses typed
            // helpers (`record_compliance_event`, `record_gate_block`).
            // The reducer emits generic name+labels+value; we log at debug
            // so the values are visible in JSON logs (Loki captures these),
            // and Phase 8 routes specific names through the typed surface.
            tracing::debug!(
                target: "cp_runtime",
                metric = %name,
                ?labels,
                value,
                "EmitMetric",
            );
        }
        Effect::EmitLog {
            level,
            target,
            message,
            fields,
        } => {
            // tracing's `target:` arg requires a string literal at the
            // macro site; the reducer's `target` is a `&'static str` but
            // not a literal. We log under "cp_runtime_emitted" and surface
            // the reducer-emitted target as a field. Loki dashboards that
            // care can filter on `emitted_target=...`.
            match level {
                LogLevel::Trace => tracing::trace!(
                    target: "cp_runtime_emitted",
                    emitted_target = target,
                    ?fields,
                    "{message}",
                ),
                LogLevel::Debug => tracing::debug!(
                    target: "cp_runtime_emitted",
                    emitted_target = target,
                    ?fields,
                    "{message}",
                ),
                LogLevel::Info => tracing::info!(
                    target: "cp_runtime_emitted",
                    emitted_target = target,
                    ?fields,
                    "{message}",
                ),
                LogLevel::Warn => tracing::warn!(
                    target: "cp_runtime_emitted",
                    emitted_target = target,
                    ?fields,
                    "{message}",
                ),
                LogLevel::Error => tracing::error!(
                    target: "cp_runtime_emitted",
                    emitted_target = target,
                    ?fields,
                    "{message}",
                ),
            }
        }

        // ─────────────────────────────────────────────────────────────
        // Local* — agent-only. Reaching CP is a code defect; log + drop.
        // ─────────────────────────────────────────────────────────────
        Effect::LocalFireSwitch { .. }
        | Effect::LocalFireRollbackTo { .. }
        | Effect::LocalResetProbeCache { .. }
        | Effect::LocalEmitEvent { .. } => {
            tracing::error!(
                target: "cp_runtime",
                effect = ?effect_kind(&effect),
                "apply_effect: agent-only Local* effect reached the CP applier — \
                 reducer state-machine defect. Dropping.",
            );
        }
    }
}

async fn open_one_rollout_record(
    ctx: &ApplierCtx<'_>,
    db: &Arc<Db>,
    now: chrono::DateTime<chrono::Utc>,
    rollout_id: &RolloutId,
    channel: &str,
    host: &str,
) {
    // Resolve the per-host target_closure + soak from the cached manifests.
    // Absent manifest means the reducer's cache drifted past where this
    // host's rollout still lives — log + skip.
    let Some(manifest) = ctx.manifests.rollouts.get(channel).map(|v| v.inner()) else {
        tracing::warn!(
            target: "cp_runtime",
            %rollout_id,
            %channel,
            %host,
            "RemoteOpenRolloutRecord: rollout manifest absent from cached set",
        );
        return;
    };
    let Some(hw) = manifest.host_set.iter().find(|h| h.hostname == host) else {
        tracing::warn!(
            target: "cp_runtime",
            %rollout_id,
            %channel,
            %host,
            "RemoteOpenRolloutRecord: host not in manifest host_set",
        );
        return;
    };
    const DEFAULT_SOAK_MINUTES: u32 = 60;
    let fleet = ctx.manifests.fleet();
    let soak_minutes = fleet
        .channels
        .get(channel)
        .and_then(|c| fleet.rollout_policies.get(&c.rollout_policy))
        .and_then(|p| p.waves.get(hw.wave_index as usize))
        .map(|w| w.soak_minutes)
        .unwrap_or(DEFAULT_SOAK_MINUTES);
    let soak_due_at = compute_soak_due_at(now, soak_minutes);
    let pending = HostRolloutState::new_pending(
        rollout_id.clone(),
        host.to_string(),
        channel.to_string(),
        hw.target_closure.clone(),
        now,
        soak_due_at,
    );
    if let Err(err) = db.host_rollout_records().upsert(&pending) {
        tracing::error!(
            target: "cp_runtime",
            %rollout_id,
            %host,
            error = %err,
            "RemoteOpenRolloutRecord: upsert failed",
        );
        return;
    }
    append_event_log(
        ctx,
        now,
        Some(host),
        Some(rollout_id.as_str()),
        EventLogKind::Effect,
        json!({
            "effect": "RemoteOpenRolloutRecord",
            "rollout_id": rollout_id.as_str(),
            "channel": channel,
            "host": host,
        }),
    )
    .await;
}

/// Send an entry to the bounded MPSC drained by
/// [`super::event_log_writer`]. Backpressure (full channel) blocks the
/// caller via `await` — that's the desired propagation per the audit-log
/// no-fail-open contract.
///
/// If the writer task has died (closed channel), log + return rather than
/// panic — the runtime is being torn down and losing a few tail entries
/// is acceptable.
async fn append_event_log(
    ctx: &ApplierCtx<'_>,
    ts: chrono::DateTime<chrono::Utc>,
    host_id: Option<&str>,
    rollout_id: Option<&str>,
    kind: EventLogKind,
    payload: Value,
) {
    let entry = EventLogEntry {
        kind,
        ts,
        host_id: host_id.map(str::to_string),
        rollout_id: rollout_id.map(str::to_string),
        payload: payload.to_string(),
    };
    if let Err(err) = ctx.event_log_tx.send(entry).await {
        tracing::error!(
            target: "cp_runtime",
            ?kind,
            host_id,
            rollout_id,
            error = %err,
            "append_event_log: writer channel closed",
        );
    }
}

/// Step the rollout reducer with the given event and apply its effects.
///
/// Reads the current `rollouts` row, builds a `RolloutRecord`, steps the
/// pure reducer (RFC-0012 §3), then appends a `rollout_event` row to
/// `event_log` and writes each emitted `RolloutEffect` against the
/// derived-view tables. Matches Phase 9b's eventual-consistency pattern
/// (SR-2): the event_log row and the derived-view rows are sequential
/// writes within the applier task, not a single SQL transaction.
///
/// `event_log_seq` on derived-view rows is NULL under the v0.2.1
/// baseline (RFC-0012 §6.1 item 3 + v0.2.1-followups #1); the writer
/// task is fire-and-forget so the applier doesn't know `seq` at co-
/// write time.
///
/// Unknown rollout IDs are logged and dropped — the rollout may have
/// been pruned or the event is for a CP-mirror view that hasn't caught
/// up.
pub(super) async fn process_rollout_event(
    ctx: &ApplierCtx<'_>,
    db: &Arc<Db>,
    now: chrono::DateTime<chrono::Utc>,
    event: RolloutEvent,
) {
    let rollout_id: RolloutId = rollout_event_rollout_id(&event).clone();

    let row = match db.rollouts().state(rollout_id.as_str()) {
        Ok(Some(row)) => row,
        Ok(None) => {
            tracing::debug!(
                target: "cp_runtime",
                rollout_id = %rollout_id,
                event_kind = event.kind(),
                "process_rollout_event: unknown rollout; dropping",
            );
            return;
        }
        Err(err) => {
            tracing::error!(
                target: "cp_runtime",
                rollout_id = %rollout_id,
                error = %err,
                "process_rollout_event: state() query failed",
            );
            return;
        }
    };

    let record = RolloutRecord {
        rollout_id: row.rollout_id,
        channel: row.channel,
        target_ref: row.target_ref,
        state: row.state,
        current_wave: row.current_wave,
        opened_event_log_seq: row.opened_event_log_seq,
        last_transition_event_log_seq: row.last_transition_event_log_seq,
        opened_at: row.opened_at,
        terminal_at: row.terminal_at,
        superseded_at: row.superseded_at,
    };

    append_event_log(
        ctx,
        now,
        None,
        Some(rollout_id.as_str()),
        EventLogKind::RolloutEvent,
        rollout_event_to_json(&event),
    )
    .await;

    match rollout_sm::step(record, event.clone(), now) {
        Ok((_new_record, effects)) => {
            for effect in effects {
                apply_rollout_effect(ctx, db, now, effect).await;
            }
        }
        Err(err) => {
            tracing::warn!(
                target: "cp_runtime",
                rollout_id = %rollout_id,
                event_kind = event.kind(),
                error = %err,
                "process_rollout_event: rollout step() rejected",
            );
        }
    }
}

async fn apply_rollout_effect(
    ctx: &ApplierCtx<'_>,
    db: &Arc<Db>,
    now: chrono::DateTime<chrono::Utc>,
    effect: RolloutEffect,
) {
    match effect {
        RolloutEffect::RecordRolloutTransition {
            rollout_id,
            from,
            to,
            at,
        } => {
            if let Err(err) =
                db.rollouts()
                    .record_rollout_transition(rollout_id.as_str(), to, at, None)
            {
                tracing::error!(
                    target: "cp_runtime",
                    rollout_id = %rollout_id,
                    from = from.as_db_str(),
                    to = to.as_db_str(),
                    error = %err,
                    "RolloutEffect::RecordRolloutTransition: db write failed",
                );
            }
            append_event_log(
                ctx,
                now,
                None,
                Some(rollout_id.as_str()),
                EventLogKind::Effect,
                json!({
                    "effect": "RecordRolloutTransition",
                    "rolloutId": rollout_id.as_str(),
                    "from": from.as_db_str(),
                    "to": to.as_db_str(),
                    "at": at.to_rfc3339(),
                }),
            )
            .await;
        }
        RolloutEffect::UpdateCurrentWave { rollout_id, wave } => {
            if let Err(err) = db
                .rollouts()
                .set_current_wave(rollout_id.as_str(), wave, None)
            {
                tracing::error!(
                    target: "cp_runtime",
                    rollout_id = %rollout_id,
                    wave,
                    error = %err,
                    "RolloutEffect::UpdateCurrentWave: db write failed",
                );
            }
        }
        RolloutEffect::InsertQuarantineFromRollout {
            channel,
            closure_hash,
        } => {
            if let Err(err) = db
                .quarantined_closures()
                .insert(&channel, &closure_hash, now, None)
            {
                tracing::error!(
                    target: "cp_runtime",
                    channel,
                    closure_hash,
                    error = %err,
                    "RolloutEffect::InsertQuarantineFromRollout: db write failed",
                );
            }
        }
        RolloutEffect::SchedulePruning {
            rollout_id,
            delay_seconds,
        } => {
            // Out of scope for Phase 10b. Retention-driven pruning is the
            // existing `prune_finished_rollouts` timer path; the
            // reducer-driven event-emission cycle is v0.2.x follow-up
            // territory (RFC-0012 §3 + §13).
            tracing::debug!(
                target: "cp_runtime",
                rollout_id = %rollout_id,
                delay_seconds,
                "RolloutEffect::SchedulePruning: deferred to v0.2.x follow-up",
            );
        }
    }
}

fn rollout_event_rollout_id(event: &RolloutEvent) -> &RolloutId {
    match event {
        RolloutEvent::RolloutOpened { rollout_id, .. }
        | RolloutEvent::HostJoined { rollout_id, .. }
        | RolloutEvent::HostStateChanged { rollout_id, .. }
        | RolloutEvent::WaveAdvanced { rollout_id, .. }
        | RolloutEvent::RolloutTerminal { rollout_id, .. }
        | RolloutEvent::RetentionExpired { rollout_id, .. }
        | RolloutEvent::OperatorClearance { rollout_id, .. } => rollout_id,
        // `SuccessorOpened` carries the predecessor's id as the
        // "rollout this event targets" — the successor opening is the
        // separate `RolloutOpened` event.
        RolloutEvent::SuccessorOpened {
            superseded_rollout_id,
            ..
        } => superseded_rollout_id,
    }
}

fn rollout_event_to_json(event: &RolloutEvent) -> Value {
    match event {
        RolloutEvent::RolloutOpened {
            rollout_id,
            channel,
            target_ref,
            at,
        } => json!({
            "kind": "RolloutOpened",
            "rolloutId": rollout_id,
            "channel": channel,
            "targetRef": target_ref,
            "at": at.to_rfc3339(),
        }),
        RolloutEvent::HostJoined {
            rollout_id,
            host_id,
            wave,
            at,
        } => json!({
            "kind": "HostJoined",
            "rolloutId": rollout_id,
            "hostId": host_id,
            "wave": wave,
            "at": at.to_rfc3339(),
        }),
        RolloutEvent::HostStateChanged {
            rollout_id,
            host_id,
            from,
            to,
            at,
        } => json!({
            "kind": "HostStateChanged",
            "rolloutId": rollout_id,
            "hostId": host_id,
            "from": host_state_str(*from),
            "to": host_state_str(*to),
            "at": at.to_rfc3339(),
        }),
        RolloutEvent::WaveAdvanced {
            rollout_id,
            from_wave,
            to_wave,
            at,
        } => json!({
            "kind": "WaveAdvanced",
            "rolloutId": rollout_id,
            "fromWave": from_wave,
            "toWave": to_wave,
            "at": at.to_rfc3339(),
        }),
        RolloutEvent::RolloutTerminal { rollout_id, at } => json!({
            "kind": "RolloutTerminal",
            "rolloutId": rollout_id,
            "at": at.to_rfc3339(),
        }),
        RolloutEvent::SuccessorOpened {
            superseded_rollout_id,
            successor_rollout_id,
            at,
        } => json!({
            "kind": "SuccessorOpened",
            "supersededRolloutId": superseded_rollout_id,
            "successorRolloutId": successor_rollout_id,
            "at": at.to_rfc3339(),
        }),
        RolloutEvent::RetentionExpired { rollout_id, at } => json!({
            "kind": "RetentionExpired",
            "rolloutId": rollout_id,
            "at": at.to_rfc3339(),
        }),
        RolloutEvent::OperatorClearance {
            rollout_id,
            operator,
            reason,
            at,
        } => json!({
            "kind": "OperatorClearance",
            "rolloutId": rollout_id,
            "operator": operator,
            "reason": reason,
            "at": at.to_rfc3339(),
        }),
    }
}

/// Write a bare `rollout_event` entry to event_log without going
/// through the reducer. Used for `RolloutOpened` (creation marker;
/// reducer has nothing to validate) and for any out-of-band
/// rollout-level signal.
pub(super) async fn append_rollout_event(
    ctx: &ApplierCtx<'_>,
    now: chrono::DateTime<chrono::Utc>,
    rollout_id: &RolloutId,
    event: &RolloutEvent,
) {
    append_event_log(
        ctx,
        now,
        None,
        Some(rollout_id.as_str()),
        EventLogKind::RolloutEvent,
        rollout_event_to_json(event),
    )
    .await;
}

fn host_state_str(s: HostState) -> &'static str {
    match s {
        HostState::Pending => "Pending",
        HostState::Activating => "Activating",
        HostState::Soaking => "Soaking",
        HostState::Converged => "Converged",
        HostState::Failed => "Failed",
        HostState::Reverted => "Reverted",
    }
}

fn probe_status_str(s: ProbeStatus) -> &'static str {
    match s {
        ProbeStatus::Pass => "pass",
        ProbeStatus::Fail => "fail",
    }
}

fn probe_mode_str(m: nixfleet_state_machine::ProbeMode) -> &'static str {
    use nixfleet_state_machine::ProbeMode;
    match m {
        ProbeMode::Enforce => "enforce",
        ProbeMode::Observe => "observe",
        ProbeMode::Disabled => "disabled",
    }
}

fn effect_kind(e: &Effect) -> &'static str {
    match e {
        Effect::LocalFireSwitch { .. } => "LocalFireSwitch",
        Effect::LocalFireRollbackTo { .. } => "LocalFireRollbackTo",
        Effect::LocalResetProbeCache { .. } => "LocalResetProbeCache",
        Effect::LocalEmitEvent { .. } => "LocalEmitEvent",
        Effect::RemoteQueueDispatch { .. } => "RemoteQueueDispatch",
        Effect::RemoteInsertQuarantine { .. } => "RemoteInsertQuarantine",
        Effect::RemoteOpenRolloutRecord { .. } => "RemoteOpenRolloutRecord",
        Effect::RemoteAppendEventLog { .. } => "RemoteAppendEventLog",
        Effect::RecordTransition { .. } => "RecordTransition",
        Effect::EmitMetric { .. } => "EmitMetric",
        Effect::EmitLog { .. } => "EmitLog",
    }
}

/// Convert an `OutboundAgentEvent` to its event_log JSON payload. Schema is
/// the wire-side RFC-0008 §4.2 shape (camelCase). Hand-written because the
/// state-machine crate keeps its types serde-derive-free for now; if Phase
/// 7/8 adds `Serialize` we collapse this into a single `serde_json::to_value`.
fn outbound_event_to_json(payload: &OutboundAgentEvent) -> Value {
    match payload {
        OutboundAgentEvent::DispatchAck {
            current_closure_at_dispatch,
            received_at,
            seq,
        } => json!({
            "kind": "DispatchAck",
            "currentClosureAtDispatch": current_closure_at_dispatch,
            "receivedAt": received_at.to_rfc3339(),
            "seq": seq,
        }),
        OutboundAgentEvent::ActivationStarted {
            started_at,
            switch_method,
            seq,
        } => json!({
            "kind": "ActivationStarted",
            "startedAt": started_at.to_rfc3339(),
            "switchMethod": switch_method,
            "seq": seq,
        }),
        OutboundAgentEvent::ActivationCompleted {
            observed_current_closure,
            exit_code,
            completed_at,
            seq,
        } => json!({
            "kind": "ActivationCompleted",
            "observedCurrentClosure": observed_current_closure,
            "exitCode": exit_code,
            "completedAt": completed_at.to_rfc3339(),
            "seq": seq,
        }),
        OutboundAgentEvent::ActivationFailed {
            exit_code,
            stderr_tail,
            failed_at,
            seq,
        } => json!({
            "kind": "ActivationFailed",
            "exitCode": exit_code,
            "stderrTail": stderr_tail,
            "failedAt": failed_at.to_rfc3339(),
            "seq": seq,
        }),
        OutboundAgentEvent::ActivationDeferred {
            component,
            deferred_at,
            seq,
        } => json!({
            "kind": "ActivationDeferred",
            "component": component,
            "deferredAt": deferred_at.to_rfc3339(),
            "seq": seq,
        }),
        OutboundAgentEvent::ProbeTopologyDeclared {
            probes,
            declared_at,
            seq,
        } => json!({
            "kind": "ProbeTopologyDeclared",
            "probes": probes.iter().map(|e| json!({
                "probeName": e.probe_name,
                "kind": e.kind,
                "mode": probe_mode_str(e.mode),
            })).collect::<Vec<_>>(),
            "declaredAt": declared_at.to_rfc3339(),
            "seq": seq,
        }),
        OutboundAgentEvent::ProbeObservedFirst {
            probe_name,
            mode,
            observed_at,
            seq,
        } => json!({
            "kind": "ProbeObservedFirst",
            "probeName": probe_name,
            "mode": probe_mode_str(*mode),
            "observedAt": observed_at.to_rfc3339(),
            "seq": seq,
        }),
        OutboundAgentEvent::ProbeResult {
            probe_name,
            mode,
            status,
            observed_at,
            failure_reason,
            sub_results,
            seq,
        } => json!({
            "kind": "ProbeResult",
            "probeName": probe_name,
            "mode": probe_mode_str(*mode),
            "status": probe_status_str(*status),
            "observedAt": observed_at.to_rfc3339(),
            "failureReason": failure_reason,
            "subResults": sub_results.as_ref().map(|v| v.iter().map(|sr| json!({
                "controlId": sr.control_id,
                "status": probe_status_str(sr.status),
                "framework": sr.framework,
                "article": sr.article,
            })).collect::<Vec<_>>()),
            "seq": seq,
        }),
        OutboundAgentEvent::ProbeFailureFirst {
            probe_name,
            mode,
            first_failed_at,
            seq,
        } => json!({
            "kind": "ProbeFailureFirst",
            "probeName": probe_name,
            "mode": probe_mode_str(*mode),
            "firstFailedAt": first_failed_at.to_rfc3339(),
            "seq": seq,
        }),
        OutboundAgentEvent::Failed {
            failed_at,
            sustained_duration_secs,
            failing_probes,
            policy_applied,
            seq,
        } => json!({
            "kind": "Failed",
            "failedAt": failed_at.to_rfc3339(),
            "sustainedDurationSecs": sustained_duration_secs,
            "failingProbes": failing_probes,
            "policyApplied": policy_applied.to_string(),
            "seq": seq,
        }),
        OutboundAgentEvent::RollbackComplete {
            reverted_to_closure,
            exit_code,
            completed_at,
            seq,
        } => json!({
            "kind": "RollbackComplete",
            "revertedToClosure": reverted_to_closure,
            "exitCode": exit_code,
            "completedAt": completed_at.to_rfc3339(),
            "seq": seq,
        }),
        OutboundAgentEvent::Converged {
            converged_at,
            current_closure,
            seq,
        } => json!({
            "kind": "Converged",
            "convergedAt": converged_at.to_rfc3339(),
            "currentClosure": current_closure,
            "seq": seq,
        }),
    }
}
