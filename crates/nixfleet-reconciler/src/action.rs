//! Reconciler decision output.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    OpenRollout {
        channel: String,
        target_ref: String,
    },
    DispatchHost {
        rollout: String,
        host: String,
        target_ref: String,
    },
    PromoteWave {
        rollout: String,
        new_wave: usize,
    },
    ConvergeRollout {
        rollout: String,
    },
    HaltRollout {
        rollout: String,
        reason: String,
    },
    /// RFC-0002 §3.2 Healthy → Soaked transition. Emitted when the
    /// reconciler observes that a host has been Healthy for at
    /// least `wave.soak_minutes`. The CP-side action processor
    /// writes `host_rollout_state.host_state = 'Soaked'` so the
    /// next reconcile tick sees the host advance.
    SoakHost {
        rollout: String,
        host: String,
    },
    /// Observability-only: an active rollout references a channel
    /// not declared in `fleet.resolved.channels`. Surfaces channel
    /// removals that leave orphaned observed state. The reconciler
    /// silently `continue`s its loop (channel teardown is a valid
    /// operator workflow); this event makes the orphaning visible
    /// in journals + traces. Issue #21 / spec OQ #5.
    ChannelUnknown {
        channel: String,
    },
    Skip {
        host: String,
        reason: String,
    },
    /// Issue #59 — wave-staging compliance gate held this wave's
    /// promotion because at least one host *in an earlier wave*
    /// has outstanding `ComplianceFailure` / `RuntimeGateError`
    /// events under enforce mode. The CP-side dispatch handler
    /// returns `target: null` to hosts in `blocked_wave`; this
    /// action surfaces the same decision in the reconciler's
    /// action plan so operators reading `nixfleet plan` see the
    /// gate as a first-class event rather than only a journal
    /// log line.
    ///
    /// **Wired but not yet emitted.** This variant is reachable
    /// over the wire today (CP→agent action streams round-trip
    /// it) and is the contract surface for the operator-visible
    /// gate event. Reconciler-side emission is gated on extending
    /// `Observed` with the per-host outstanding-event projection,
    /// which couples to the host_reports SQLite migration tracked
    /// in roadmap-0002 (the in-memory ring buffer's classification
    /// gap). Until then, the dispatch handler's `tracing::warn`
    /// at `target=dispatch` carries the same information for
    /// operators tailing the journal.
    ///
    /// `blocked_wave` is the wave whose promotion is held;
    /// `failing_hosts` is the set of hosts (on earlier waves)
    /// whose outstanding events triggered the hold;
    /// `failing_events_count` is the total number of unresolved
    /// events across those hosts (1 per failing control or
    /// runtime-gate error).
    WaveBlocked {
        rollout: String,
        blocked_wave: usize,
        failing_hosts: Vec<String>,
        failing_events_count: usize,
    },
}
