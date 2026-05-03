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
    /// Emitted alongside `HaltRollout` for Failed hosts under
    /// `rollback-and-halt`. Action-plan record only — the CP-side
    /// checkin pipeline ships the actual `RollbackSignal`.
    RollbackHost {
        rollout: String,
        host: String,
        target_ref: String,
    },
    /// Healthy → Soaked transition once the host has been Healthy for
    /// at least `wave.soak_minutes`.
    SoakHost {
        rollout: String,
        host: String,
    },
    /// Observability-only: rollout references a channel no longer in
    /// `fleet.resolved.channels`. Reconciler silently continues.
    ChannelUnknown {
        channel: String,
    },
    Skip {
        host: String,
        reason: String,
    },
    /// Wave-staging compliance gate held promotion: under `enforce` mode,
    /// at least one host in an earlier wave has outstanding
    /// `ComplianceFailure` / `RuntimeGateError` events under THIS
    /// rollout's id (per-rollout grouping enforces resolution-by-replacement).
    WaveBlocked {
        rollout: String,
        blocked_wave: usize,
        failing_hosts: Vec<String>,
        failing_events_count: usize,
    },
}
