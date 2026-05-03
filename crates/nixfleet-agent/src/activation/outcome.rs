//! Outcome enums + shared poll-budget constants.

use std::time::Duration;

// LOADBEARING: 300s must stay inside CP's DEFAULT_CONFIRM_DEADLINE_SECS=360 — exceeding splits state.
pub const POLL_BUDGET: Duration = Duration::from_secs(300);

pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub enum ActivationOutcome {
    FiredAndPolled,
    RealiseFailed { reason: String },
    /// Distinct from RealiseFailed so dashboards can route trust violations.
    SignatureMismatch {
        closure_hash: String,
        stderr_tail: String,
    },
    SwitchFailed {
        phase: String,
        exit_code: Option<i32>,
    },
    /// `/run/current-system` flipped to a basename that is neither expected
    /// nor pre-switch — caller rolls back.
    VerifyMismatch {
        expected: String,
        actual: String,
    },
}

#[derive(Debug)]
pub enum RollbackOutcome {
    FiredAndPolled,
    Failed {
        phase: String,
        exit_code: Option<i32>,
    },
}

impl RollbackOutcome {
    pub fn success(&self) -> bool {
        matches!(self, RollbackOutcome::FiredAndPolled)
    }
    pub fn exit_code(&self) -> Option<i32> {
        match self {
            RollbackOutcome::Failed { exit_code, .. } => *exit_code,
            RollbackOutcome::FiredAndPolled => None,
        }
    }
    pub fn phase(&self) -> Option<&str> {
        match self {
            RollbackOutcome::Failed { phase, .. } => Some(phase.as_str()),
            RollbackOutcome::FiredAndPolled => None,
        }
    }
}
