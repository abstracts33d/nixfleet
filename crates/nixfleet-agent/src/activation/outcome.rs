//! Outcome enums emitted by the activate / rollback pipelines, plus
//! the shared poll-budget constants.

use std::time::Duration;

/// 300s sized to fit inside the CP's `DEFAULT_CONFIRM_DEADLINE_SECS = 360`.
pub const POLL_BUDGET: Duration = Duration::from_secs(300);

pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub enum ActivationOutcome {
    /// Fire-and-forget completed: switch fired AND
    /// `/run/current-system` flipped to expected. By the time this
    /// returns the system *is* running the new closure, but the
    /// activation work happened in `nixfleet-switch.service`.
    /// Caller should POST `/v1/agent/confirm`.
    FiredAndPolled,
    /// `nix-store --realise` failed (non-signature). System never
    /// switched; caller skips rollback, retries next tick.
    RealiseFailed { reason: String },
    /// `nix-store --realise` failed because the closure's narinfo
    /// signature didn't match any key in `nixfleet.trust.cacheKeys`.
    /// Distinct so dashboards can route trust violations separately
    /// from transient fetch failures. System never switched.
    SignatureMismatch {
        closure_hash: String,
        stderr_tail: String,
    },
    /// `phase`:
    /// - `nix-env-set` — setting the system profile (rollback re-points it)
    /// - `systemd-run-fire` — queueing the transient unit (systemd refused)
    /// - `switch-poll-timeout` — budget elapsed without `/run/current-system` flip
    SwitchFailed {
        phase: String,
        exit_code: Option<i32>,
    },
    /// Post-switch verify caught `/run/current-system` resolving to a
    /// basename that is neither the expected new closure nor the
    /// pre-switch basename. Symptom of a concurrent `nix-env --set`,
    /// a profile-self-correction misfire, or a hostile activation
    /// script. Caller rolls back to a known-good generation.
    VerifyMismatch {
        expected: String,
        actual: String,
    },
}

/// Outcome of a `rollback()` call. Mirrors `ActivationOutcome`'s
/// shape so callers can pattern-match similarly. Fire-and-forget
/// applies to rollback for the same reason as activate: if the
/// rolled-back closure's activation script changes a unit definition
/// the running agent depends on (transitively — system services like
/// dbus/systemd-tmpfiles can chain into this), a synchronous spawn
/// gets SIGTERMed mid-run when systemd reloads.
#[derive(Debug)]
pub enum RollbackOutcome {
    FiredAndPolled,
    /// `phase`: `nix-env-rollback`, `discover-target`,
    /// `systemd-run-fire`, `rollback-poll-timeout`.
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
