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
    /// Healthy → Soaked transition. Emitted when the
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
    /// in journals + traces. / .
    ChannelUnknown {
        channel: String,
    },
    Skip {
        host: String,
        reason: String,
    },
    /// — wave-staging compliance gate held this wave's
    /// promotion because at least one host *in an earlier wave*
    /// has outstanding `ComplianceFailure` / `RuntimeGateError`
    /// events under enforce mode. The CP-side dispatch handler
    /// returns `target: null` to hosts in `blocked_wave`; this
    /// action surfaces the same decision in the reconciler's
    /// action plan so operators reading `nixfleet plan` see the
    /// gate as a first-class event rather than only a journal
    /// log line.
    ///
    /// Emitted by `rollout_state::advance_rollout` when the
    /// channel mode is `enforce` AND at least one host on a wave
    /// before the gate's promotion target has an outstanding
    /// failure under the rollout's id (per-rollout grouping
    /// resolution-by-replacement means events bound to a
    /// superseded rollout don't gate the new one). The
    /// projection layer that feeds this comes from
    /// `db::outstanding_compliance_events_by_rollout` →
    /// `Observed.compliance_failures_by_rollout` .
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
