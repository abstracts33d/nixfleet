//! Imperative shell for the agent reducer (RFC-0009 §7.1).
//!
//! Effect dispatch — the applier itself contains NO platform-specific
//! code. `LocalFireSwitch` / `LocalFireRollbackTo` send an
//! [`ActivationIntent`] to the activation worker; the worker holds the
//! per-OS branches (Linux: `systemd-run --unit=nixfleet-switch`;
//! Darwin: `setsid()`-detached launchd). Same shape for
//! `LocalResetProbeCache` (probe-worker channel).
//!
//! Keeping the platform branches off the applier preserves the
//! architectural symmetry with the CP-side applier (which has no
//! platform code either) and means the reducer task is portable across
//! Linux + Darwin (and any future target) without changes.
//!
//! `Remote*` variants are CP-only; reaching the agent applier means a
//! reducer state-machine defect. Log + drop, do not panic — a buggy
//! peer must not crash the agent.

use nixfleet_state_machine::{Effect, LogLevel};

use super::ApplierCtx;
use super::outbound_queue::{QueuedEvent, outbound_event_kind, outbound_event_seq};
use super::wire::{ActivationIntent, ProbeResetCommand};

/// Execute one `Effect` emitted by `nixfleet_state_machine::step`.
pub async fn apply_effect(ctx: &ApplierCtx<'_>, effect: Effect) {
    match effect {
        Effect::LocalFireSwitch { rollout_id, target } => {
            send_intent(
                ctx,
                ActivationIntent {
                    rollout_id,
                    target_closure: target,
                    rollback: false,
                },
            )
            .await;
        }
        Effect::LocalFireRollbackTo {
            rollout_id,
            closure,
        } => {
            send_intent(
                ctx,
                ActivationIntent {
                    rollout_id,
                    target_closure: closure,
                    rollback: true,
                },
            )
            .await;
        }
        Effect::LocalResetProbeCache { rollout_id } => {
            if let Err(err) = ctx
                .probe_reset_tx
                .send(ProbeResetCommand { rollout_id })
                .await
            {
                tracing::warn!(
                    target: "agent_applier",
                    error = %err,
                    "probe-reset channel closed; reset signal lost",
                );
            }
        }
        Effect::LocalEmitEvent {
            rollout_id,
            payload,
            durable,
        } => {
            // Persist to the disk-backed queue BEFORE returning so a
            // crash between this point and the network POST is
            // recoverable on restart (Plan 07 + RFC-0008 §9.7). The
            // outbound worker drains the queue and POSTs; on success
            // it deletes the file.
            //
            // `durable = false` events are rare (today only telemetry
            // metrics like ReducerError would set it); for v0.2 we
            // route everything through the queue + drainer to keep
            // the on-disk audit trail complete. Phase 8 may add a
            // fast-path for non-durable events if profiling shows the
            // fsync cost matters.
            let _ = durable;
            let event = QueuedEvent {
                seq: outbound_event_seq(&payload),
                hostname: ctx.cfg.machine_id.clone(),
                // `rollout_id` is carried on the effect directly (Phase 8a —
                // closes the v0.2 enrichment stopgap by lifting the field
                // onto every Local* variant per RFC-0009 §9's
                // "effects carry everything the applier needs" principle).
                rollout_id,
                event_kind: outbound_event_kind(&payload).to_string(),
                created_at: ctx.clock.now(),
                payload: nixfleet_proto::AgentEvent::from(payload),
            };
            match ctx.outbound_queue.enqueue(&event) {
                Ok(_path) => {
                    // Wake the drainer immediately rather than waiting
                    // for its periodic tick.
                    let _ = ctx.outbound_kick.send(());
                    tracing::debug!(
                        target: "agent_applier",
                        seq = event.seq,
                        kind = %event.event_kind,
                        "LocalEmitEvent enqueued",
                    );
                }
                Err(err) => {
                    // fsync / rename failure on the queue dir is
                    // operator-actionable (disk full, FS read-only).
                    // We log + drop; the agent stays alive but the
                    // event is lost. Phase 8 metric:
                    // `agent_outbound_queue_enqueue_failures_total`.
                    tracing::error!(
                        target: "agent_applier",
                        seq = event.seq,
                        kind = %event.event_kind,
                        error = %err,
                        "LocalEmitEvent enqueue failed; event LOST — investigate state-dir disk health",
                    );
                }
            }
        }

        Effect::RecordTransition {
            host,
            rollout_id,
            from,
            to,
            at,
        } => {
            tracing::info!(
                target: "agent_applier",
                %host,
                %rollout_id,
                ?from,
                ?to,
                %at,
                "RecordTransition",
            );
        }
        Effect::EmitMetric {
            name,
            labels,
            value,
        } => {
            tracing::debug!(
                target: "agent_applier",
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
        } => match level {
            LogLevel::Trace => tracing::trace!(
                target: "agent_runtime_emitted",
                emitted_target = target,
                ?fields,
                "{message}",
            ),
            LogLevel::Debug => tracing::debug!(
                target: "agent_runtime_emitted",
                emitted_target = target,
                ?fields,
                "{message}",
            ),
            LogLevel::Info => tracing::info!(
                target: "agent_runtime_emitted",
                emitted_target = target,
                ?fields,
                "{message}",
            ),
            LogLevel::Warn => tracing::warn!(
                target: "agent_runtime_emitted",
                emitted_target = target,
                ?fields,
                "{message}",
            ),
            LogLevel::Error => tracing::error!(
                target: "agent_runtime_emitted",
                emitted_target = target,
                ?fields,
                "{message}",
            ),
        },

        Effect::RemoteQueueDispatch { .. }
        | Effect::RemoteInsertQuarantine { .. }
        | Effect::RemoteOpenRolloutRecord { .. }
        | Effect::RemoteAppendEventLog { .. } => {
            tracing::error!(
                target: "agent_applier",
                effect = ?remote_kind(&effect),
                "apply_effect: CP-only Remote* effect reached the agent applier — \
                 reducer state-machine defect. Dropping.",
            );
        }
    }
}

async fn send_intent(ctx: &ApplierCtx<'_>, intent: ActivationIntent) {
    if let Err(err) = ctx.activation_tx.send(intent).await {
        tracing::error!(
            target: "agent_applier",
            error = %err,
            "activation-intent channel closed; switch/rollback intent lost",
        );
    }
}

fn remote_kind(e: &Effect) -> &'static str {
    match e {
        Effect::RemoteQueueDispatch { .. } => "RemoteQueueDispatch",
        Effect::RemoteInsertQuarantine { .. } => "RemoteInsertQuarantine",
        Effect::RemoteOpenRolloutRecord { .. } => "RemoteOpenRolloutRecord",
        Effect::RemoteAppendEventLog { .. } => "RemoteAppendEventLog",
        _ => "<not-remote>",
    }
}
