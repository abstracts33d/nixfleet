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
//! the agent never sees `host_dispatch_state.state` strings) while
//! still letting `db.rs`, `rollback_timer.rs`, and any future SQL
//! call sites share the same compile-time-checked variant names.

use anyhow::{anyhow, Result};

/// Per-host activation lifecycle. Persisted as TEXT in
/// `host_dispatch_state.state` with a CHECK constraint over the
/// canonical literals returned by [`PendingConfirmState::as_db_str`].
///
/// Lifecycle:
/// - [`Pending`](Self::Pending): row created by the dispatch loop;
///   the agent has been told to activate but has not yet confirmed.
/// - [`Confirmed`](Self::Confirmed): the agent posted
///   `/v1/agent/confirm` (or the orphan-recovery path inserted a
///   synthetic row directly in this state).
/// - [`RolledBack`](Self::RolledBack): the magic-rollback timer
///   tripped — `confirm_deadline` passed without confirmation —
///   or the report handler closed the rollback-and-halt loop.
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
    /// the V006 migration's `host_dispatch_state.state` column.
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
            other => Err(anyhow!("unknown host_dispatch_state.state: {other:?}")),
        }
    }
}

/// Terminal classification stamped on `dispatch_history.terminal_state`
/// when a host's dispatch reaches the end of its lifecycle. Distinct
/// from [`PendingConfirmState`]: operational state has both pre-
/// terminal (Pending/Confirmed) and terminal (RolledBack/Cancelled)
/// values, while audit only records what terminal a dispatch
/// eventually reached. Confirmed rows that the reconciler later
/// converges land here as `Converged`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TerminalState {
    Converged,
    RolledBack,
    Cancelled,
}

impl TerminalState {
    /// Canonical SQLite literal. Matches the CHECK constraint in
    /// the V006 migration's `dispatch_history.terminal_state`.
    pub fn as_db_str(&self) -> &'static str {
        match self {
            TerminalState::Converged => "converged",
            TerminalState::RolledBack => "rolled-back",
            TerminalState::Cancelled => "cancelled",
        }
    }
}

/// Per-host rollout state machine. Re-exported from
/// [`nixfleet_proto`] so the CP and the reconciler share a single
/// source of truth for the variant set + literals; the SQL
/// CHECK constraint in V003 is bridged to this enum by the
/// `host_rollout_state_check_matches_enum` test in
/// `observed_projection.rs`.
pub use nixfleet_proto::HostRolloutState;

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
    fn terminal_state_literals_match_check_constraint() {
        // Bridge to the V006 migration's CHECK constraint on
        // dispatch_history.terminal_state. Hard-coded so a future
        // rename catches the drift.
        assert_eq!(TerminalState::Converged.as_db_str(), "converged");
        assert_eq!(TerminalState::RolledBack.as_db_str(), "rolled-back");
        assert_eq!(TerminalState::Cancelled.as_db_str(), "cancelled");
    }

    // HostRolloutState round-trip + unknown-string tests live with
    // the canonical enum in `nixfleet-proto`. The SQL/enum bridge
    // (`host_rollout_state_check_matches_enum`) stays here in CP
    // since it parses the local `migrations/V003__*.sql` file.
}
