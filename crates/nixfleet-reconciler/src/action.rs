//! Reconciler decision output.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    OpenRollout { channel: String, target_ref: String },
    DispatchHost { rollout: String, host: String, target_ref: String },
    PromoteWave { rollout: String, new_wave: usize },
    ConvergeRollout { rollout: String },
    HaltRollout { rollout: String, reason: String },
    Skip { host: String, reason: String },
}
