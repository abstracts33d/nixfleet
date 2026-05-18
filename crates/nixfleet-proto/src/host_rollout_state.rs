//! Wire-side per-host rollout state. Mirrors RFC-0008 §3's 6-state machine.
//!
//! The CP's internal source of truth lives in
//! [`nixfleet_state_machine::HostState`]; this proto type exists for
//! HTTP / JSON serialization on the legacy `/v1/hosts` view layer. The
//! six variants are 1:1 with the state-machine enum.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRolloutStateParseError {
    pub got: String,
}

impl std::fmt::Display for HostRolloutStateParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown host_rollout_state: {:?}", self.got)
    }
}

impl std::error::Error for HostRolloutStateParseError {}

/// 7-state machine per RFC-0008 §3. Pre-v0.2 carried `Queued`,
/// `Dispatched`, `ConfirmWindow`, `Healthy`, `Soaked` — those are gone.
/// `Deferred` is the v0.2.x lift addition (Option C / D-027): activation
/// staged but live-switch skipped pending operator reboot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HostRolloutState {
    Pending,
    Activating,
    Deferred,
    Soaking,
    Converged,
    Failed,
    Reverted,
}

impl HostRolloutState {
    /// Canonical literal — matches the SQL CHECK on
    /// `host_rollout_records.state` and the RFC-0008 §3 wire shape.
    pub fn as_db_str(&self) -> &'static str {
        match self {
            HostRolloutState::Pending => "Pending",
            HostRolloutState::Activating => "Activating",
            HostRolloutState::Deferred => "Deferred",
            HostRolloutState::Soaking => "Soaking",
            HostRolloutState::Converged => "Converged",
            HostRolloutState::Failed => "Failed",
            HostRolloutState::Reverted => "Reverted",
        }
    }

    pub fn from_db_str(s: &str) -> Result<Self, HostRolloutStateParseError> {
        match s {
            "Pending" => Ok(HostRolloutState::Pending),
            "Activating" => Ok(HostRolloutState::Activating),
            "Deferred" => Ok(HostRolloutState::Deferred),
            "Soaking" => Ok(HostRolloutState::Soaking),
            "Converged" => Ok(HostRolloutState::Converged),
            "Failed" => Ok(HostRolloutState::Failed),
            "Reverted" => Ok(HostRolloutState::Reverted),
            other => Err(HostRolloutStateParseError {
                got: other.to_string(),
            }),
        }
    }

    /// Terminal-for-ordering: predecessor hosts/waves can release once
    /// every host hits this state. Converged is the canonical
    /// health-verified state; Deferred is "staged for reboot,
    /// ordering-eligible but not health-verified" — both clear
    /// host-edges + wave-promotion gates (Option C / D-027 lift).
    /// `channel_edges` keeps the stricter Converged-only predicate
    /// (cross-channel cascade should wait for actual verification).
    pub fn is_terminal_for_ordering(&self) -> bool {
        matches!(self, Self::Converged | Self::Deferred)
    }

    /// Host is consuming a disruption-budget slot (still moving through
    /// activation / soak). Deferred is NOT in-flight: the in-memory
    /// activation work is done, the host is just waiting on the
    /// operator to reboot.
    pub fn is_in_flight(&self) -> bool {
        matches!(self, Self::Pending | Self::Activating | Self::Soaking)
    }

    /// Stuck and staying stuck; needs operator action.
    pub fn is_failed(&self) -> bool {
        matches!(self, Self::Failed | Self::Reverted)
    }
}

#[cfg(feature = "rusqlite")]
mod rusqlite_impls {
    use super::*;
    use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};

    impl ToSql for HostRolloutState {
        fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
            Ok(ToSqlOutput::Borrowed(self.as_db_str().into()))
        }
    }

    impl FromSql for HostRolloutState {
        fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
            let s = value.as_str()?;
            Self::from_db_str(s).map_err(|e| FromSqlError::Other(Box::new(e)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_known_values() {
        for v in [
            HostRolloutState::Pending,
            HostRolloutState::Activating,
            HostRolloutState::Deferred,
            HostRolloutState::Soaking,
            HostRolloutState::Converged,
            HostRolloutState::Failed,
            HostRolloutState::Reverted,
        ] {
            assert_eq!(HostRolloutState::from_db_str(v.as_db_str()).unwrap(), v);
        }
    }

    #[test]
    fn legacy_variants_no_longer_parse() {
        for legacy in ["Queued", "Dispatched", "ConfirmWindow", "Healthy", "Soaked"] {
            assert!(
                HostRolloutState::from_db_str(legacy).is_err(),
                "v0.1 variant {legacy} must not parse against v0.2 wire shape",
            );
        }
    }

    #[test]
    fn unknown_strings_error() {
        assert!(HostRolloutState::from_db_str("").is_err());
        assert!(HostRolloutState::from_db_str("pending").is_err());
        assert!(HostRolloutState::from_db_str("Pendng").is_err());
    }
}
