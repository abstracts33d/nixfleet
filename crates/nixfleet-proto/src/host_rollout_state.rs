//! Per-host rollout state machine. Canonical source of truth for
//! the variant set, shared by the CP (SQL CHECK constraint round-trip)
//! and the reconciler (decision-procedure pattern matches).
//!
//! Two consumers used to declare separate enums with identical literals.
//! The alignment was guarded only by the
//! `host_rollout_state_check_matches_enum` test in the CP crate, which
//! passes as long as both enums and the migration SQL stay in sync —
//! a fragile compile-time-check-via-test posture. Promoting the enum
//! here makes the proto crate the single source of truth; both
//! consumers re-export.
//!
//! `as_db_str` and `from_db_str` carry the SQL-literal name even though
//! the same strings are used on the reconciler's wire-shaped
//! `observed.json` fixtures. The discipline is "one literal, one shape,
//! one set of accessors" — naming it after the SQL boundary follows
//! the pre-existing module convention in `nixfleet-control-plane::state`.

use serde::{Deserialize, Serialize};

/// Returned by [`HostRolloutState::from_db_str`] when the input does
/// not match a known variant. Implements `std::error::Error` so it
/// converts cleanly into `anyhow::Error` at consumer crates and is
/// accepted by `serde::de::Error::custom`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRolloutStateParseError {
    /// The unknown literal that was passed in.
    pub got: String,
}

impl std::fmt::Display for HostRolloutStateParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown host_rollout_state: {:?}", self.got)
    }
}

impl std::error::Error for HostRolloutStateParseError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HostRolloutState {
    Queued,
    Dispatched,
    Activating,
    ConfirmWindow,
    Healthy,
    Soaked,
    Converged,
    Reverted,
    Failed,
}

impl HostRolloutState {
    /// Canonical literal. Matches the `host_rollout_state.host_state`
    /// CHECK constraint in V003 and the wire string emitted in
    /// `observed.json` fixtures.
    pub fn as_db_str(&self) -> &'static str {
        match self {
            HostRolloutState::Queued => "Queued",
            HostRolloutState::Dispatched => "Dispatched",
            HostRolloutState::Activating => "Activating",
            HostRolloutState::ConfirmWindow => "ConfirmWindow",
            HostRolloutState::Healthy => "Healthy",
            HostRolloutState::Soaked => "Soaked",
            HostRolloutState::Converged => "Converged",
            HostRolloutState::Reverted => "Reverted",
            HostRolloutState::Failed => "Failed",
        }
    }

    /// Parse a literal back into the typed variant. Returns an error
    /// on unknown strings so a future schema drift surfaces loudly
    /// rather than silently mis-classifying.
    pub fn from_db_str(s: &str) -> Result<Self, HostRolloutStateParseError> {
        match s {
            "Queued" => Ok(HostRolloutState::Queued),
            "Dispatched" => Ok(HostRolloutState::Dispatched),
            "Activating" => Ok(HostRolloutState::Activating),
            "ConfirmWindow" => Ok(HostRolloutState::ConfirmWindow),
            "Healthy" => Ok(HostRolloutState::Healthy),
            "Soaked" => Ok(HostRolloutState::Soaked),
            "Converged" => Ok(HostRolloutState::Converged),
            "Reverted" => Ok(HostRolloutState::Reverted),
            "Failed" => Ok(HostRolloutState::Failed),
            other => Err(HostRolloutStateParseError {
                got: other.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_known_values() {
        for v in [
            HostRolloutState::Queued,
            HostRolloutState::Dispatched,
            HostRolloutState::Activating,
            HostRolloutState::ConfirmWindow,
            HostRolloutState::Healthy,
            HostRolloutState::Soaked,
            HostRolloutState::Converged,
            HostRolloutState::Reverted,
            HostRolloutState::Failed,
        ] {
            assert_eq!(HostRolloutState::from_db_str(v.as_db_str()).unwrap(), v);
        }
    }

    #[test]
    fn unknown_strings_error() {
        assert!(HostRolloutState::from_db_str("").is_err());
        assert!(HostRolloutState::from_db_str("healthy").is_err()); // case-sensitive
        assert!(HostRolloutState::from_db_str("soaked").is_err());
        assert!(HostRolloutState::from_db_str("Healhty").is_err()); // typo guard
    }
}
