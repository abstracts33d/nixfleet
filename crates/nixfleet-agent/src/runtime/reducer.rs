//! Reducer task body. Sole `nixfleet_state_machine::step` caller per
//! invariant (1) in `runtime::mod`.
//!
//! State held in-task:
//!   - per-rollout `HostRolloutState` keyed by `rollout_id` (the agent
//!     owns its own host's state, so the key is just the rollout id)
//!   - cached `SignedManifestSet` (refreshed by the `manifest_poll`
//!     worker per RFC-0011 §1 invariant #1 — single signed source of
//!     truth fetched + verified once per tick; the reducer reads
//!     rollout policy from it for `step()` calls)
//!
//! Seq assignment: workers emit events with `seq = 0`. The reducer
//! rewrites it to `state.last_event_seq + 1` before calling step()
//! — single mutator owns the per-rollout monotonic counter, so
//! cross-worker ordering can't race.

use std::collections::HashMap;

use nixfleet_proto::RolloutPolicy;
use nixfleet_proto::clock::ClockHandle;
use nixfleet_reconciler::planner_types::SignedManifestSet;
use nixfleet_state_machine::{Event, HostRolloutState, HostState};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use std::sync::Arc;

use super::applier::apply_effect;
use super::outbound_queue::OutboundQueue;
use super::{
    ActivationIntentTx, AgentConfig, ApplierCtx, OutboundKickTx, ProbeResetTx, ReducerInput,
    ShutdownGuard,
};

/// Sustained-failure window cap. RFC-0008 §6 — the agent transitions
/// Soaking → Failed when a probe has been failing continuously past
/// this threshold.
///
/// TODO(v0.2.1): wire from `services.nixfleet-agent.healthChecks` via
/// the NixOS module → agent CLI arg → runtime config struct, per
/// RFC-0008 §6 + §9.1 ("each agent reads it from
/// services.nixfleet-agent.healthChecks"). Bigger than v0.2 scope
/// because it touches the NixOS module surface; v0.2 ships with the
/// hardcoded floor.
///
/// Tracking issue: open on `abstracts33d/nixfleet` (Forgejo `origin`
/// or GitHub `upstream`, operator's call) — title "Wire
/// SUSTAINED_FAILURE_THRESHOLD_SECS from NixOS module config (v0.2.1)".
///
/// The hardcoded 120s is twice RFC-0008 §6's documented default
/// (60s), so under-shooting safely: real probe-failure detection
/// still fires, just 60s later than a tuned deployment would. Safe
/// for v0.2 demo + lab work; not appropriate for production fleets
/// with tight SLOs. NixOS-module wire-through to make this
/// operator-tunable is tracked in `v0.2.1-followups.md`.
const SUSTAINED_FAILURE_THRESHOLD_SECS: i64 = 120;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    cancel: CancellationToken,
    cfg: AgentConfig,
    clock: ClockHandle,
    mut input_rx: mpsc::Receiver<ReducerInput>,
    input_tx: mpsc::Sender<ReducerInput>,
    activation_tx: ActivationIntentTx,
    probe_reset_tx: ProbeResetTx,
    outbound_queue: Arc<OutboundQueue>,
    outbound_kick: OutboundKickTx,
    shutdown_senders: Vec<oneshot::Sender<()>>,
) {
    let _shutdown_guard = ShutdownGuard(shutdown_senders);

    let mut host_states: HashMap<nixfleet_proto::RolloutId, HostRolloutState> = HashMap::new();
    // Cached signed manifests. Populated by the longpoll worker after
    // each `verify_rollout_manifest` succeeds (and the boot-recovery
    // handshake in 7f); the reducer reads it to find `RolloutPolicy`
    // for step() calls. None at startup; events that arrive before
    // the cache is warm are dropped with a warn (RFC-0008 §9.5's
    // "agent can't act on unverified state").
    let mut manifests: Option<SignedManifestSet> = None;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!(
                    target: "shutdown",
                    task = "agent_reducer",
                    "task shut down",
                );
                return;
            }
            maybe_input = input_rx.recv() => {
                let Some(input) = maybe_input else { return };
                let ctx = ApplierCtx {
                    cfg: &cfg,
                    clock: &clock,
                    input_tx: &input_tx,
                    activation_tx: &activation_tx,
                    probe_reset_tx: &probe_reset_tx,
                    outbound_queue: &outbound_queue,
                    outbound_kick: &outbound_kick,
                };
                handle_input(&mut host_states, &mut manifests, &clock, &ctx, input).await;
            }
        }
    }
}

async fn handle_input(
    host_states: &mut HashMap<nixfleet_proto::RolloutId, HostRolloutState>,
    manifests: &mut Option<SignedManifestSet>,
    clock: &ClockHandle,
    ctx: &ApplierCtx<'_>,
    input: ReducerInput,
) {
    match input {
        ReducerInput::HostEvent { rollout_id, event } => {
            run_host_event(host_states, manifests, clock, ctx, rollout_id, event).await;
        }
        ReducerInput::AgentAdvanceTick => {
            run_advance_tick(host_states, manifests, clock, ctx).await;
        }
        ReducerInput::ManifestSetUpdated(set) => {
            *manifests = Some(*set);
            tracing::info!(
                target: "agent_reducer",
                "manifest cache refreshed",
            );
        }
        ReducerInput::BootstrapHost(snapshot) => {
            apply_bootstrap_snapshot(host_states, ctx, *snapshot).await;
        }
    }
}

/// LIFT #3 + LIFT #4: apply a CP-supplied HostRolloutSnapshot to the
/// agent's in-memory reducer cache, then emit the worker re-priming
/// effects the rehydrated state demands.
///
/// Snapshot-shape, not event-replay — the canonical state lives on CP,
/// the agent's HostRolloutState is a reconstructable cache. Called from
/// two entry points: the boot-recovery handshake before workers spawn
/// (`recovery.rs`), and the steady-state heartbeat worker after CP
/// signals a fresh snapshot (`workers/heartbeat.rs`). Both paths share
/// this function so worker re-priming is consistent.
///
/// LOADBEARING: the merge is asymmetric. Canonical fields (state,
/// target_closure, dispatch/activation timestamps, last_event_seq)
/// always come from the snapshot. Agent-local-only fields that the
/// wire snapshot does NOT carry (probes, probe_observed_first_at,
/// probe_failure_first_at, failed_at, converged_at, etc.) are
/// preserved from the existing entry when one is present, defaulted
/// when not.
///
/// `probe_failure_first_at` in particular MUST survive a warm
/// heartbeat rehydration: LIFT #5 makes CP return `bootstrap_rollouts`
/// on every steady-state heartbeat (~60s cadence), and clobbering the
/// sustained-failure timer on each tick prevents `Soaking → Failed`
/// from ever firing (HEALTH_FAILURE_THRESHOLD_SECS = 120s, so a
/// 60s clobber starves the timer indefinitely).
///
/// LOADBEARING: every non-Pending rehydration emits effects via
/// `nixfleet_state_machine::rehydration_effects` and routes them through
/// `apply_effect` — the same channel workers consume during ordinary
/// transitions. Without this, probe runners (and any future worker that
/// caches per-rollout state) keep tickers tagged with stale rollout_ids
/// from a prior process incarnation; the reducer rejects the resulting
/// events with `LocalProbeResult not legal from state Converged`.
async fn apply_bootstrap_snapshot(
    host_states: &mut HashMap<nixfleet_proto::RolloutId, HostRolloutState>,
    ctx: &ApplierCtx<'_>,
    snapshot: nixfleet_proto::agent_wire::HostRolloutSnapshot,
) {
    let warm = host_states.contains_key(&snapshot.rollout_id);
    let rollout_id = snapshot.rollout_id.clone();
    let record = merge_snapshot_into_state(host_states.get(&rollout_id), snapshot);
    tracing::info!(
        target: "agent_reducer",
        rollout_id = %record.rollout_id,
        state = ?record.state,
        target_closure = %record.target_closure,
        warm,
        probe_failure_first_at = ?record.probe_failure_first_at,
        "bootstrap: rehydrating in-memory HostRolloutState from CP snapshot (LIFT #3)",
    );
    let effects = nixfleet_state_machine::rehydration_effects(&record);
    host_states.insert(rollout_id, record);
    for effect in effects {
        apply_effect(ctx, effect).await;
    }
}

/// Pure merge of a wire snapshot with the existing in-memory entry.
/// Canonical fields (state, target_closure, dispatch/activation
/// timestamps, last_event_seq) come from the snapshot. Agent-local
/// fields not carried in the wire shape are preserved from `existing`
/// when present, defaulted when not.
fn merge_snapshot_into_state(
    existing: Option<&HostRolloutState>,
    snapshot: nixfleet_proto::agent_wire::HostRolloutSnapshot,
) -> HostRolloutState {
    use nixfleet_proto::HostRolloutState as WireState;
    use nixfleet_state_machine::HostState;
    let internal_state = match snapshot.state {
        WireState::Pending => HostState::Pending,
        WireState::Activating => HostState::Activating,
        WireState::Deferred => HostState::Deferred,
        WireState::Soaking => HostState::Soaking,
        WireState::Converged => HostState::Converged,
        WireState::Failed => HostState::Failed,
        WireState::Reverted => HostState::Reverted,
    };
    HostRolloutState {
        // Canonical fields — always from the snapshot.
        rollout_id: snapshot.rollout_id,
        hostname: snapshot.hostname,
        channel: snapshot.channel,
        state: internal_state,
        target_closure: snapshot.target_closure,
        current_closure_at_dispatch: snapshot.current_closure_at_dispatch,
        current_closure: snapshot.current_closure,
        dispatched_at: snapshot.dispatched_at,
        dispatch_acked_at: snapshot.dispatch_acked_at,
        activation_started_at: snapshot.activation_started_at,
        activation_completed_at: snapshot.activation_completed_at,
        soak_due_at: snapshot.soak_due_at,
        last_event_seq: snapshot.last_event_seq,
        // Agent-local-only fields — preserved from the existing entry
        // on warm rehydration, defaulted on cold rehydration. Wire
        // snapshot does not carry these (agent_wire.rs:55-78).
        probes: existing.map(|e| e.probes.clone()).unwrap_or_default(),
        probe_observed_first_at: existing.and_then(|e| e.probe_observed_first_at),
        probe_failure_first_at: existing.and_then(|e| e.probe_failure_first_at),
        activation_failed_at: existing.and_then(|e| e.activation_failed_at),
        failed_at: existing.and_then(|e| e.failed_at),
        converged_at: existing.and_then(|e| e.converged_at),
        reverted_to: existing.and_then(|e| e.reverted_to.clone()),
        reverted_at: existing.and_then(|e| e.reverted_at),
        policy_applied: existing.and_then(|e| e.policy_applied),
    }
}

async fn run_host_event(
    host_states: &mut HashMap<nixfleet_proto::RolloutId, HostRolloutState>,
    manifests: &Option<SignedManifestSet>,
    clock: &ClockHandle,
    ctx: &ApplierCtx<'_>,
    rollout_id: nixfleet_proto::RolloutId,
    event: Event,
) {
    // Bootstrap from LocalActivate: when no state exists yet, the
    // event must be a fresh dispatch. `target_closure` is carried on
    // the event payload, having been validated by the longpoll
    // worker's `manifest_cache.ensure_for_dispatch` call against the
    // freshly-fetched per-rollout manifest.
    //
    // LOADBEARING: the reducer's own `manifests` cache is NOT
    // consulted at bootstrap — that cache is fed by
    // `agent_manifest_poll` on a slower cadence and can be stale
    // immediately after a new rollout's channel_ref is published,
    // producing a TOCTOU between longpoll's verify and the reducer's
    // snapshot read. Carrying the validated value with the event
    // makes the trust chain explicit: longpoll verifies against the
    // signed manifest, longpoll passes the value forward, reducer
    // consumes it without re-derivation. RFC-0011 §1 invariant 1.
    let prior = host_states.get(&rollout_id).cloned();
    let (state, policy_channel) = match (prior, &event) {
        (Some(s), _) => {
            let ch = s.channel.clone();
            (s, ch)
        }
        (
            None,
            Event::LocalActivate {
                target_closure,
                soak_due_at,
                ..
            },
        ) => {
            let now = clock.now();
            let state = bootstrap_pending_state(&rollout_id, target_closure, *soak_due_at, now);
            (state, "<bootstrap>".to_string())
        }
        (None, _) => {
            tracing::warn!(
                target: "agent_reducer",
                %rollout_id,
                ?event,
                "HostEvent for unknown rollout (no LocalActivate seen yet); dropping",
            );
            return;
        }
    };

    let Some(policy) = resolve_policy(manifests, &state.channel) else {
        tracing::warn!(
            target: "agent_reducer",
            %rollout_id,
            channel = %state.channel,
            policy_channel,
            "HostEvent: rollout policy not cached yet; dropping event (longpoll will refill)",
        );
        return;
    };

    // Rewrite seq so workers can emit with seq=0 and the reducer owns
    // the per-rollout monotonic counter (single mutator → no race).
    let next_seq = state.last_event_seq + 1;
    let event = with_seq(event, next_seq);

    let now = clock.now();
    let (next_state, effects) = match nixfleet_state_machine::step(state, event, now, &policy) {
        Ok(out) => out,
        Err(err) => {
            tracing::warn!(
                target: "agent_reducer",
                %rollout_id,
                error = %err,
                "step() rejected event — illegal transition or invariant violation",
            );
            return;
        }
    };
    host_states.insert(rollout_id, next_state);

    // Effects carry their own `rollout_id` per RFC-0009 §9. Applier
    // reads directly from the effect variant.
    for effect in effects {
        apply_effect(ctx, effect).await;
    }
}

async fn run_advance_tick(
    host_states: &mut HashMap<nixfleet_proto::RolloutId, HostRolloutState>,
    manifests: &Option<SignedManifestSet>,
    clock: &ClockHandle,
    ctx: &ApplierCtx<'_>,
) {
    let now = clock.now();
    // Collect synthesised events first so we don't mutate the map
    // while iterating.
    let mut synth: Vec<(nixfleet_proto::RolloutId, Event)> = Vec::new();

    // Fail-gate: Soaking → Failed via LocalSustainedFailureCrossed. Mode
    // filter on the failing-probe set per RFC-0010 §3.3 (ProbeMode
    // docstring): only Enforce-mode probes contribute to sustained-failure.
    for (rollout_id, state) in host_states.iter() {
        if state.state != HostState::Soaking {
            continue;
        }
        let Some(first_failed) = state.probe_failure_first_at else {
            continue;
        };
        if (now - first_failed).num_seconds() < SUSTAINED_FAILURE_THRESHOLD_SECS {
            continue;
        }
        let failing_probes = collect_failing_enforce_probes(&state.probes);
        if failing_probes.is_empty() {
            continue;
        }
        let policy_applied = resolve_policy(manifests, &state.channel)
            .map(|p| p.on_health_failure)
            .unwrap_or(nixfleet_proto::OnHealthFailure::Halt);
        synth.push((
            rollout_id.clone(),
            Event::LocalSustainedFailureCrossed {
                failed_at: now,
                sustained_duration_secs: (now - first_failed).num_seconds() as u64,
                failing_probes,
                policy_applied,
                seq: 0, // run_host_event rewrites
            },
        ));
    }

    // Pass-gate: Soaking → Converged via LocalConvergedReached. Three
    // RFC-0008 §4.2 invariants: current==target, soak_due_at elapsed, all
    // enforce-mode probes Pass. Mode filter on the probe-pass check
    // brings convergence into parity with the fail-gate above per
    // RFC-0010 §3.3 (ProbeMode docstring). The shared verifier
    // (state-machine soaking::verify_converged_invariants) re-checks
    // these at step() time before transitioning.
    for (rollout_id, state) in host_states.iter() {
        if state.state != HostState::Soaking {
            continue;
        }
        let Some(soak_due_at) = state.soak_due_at else {
            continue;
        };
        if now < soak_due_at {
            continue;
        }
        let Some(current) = state.current_closure.as_ref() else {
            continue;
        };
        if *current != state.target_closure {
            continue;
        }
        if !all_enforce_probes_pass(&state.probes) {
            continue;
        }
        synth.push((
            rollout_id.clone(),
            Event::LocalConvergedReached {
                converged_at: now,
                current_closure: current.clone(),
                seq: 0, // run_host_event rewrites
            },
        ));
    }

    for (rollout_id, event) in synth {
        run_host_event(host_states, manifests, clock, ctx, rollout_id, event).await;
    }
}

/// First-touch bootstrap for a fresh `LocalActivate` event. Pure: derives
/// channel from the canonical `RolloutId` composite (RFC-0012 §6.3); the
/// caller threads in the manifest-looked-up `target_closure` for this
/// host (selecting by `hostname == cfg.machine_id`) and the
/// CP-resolved `soak_due_at` carried by the `LocalActivate` event
/// from `DispatchResponse.soak_due_at` (CP is the single source of
/// truth for the policy-resolved soak window per RFC-0011 §1
/// invariant 1). Caller also threads `now` so the helper stays
/// clock-injection-free.
fn bootstrap_pending_state(
    rollout_id: &nixfleet_proto::RolloutId,
    target_closure: &str,
    soak_due_at: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> HostRolloutState {
    let channel = rollout_id.channel().to_string();
    HostRolloutState::new_pending(
        rollout_id.clone(),
        "self".to_string(),
        channel,
        target_closure.to_string(),
        now,
        soak_due_at,
    )
}

/// Collect probe names that are currently failing AND declared with
/// `mode = Enforce`. Per RFC-0010 §3.4, only `Enforce`-mode probes
/// participate in the soak gate; `Observe` and `Disabled` records
/// events but does not gate. The pre-fix builder filtered only by
/// `status == Fail`, which silently included failing `Observe`-mode
/// probes in `LocalSustainedFailureCrossed.failing_probes` and gated
/// soak promotion against the documented contract.
fn collect_failing_enforce_probes(
    probes: &HashMap<String, nixfleet_state_machine::ProbeRecord>,
) -> Vec<String> {
    probes
        .iter()
        .filter(|(_, r)| {
            r.status == nixfleet_state_machine::ProbeStatus::Fail
                && matches!(r.mode, nixfleet_state_machine::ProbeMode::Enforce)
        })
        .map(|(name, _)| name.clone())
        .collect()
}

/// All enforce-mode probes have status `Pass`. Observe and Disabled are
/// ignored per RFC-0010 §3.3 (ProbeMode docstring, state.rs); they do not
/// gate convergence. Mirror of `collect_failing_enforce_probes` on the
/// Soaking → Converged exit path. Empty enforce set trivially satisfies
/// — matches the shared verifier's "empty probe map acceptable" semantic
/// in `verify_converged_invariants`.
fn all_enforce_probes_pass(probes: &HashMap<String, nixfleet_state_machine::ProbeRecord>) -> bool {
    probes
        .values()
        .filter(|r| matches!(r.mode, nixfleet_state_machine::ProbeMode::Enforce))
        .all(|r| r.status == nixfleet_state_machine::ProbeStatus::Pass)
}

fn resolve_policy(manifests: &Option<SignedManifestSet>, channel: &str) -> Option<RolloutPolicy> {
    let m = manifests.as_ref()?;
    let fleet = m.fleet();
    let channel_entry = fleet.channels.get(channel)?;
    fleet
        .rollout_policies
        .get(&channel_entry.rollout_policy)
        .cloned()
}

/// Rewrite the `seq` field on a `Local*` event. The reducer owns the
/// monotonic counter (single mutator) so workers can emit with `seq = 0`
/// and let this function fill it in.
fn with_seq(event: Event, seq: u64) -> Event {
    match event {
        Event::LocalActivate {
            current_closure_at_dispatch,
            target_closure,
            received_at,
            soak_due_at,
            ..
        } => Event::LocalActivate {
            current_closure_at_dispatch,
            target_closure,
            received_at,
            soak_due_at,
            seq,
        },
        Event::LocalActivationStarted {
            started_at,
            switch_method,
            ..
        } => Event::LocalActivationStarted {
            started_at,
            switch_method,
            seq,
        },
        Event::LocalActivationCompleted {
            observed_current_closure,
            exit_code,
            completed_at,
            ..
        } => Event::LocalActivationCompleted {
            observed_current_closure,
            exit_code,
            completed_at,
            seq,
        },
        Event::LocalActivationFailed {
            exit_code,
            stderr_tail,
            failed_at,
            ..
        } => Event::LocalActivationFailed {
            exit_code,
            stderr_tail,
            failed_at,
            seq,
        },
        Event::LocalProbeObservedFirst {
            probe_name,
            mode,
            observed_at,
            ..
        } => Event::LocalProbeObservedFirst {
            probe_name,
            mode,
            observed_at,
            seq,
        },
        Event::LocalProbeResult {
            probe_name,
            mode,
            status,
            observed_at,
            failure_reason,
            ..
        } => Event::LocalProbeResult {
            probe_name,
            mode,
            status,
            observed_at,
            failure_reason,
            seq,
        },
        Event::LocalProbeFailureFirst {
            probe_name,
            mode,
            first_failed_at,
            ..
        } => Event::LocalProbeFailureFirst {
            probe_name,
            mode,
            first_failed_at,
            seq,
        },
        Event::LocalSustainedFailureCrossed {
            failed_at,
            sustained_duration_secs,
            failing_probes,
            policy_applied,
            ..
        } => Event::LocalSustainedFailureCrossed {
            failed_at,
            sustained_duration_secs,
            failing_probes,
            policy_applied,
            seq,
        },
        Event::LocalRollbackCompleted {
            reverted_to_closure,
            exit_code,
            completed_at,
            ..
        } => Event::LocalRollbackCompleted {
            reverted_to_closure,
            exit_code,
            completed_at,
            seq,
        },
        Event::LocalConvergedReached {
            converged_at,
            current_closure,
            ..
        } => Event::LocalConvergedReached {
            converged_at,
            current_closure,
            seq,
        },
        Event::LocalProbeTopologyDeclared {
            probes,
            declared_at,
            ..
        } => Event::LocalProbeTopologyDeclared {
            probes,
            declared_at,
            seq,
        },
        // Remote* events should never reach the agent reducer's
        // run_host_event path. Return as-is; the upstream layer will
        // log + drop via the applier's Remote* arm.
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fixed_now() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.with_ymd_and_hms(2026, 5, 17, 12, 0, 0).unwrap()
    }

    #[test]
    fn bootstrap_extracts_channel_from_canonical_rollout_id() {
        // SR-1 regression guard: the bootstrap derives `channel` from
        // the canonical RolloutId composite, not from a scan over
        // manifests.rollouts. Verified directly against the pure helper.
        let rid = nixfleet_proto::RolloutId::new("stable", "abc1234deadbeef");
        let soak = fixed_now() + chrono::Duration::minutes(5);
        let state = bootstrap_pending_state(&rid, "closure-X", soak, fixed_now());
        assert_eq!(
            state.channel, "stable",
            "channel derived from rollout_id.channel(), not from manifest scan",
        );
    }

    #[test]
    fn bootstrap_uses_caller_provided_target_closure_not_host_set_first() {
        // SR-2 regression guard: target_closure is resolved by the
        // caller from a hostname-aware manifest lookup
        // (`hw.hostname == cfg.machine_id`). The pre-fix shape used
        // `host_set.first()` which silently produced the wrong closure
        // on any host whose hostname did not sort first in host_set.
        // The bootstrap helper is now a pure function over the
        // caller-resolved target; this test pins the helper's
        // pass-through behaviour against any future drift that would
        // re-introduce a manifest-scan inside the helper.
        let rid = nixfleet_proto::RolloutId::new("stable", "abc1234deadbeef");
        let soak = fixed_now() + chrono::Duration::minutes(5);
        let state = bootstrap_pending_state(&rid, "RIGHT-closure", soak, fixed_now());
        assert_eq!(
            state.target_closure, "RIGHT-closure",
            "target_closure comes from the caller (manifest by-hostname lookup), not from inside the helper",
        );
    }

    #[test]
    fn bootstrap_uses_caller_provided_soak_due_at_not_hardcoded() {
        // LOADBEARING: soak_due_at is the CP-resolved value carried
        // by the LocalActivate event (from
        // DispatchResponse.soak_due_at, computed by CP from the
        // manifest's
        // rollout_policies[policy].waves[wave_index].soak_minutes).
        // A hardcoded agent-side default would ignore CP's
        // resolution and force every host through the same soak
        // window regardless of policy.
        let rid = nixfleet_proto::RolloutId::new("stable", "abc1234deadbeef");
        let now = fixed_now();
        let dispatched_soak = now + chrono::Duration::seconds(0);
        let state = bootstrap_pending_state(&rid, "closure-X", dispatched_soak, now);
        assert_eq!(state.state, HostState::Pending);
        assert_eq!(
            state.soak_due_at,
            Some(dispatched_soak),
            "soak_due_at comes from the caller (CP-dispatched value), not a hardcoded default",
        );

        // And confirm a non-zero value passes through faithfully too —
        // proves the helper is genuinely caller-driven, not coincidentally
        // matching the old 5-minute default.
        let custom_soak = now + chrono::Duration::minutes(17);
        let state2 = bootstrap_pending_state(&rid, "closure-X", custom_soak, now);
        assert_eq!(state2.soak_due_at, Some(custom_soak));
    }

    #[test]
    fn bootstrap_target_closure_independent_of_manifests_snapshot() {
        // LOADBEARING: bootstrap MUST read target_closure from the
        // LocalActivate event (which longpoll filled with the
        // freshly-validated dispatch target), NOT from the reducer's
        // `manifests` snapshot. The snapshot is fed by
        // `agent_manifest_poll` on a slower cadence than longpoll's
        // dispatch arrival; when a new rollout's channel_ref is
        // freshly published, the snapshot can still hold the OLD
        // per-rollout manifest even after longpoll verified the NEW
        // target — producing a TOCTOU that strands the agent in
        // Soaking against an OLD target while CP has already moved on
        // (RFC-0011 §1 invariant 1). The pure helper is
        // caller-driven; the call site in run_host_event extracts the
        // field from the event and passes it through.
        let rid = nixfleet_proto::RolloutId::new("edge", "f8c46e472deadbeef");
        let now = fixed_now();
        let soak = now;

        // Simulate "manifest snapshot has STALE-X cached but longpoll
        // just validated FRESH-Y". Both values exercise the helper;
        // neither comes from any manifest snapshot.
        let stale_from_cache = "STALE-target-from-old-manifest".to_string();
        let fresh_from_dispatch = "FRESH-target-from-just-verified-dispatch".to_string();

        let state_stale = bootstrap_pending_state(&rid, &stale_from_cache, soak, now);
        assert_eq!(state_stale.target_closure, stale_from_cache);

        let state_fresh = bootstrap_pending_state(&rid, &fresh_from_dispatch, soak, now);
        assert_eq!(state_fresh.target_closure, fresh_from_dispatch);

        // The two values differ deliberately. The helper produces what
        // the caller asks for; neither path consults any global state.
        assert_ne!(state_stale.target_closure, state_fresh.target_closure);
    }

    fn probe_record(
        status: nixfleet_state_machine::ProbeStatus,
        mode: nixfleet_state_machine::ProbeMode,
    ) -> nixfleet_state_machine::ProbeRecord {
        nixfleet_state_machine::ProbeRecord {
            status,
            mode,
            last_observed_at: fixed_now(),
            last_pass_at: None,
            failure_reason: None,
        }
    }

    #[test]
    fn collect_failing_enforce_probes_includes_failing_enforce() {
        let mut probes = HashMap::new();
        probes.insert(
            "enforce-fail".to_string(),
            probe_record(
                nixfleet_state_machine::ProbeStatus::Fail,
                nixfleet_state_machine::ProbeMode::Enforce,
            ),
        );
        let failing = collect_failing_enforce_probes(&probes);
        assert_eq!(
            failing,
            vec!["enforce-fail".to_string()],
            "failing enforce-mode probe MUST gate per RFC-0010 §3.4",
        );
    }

    #[test]
    fn collect_failing_enforce_probes_excludes_failing_observe_and_disabled() {
        // RFC-0010 §3.4 regression guard: a failing observe-mode probe
        // (e.g. an evidence-kind compliance probe with mode = "observe"
        // that triggers an audit failure) records the event but MUST
        // NOT gate soak promotion. Same for disabled mode. The pre-fix
        // builder filtered only on `status == Fail` and let observe +
        // disabled failures gate, contradicting the documented contract
        // and tripping lab's edge soak window on anssi-bp028.
        let mut probes = HashMap::new();
        probes.insert(
            "observe-fail".to_string(),
            probe_record(
                nixfleet_state_machine::ProbeStatus::Fail,
                nixfleet_state_machine::ProbeMode::Observe,
            ),
        );
        probes.insert(
            "disabled-fail".to_string(),
            probe_record(
                nixfleet_state_machine::ProbeStatus::Fail,
                nixfleet_state_machine::ProbeMode::Disabled,
            ),
        );
        probes.insert(
            "enforce-pass".to_string(),
            probe_record(
                nixfleet_state_machine::ProbeStatus::Pass,
                nixfleet_state_machine::ProbeMode::Enforce,
            ),
        );
        let failing = collect_failing_enforce_probes(&probes);
        assert!(
            failing.is_empty(),
            "observe + disabled failures and passing enforce probes MUST NOT gate; got: {failing:?}",
        );
    }

    #[test]
    fn all_enforce_probes_pass_with_empty_map_is_true() {
        let probes: HashMap<String, nixfleet_state_machine::ProbeRecord> = HashMap::new();
        assert!(
            all_enforce_probes_pass(&probes),
            "empty probe map satisfies convergence vacuously — matches shared verifier semantic",
        );
    }

    #[test]
    fn all_enforce_probes_pass_with_passing_enforce_only_is_true() {
        let mut probes = HashMap::new();
        probes.insert(
            "nginx-version".to_string(),
            probe_record(
                nixfleet_state_machine::ProbeStatus::Pass,
                nixfleet_state_machine::ProbeMode::Enforce,
            ),
        );
        assert!(all_enforce_probes_pass(&probes));
    }

    #[test]
    fn all_enforce_probes_pass_ignores_failing_observe_and_disabled() {
        // RFC-0010 §3.3 regression guard: observe + disabled probe
        // failures MUST NOT gate convergence. Mirror of the
        // collect_failing_enforce_probes filter on the soak-fail side.
        let mut probes = HashMap::new();
        probes.insert(
            "nginx-version".to_string(),
            probe_record(
                nixfleet_state_machine::ProbeStatus::Pass,
                nixfleet_state_machine::ProbeMode::Enforce,
            ),
        );
        probes.insert(
            "evidence-nis2".to_string(),
            probe_record(
                nixfleet_state_machine::ProbeStatus::Fail,
                nixfleet_state_machine::ProbeMode::Observe,
            ),
        );
        probes.insert(
            "suppressed-probe".to_string(),
            probe_record(
                nixfleet_state_machine::ProbeStatus::Fail,
                nixfleet_state_machine::ProbeMode::Disabled,
            ),
        );
        assert!(
            all_enforce_probes_pass(&probes),
            "observe + disabled failures MUST NOT gate convergence per RFC-0010 §3.3",
        );
    }

    #[test]
    fn all_enforce_probes_pass_with_failing_enforce_is_false() {
        let mut probes = HashMap::new();
        probes.insert(
            "nginx-version".to_string(),
            probe_record(
                nixfleet_state_machine::ProbeStatus::Fail,
                nixfleet_state_machine::ProbeMode::Enforce,
            ),
        );
        assert!(!all_enforce_probes_pass(&probes));
    }

    fn make_snapshot(
        rid: &nixfleet_proto::RolloutId,
        wire_state: nixfleet_proto::HostRolloutState,
        last_event_seq: u64,
    ) -> nixfleet_proto::agent_wire::HostRolloutSnapshot {
        let now = fixed_now();
        nixfleet_proto::agent_wire::HostRolloutSnapshot {
            rollout_id: rid.clone(),
            hostname: "h1".to_string(),
            channel: "stable".to_string(),
            state: wire_state,
            target_closure: "target-closure-X".to_string(),
            current_closure_at_dispatch: Some("prior-closure".to_string()),
            current_closure: Some("target-closure-X".to_string()),
            dispatched_at: now,
            dispatch_acked_at: Some(now),
            activation_started_at: Some(now),
            activation_completed_at: Some(now),
            soak_due_at: Some(now + chrono::Duration::minutes(5)),
            last_event_seq,
        }
    }

    #[test]
    fn merge_snapshot_cold_rehydration_defaults_agent_local_fields() {
        // No existing entry — agent-local fields must default. Mirrors
        // the boot-recovery handshake path.
        let rid = nixfleet_proto::RolloutId::new("stable", "abc1234deadbeef");
        let snapshot =
            make_snapshot(&rid, nixfleet_proto::HostRolloutState::Soaking, 7);
        let record = merge_snapshot_into_state(None, snapshot);
        assert_eq!(record.state, HostState::Soaking);
        assert_eq!(record.target_closure, "target-closure-X");
        assert_eq!(record.last_event_seq, 7);
        assert!(record.probes.is_empty(), "cold rehydration starts with empty probe map");
        assert_eq!(record.probe_failure_first_at, None);
        assert_eq!(record.probe_observed_first_at, None);
        assert_eq!(record.failed_at, None);
        assert_eq!(record.converged_at, None);
    }

    #[test]
    fn merge_snapshot_warm_rehydration_preserves_probe_failure_timer() {
        // LOADBEARING: Bug D regression guard. Under LIFT #5, CP returns
        // bootstrap_rollouts on every steady-state heartbeat (~60s). If
        // `probe_failure_first_at` got clobbered on each tick, the
        // sustained-failure threshold (120s) would never cross and
        // Soaking → Failed could never fire (BT-04). The merge MUST
        // preserve the timer from the existing entry.
        let rid = nixfleet_proto::RolloutId::new("stable", "abc1234deadbeef");
        let failure_stamp = fixed_now() - chrono::Duration::seconds(90);
        let mut existing = HostRolloutState::new_pending(
            rid.clone(),
            "h1".to_string(),
            "stable".to_string(),
            "target-closure-X".to_string(),
            fixed_now() - chrono::Duration::minutes(2),
            fixed_now() + chrono::Duration::minutes(5),
        );
        existing.state = HostState::Soaking;
        existing.probe_failure_first_at = Some(failure_stamp);
        existing.probe_observed_first_at = Some(failure_stamp);
        existing.probes.insert(
            "tcp-fail".to_string(),
            probe_record(
                nixfleet_state_machine::ProbeStatus::Fail,
                nixfleet_state_machine::ProbeMode::Enforce,
            ),
        );

        let snapshot =
            make_snapshot(&rid, nixfleet_proto::HostRolloutState::Soaking, 12);
        let record = merge_snapshot_into_state(Some(&existing), snapshot);

        assert_eq!(record.state, HostState::Soaking, "canonical state still comes from snapshot");
        assert_eq!(record.last_event_seq, 12, "last_event_seq still comes from snapshot");
        assert_eq!(
            record.probe_failure_first_at,
            Some(failure_stamp),
            "probe_failure_first_at MUST survive warm rehydration so sustained-failure timer can accumulate",
        );
        assert_eq!(
            record.probe_observed_first_at,
            Some(failure_stamp),
            "probe_observed_first_at also preserved (agent-local, not in wire snapshot)",
        );
        assert!(
            record.probes.contains_key("tcp-fail"),
            "existing probe map preserved so probe scheduler doesn't lose mid-flight tracking",
        );
    }

    #[test]
    fn merge_snapshot_warm_rehydration_advances_state_but_preserves_probes() {
        // Symmetric to the BT-04 case: when CP synthesises a state
        // advance (e.g. LIFT #5 Pending → Soaking after wipe), the
        // record's state must update to match CP's view AND probe
        // state survives. Pinned so future drift can't trade one bug
        // for the other.
        let rid = nixfleet_proto::RolloutId::new("stable", "abc1234deadbeef");
        let mut existing = HostRolloutState::new_pending(
            rid.clone(),
            "h1".to_string(),
            "stable".to_string(),
            "target-closure-X".to_string(),
            fixed_now(),
            fixed_now() + chrono::Duration::minutes(5),
        );
        existing.state = HostState::Pending;
        existing.probe_failure_first_at = Some(fixed_now() - chrono::Duration::seconds(30));

        let snapshot =
            make_snapshot(&rid, nixfleet_proto::HostRolloutState::Soaking, 5);
        let record = merge_snapshot_into_state(Some(&existing), snapshot);

        assert_eq!(record.state, HostState::Soaking);
        assert_eq!(
            record.probe_failure_first_at,
            existing.probe_failure_first_at,
            "probe timer survives state advance",
        );
    }
}
