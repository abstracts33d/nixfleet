//! Typed state-machine enums for CP persistence rows.
//!
//! Mirrors the proto-side `compliance::GateMode` pattern: a single
//! Rust enum, with `as_db_str` / `from_db_str` accessors for the
//! SQLite boundary. The SQL CHECK constraints + column types stay
//! `TEXT`; the canonical literals are emitted from this module so
//! every call site reads the same source of truth.
//!
//! Keeping the enum in a sibling module to `db.rs` avoids pulling
//! a domain-specific type into the proto crate (it's CP-private
//! the agent never sees `pending_confirms.state` strings) while
//! still letting `db.rs`, `rollback_timer.rs`, and any future SQL
//! call sites share the same compile-time-checked variant names.

use anyhow::{anyhow, Result};

/// activation lifecycle. Persisted as TEXT in
/// `pending_confirms.state` with a CHECK constraint over the
/// canonical literals returned by [`PendingConfirmState::as_db_str`].
///
/// Lifecycle:
/// - [`Pending`](Self::Pending): row created by the dispatch loop;
///   the agent has been told to activate but has not yet confirmed.
/// - [`Confirmed`](Self::Confirmed): the agent posted
///   `/v1/agent/confirm` (or the orphan-recovery path inserted a
///   synthetic row directly in this state).
/// - [`RolledBack`](Self::RolledBack): the magic-rollback timer
///   tripped — `confirm_deadline` passed without confirmation.
/// - [`Cancelled`](Self::Cancelled): operator-driven cancellation
///   path (reserved; no caller emits this yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PendingConfirmState {
    Pending,
    Confirmed,
    RolledBack,
    Cancelled,
}

impl PendingConfirmState {
    /// Canonical SQLite literal. Matches the CHECK constraint in
    /// the V001 migration.
    pub fn as_db_str(&self) -> &'static str {
        match self {
            PendingConfirmState::Pending => "pending",
            PendingConfirmState::Confirmed => "confirmed",
            PendingConfirmState::RolledBack => "rolled-back",
            PendingConfirmState::Cancelled => "cancelled",
        }
    }

    /// Parse a TEXT column value back into the typed variant.
    /// Returns an error on unknown strings so a future schema
    /// drift surfaces loudly rather than silently mis-classifying.
    pub fn from_db_str(s: &str) -> Result<Self> {
        match s {
            "pending" => Ok(PendingConfirmState::Pending),
            "confirmed" => Ok(PendingConfirmState::Confirmed),
            "rolled-back" => Ok(PendingConfirmState::RolledBack),
            "cancelled" => Ok(PendingConfirmState::Cancelled),
            other => Err(anyhow!("unknown pending_confirms.state: {other:?}")),
        }
    }
}

/// Per-host rollout state machine. Persisted as TEXT in
/// `host_rollout_state.host_state` with a CHECK constraint over the
/// canonical literals returned by [`HostRolloutState::as_db_str`].
///
/// Mirrors [`PendingConfirmState`] — single source of truth for the
/// V003 migration's CHECK literals so a typo'd `"healhty"` is a
/// compile error rather than a silent runtime drift. The state-
/// machine semantics live in
/// `crates/nixfleet-reconciler/src/host_state.rs`; this enum is the
/// SQLite-boundary mirror.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    /// Canonical SQLite literal. Matches the CHECK constraint in
    /// the V003 migration.
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

    /// Parse a TEXT column value back into the typed variant.
    /// Returns an error on unknown strings so a future schema
    /// drift surfaces loudly rather than silently mis-classifying.
    pub fn from_db_str(s: &str) -> Result<Self> {
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
            other => Err(anyhow!("unknown host_rollout_state.host_state: {other:?}")),
        }
    }
}

/// Side-channel mutation of `host_rollout_state.last_healthy_since`
/// passed alongside a state transition. Kept off the state-machine
/// enum because the marker is orthogonal to `host_state` — entering
/// Healthy stamps it, but every other transition leaves it alone
/// unless explicitly cleared.
#[derive(Debug, Clone, Copy)]
pub enum HealthyMarker {
    /// Stamp `last_healthy_since` with the given timestamp. Used when
    /// transitioning into Healthy.
    Set(chrono::DateTime<chrono::Utc>),
    /// Leave the column as-is. Default for non-Healthy transitions.
    Untouched,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_known_values() {
        for v in [
            PendingConfirmState::Pending,
            PendingConfirmState::Confirmed,
            PendingConfirmState::RolledBack,
            PendingConfirmState::Cancelled,
        ] {
            assert_eq!(PendingConfirmState::from_db_str(v.as_db_str()).unwrap(), v);
        }
    }

    #[test]
    fn unknown_strings_error() {
        assert!(PendingConfirmState::from_db_str("").is_err());
        assert!(PendingConfirmState::from_db_str("Pending").is_err()); // case-sensitive
        assert!(PendingConfirmState::from_db_str("rolledback").is_err());
    }

    #[test]
    fn host_rollout_state_round_trip_known_values() {
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
    fn host_rollout_state_unknown_strings_error() {
        assert!(HostRolloutState::from_db_str("").is_err());
        assert!(HostRolloutState::from_db_str("healthy").is_err()); // case-sensitive
        assert!(HostRolloutState::from_db_str("soaked").is_err());
        assert!(HostRolloutState::from_db_str("Healhty").is_err()); // typo guard
    }
}
