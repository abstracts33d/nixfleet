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
}
