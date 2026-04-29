//! Typed state-machine enums for CP persistence rows.
//!
//! Mirrors the proto-side `compliance::GateMode` pattern: a single
//! Rust enum, with `as_db_str` / `from_db_str` accessors for the
//! SQLite boundary. The SQL CHECK constraints + column types stay
//! `TEXT`; the canonical literals are emitted from this module so
//! every call site reads the same source of truth.
//!
//! Keeping the enum in a sibling module to `db.rs` avoids pulling
//! a domain-specific type into the proto crate (it's CP-private —
//! the agent never sees `pending_confirms.state` strings) while
//! still letting `db.rs`, `rollback_timer.rs`, and any future SQL
//! call sites share the same compile-time-checked variant names.

use anyhow::{anyhow, Result};

/// RFC-0003 §4.2 activation lifecycle. Persisted as TEXT in
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
}
