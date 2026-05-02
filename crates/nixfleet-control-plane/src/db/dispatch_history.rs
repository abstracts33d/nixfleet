//! `dispatch_history` — append-only audit of every dispatch.
//!
//! Recovery class: **soft state** (ARCHITECTURE.md §6 Phase 10).
//! The audit log is for forensics, not the control loop. Loss
//! removes a debugging surface (which rollouts has host X been
//! through?) but does not affect convergence or safety: the
//! operational row in [`super::host_dispatch_state`] is the single
//! source of truth for "what is host X doing right now". Pruned by
//! retention window (default 90d).
//!
//! Split out of `pending_confirms` in V006 (#81); see [`super::host_dispatch_state`]
//! for the operational half.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::sync::Mutex;

use crate::state::TerminalState;

use super::host_dispatch_state::DispatchInsert;

/// One audit row.
#[derive(Debug, Clone)]
pub struct DispatchHistoryRow {
    pub id: i64,
    pub hostname: String,
    pub rollout_id: String,
    pub channel: String,
    pub wave: u32,
    pub target_closure_hash: String,
    pub target_channel_ref: String,
    pub dispatched_at: String,
    pub terminal_state: Option<String>,
    pub terminal_at: Option<String>,
}

pub struct DispatchHistory<'a> {
    pub(super) conn: &'a Mutex<Connection>,
}

impl DispatchHistory<'_> {
    /// Stamp the most-recent open audit row for (rollout, host) with
    /// a terminal state. Idempotent — if no open row exists (already
    /// stamped, or never dispatched in this rollout), returns 0.
    pub fn mark_terminal_for_rollout_host(
        &self,
        rollout_id: &str,
        hostname: &str,
        terminal: TerminalState,
        at: DateTime<Utc>,
    ) -> Result<usize> {
        let guard = super::lock_conn(self.conn)?;
        let n = guard
            .execute(
                "UPDATE dispatch_history
                 SET terminal_state = ?1, terminal_at = ?2
                 WHERE id = (
                     SELECT id FROM dispatch_history
                     WHERE rollout_id = ?3 AND hostname = ?4
                       AND terminal_state IS NULL
                     ORDER BY dispatched_at DESC, id DESC
                     LIMIT 1
                 )",
                params![
                    terminal.as_db_str(),
                    at.to_rfc3339(),
                    rollout_id,
                    hostname,
                ],
            )
            .context("mark_terminal_for_rollout_host")?;
        Ok(n)
    }

    /// Stamp every open history row of a converged rollout with
    /// `terminal_state = 'converged'`. Replaces (post-#81) the
    /// `delete_rollout_records` cleanup that the
    /// [`Action::ConvergeRollout`](nixfleet_reconciler::Action) arm
    /// used to call against `pending_confirms` — the operational
    /// table no longer needs cleanup since converged hosts stay
    /// parked on Confirmed rows that the next dispatch overwrites.
    pub fn mark_rollout_converged(
        &self,
        rollout_id: &str,
        at: DateTime<Utc>,
    ) -> Result<usize> {
        let guard = super::lock_conn(self.conn)?;
        let n = guard
            .execute(
                "UPDATE dispatch_history
                 SET terminal_state = ?1, terminal_at = ?2
                 WHERE rollout_id = ?3 AND terminal_state IS NULL",
                params![TerminalState::Converged.as_db_str(), at.to_rfc3339(), rollout_id],
            )
            .context("mark_rollout_converged")?;
        Ok(n)
    }

    /// Drop history rows older than `max_age_hours` whose
    /// `terminal_state` is set. Open rows are never pruned (they
    /// reflect dispatches still in flight). Mirror of the
    /// `prune_token_replay` and `prune_host_reports` shape.
    pub fn prune_history(&self, max_age_hours: i64) -> Result<usize> {
        let guard = super::lock_conn(self.conn)?;
        let n = guard
            .execute(
                "DELETE FROM dispatch_history
                 WHERE terminal_state IS NOT NULL
                   AND datetime(terminal_at) < datetime('now', ?1)",
                params![format!("-{max_age_hours} hours")],
            )
            .context("prune dispatch_history")?;
        Ok(n)
    }

    /// Most-recent `limit` audit rows for a host, newest first.
    /// Surfaces the per-host dispatch history for the future
    /// `nixfleet status` CLI (#66). Internal contract: callers may
    /// rely on dispatched_at DESC ordering.
    pub fn recent_for_host(
        &self,
        hostname: &str,
        limit: usize,
    ) -> Result<Vec<DispatchHistoryRow>> {
        let guard = super::lock_conn(self.conn)?;
        let mut stmt = guard.prepare(
            "SELECT id, hostname, rollout_id, channel, wave,
                    target_closure_hash, target_channel_ref,
                    dispatched_at, terminal_state, terminal_at
             FROM dispatch_history
             WHERE hostname = ?1
             ORDER BY dispatched_at DESC, id DESC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![hostname, limit as i64], row_to_history_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

/// Append a history row inside an existing connection / transaction.
/// `pub(super)` so [`super::host_dispatch_state::HostDispatchState::record_dispatch`]
/// can call us from inside its own transaction guarding the
/// operational + audit pair.
pub(super) fn insert_history(conn: &Connection, row: &DispatchInsert<'_>) -> Result<i64> {
    conn.execute(
        "INSERT INTO dispatch_history(
             hostname, rollout_id, channel, wave,
             target_closure_hash, target_channel_ref
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            row.hostname,
            row.rollout_id,
            row.channel,
            row.wave,
            row.target_closure_hash,
            row.target_channel_ref,
        ],
    )
    .context("insert dispatch_history")?;
    Ok(conn.last_insert_rowid())
}

fn row_to_history_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DispatchHistoryRow> {
    Ok(DispatchHistoryRow {
        id: row.get(0)?,
        hostname: row.get(1)?,
        rollout_id: row.get(2)?,
        channel: row.get(3)?,
        wave: row.get(4)?,
        target_closure_hash: row.get(5)?,
        target_channel_ref: row.get(6)?,
        dispatched_at: row.get(7)?,
        terminal_state: row.get(8)?,
        terminal_at: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::{dispatch_insert, fresh_db};
    use crate::state::TerminalState;
    use chrono::Utc;

    #[test]
    fn append_only_grows_on_each_dispatch() {
        // Each record_dispatch (operational write) appends a new
        // history row. Two dispatches → two rows, regardless of
        // whether the operational row was upserted.
        let db = fresh_db();
        let deadline = Utc::now() + chrono::Duration::seconds(120);
        for rollout in ["stable@r1", "stable@r2", "stable@r3"] {
            db.host_dispatch_state()
                .record_dispatch(&dispatch_insert("ohm", rollout, "system", deadline))
                .unwrap();
        }
        let history = db.dispatch_history().recent_for_host("ohm", 10).unwrap();
        assert_eq!(history.len(), 3);
        // recent_for_host returns newest-first.
        assert_eq!(history[0].rollout_id, "stable@r3");
        assert_eq!(history[2].rollout_id, "stable@r1");
    }

    #[test]
    fn mark_terminal_for_rollout_host_idempotent() {
        let db = fresh_db();
        let deadline = Utc::now() + chrono::Duration::seconds(120);
        db.host_dispatch_state()
            .record_dispatch(&dispatch_insert("ohm", "stable@r1", "system", deadline))
            .unwrap();
        let now = Utc::now();
        let n = db
            .dispatch_history()
            .mark_terminal_for_rollout_host("stable@r1", "ohm", TerminalState::RolledBack, now)
            .unwrap();
        assert_eq!(n, 1);
        // Second call: row is already terminal, no open row to find.
        let n = db
            .dispatch_history()
            .mark_terminal_for_rollout_host("stable@r1", "ohm", TerminalState::RolledBack, now)
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn mark_rollout_converged_stamps_all_open_rows() {
        // Two hosts, same rollout → both audit rows flip to
        // converged in one shot. Replaces the pre-#81
        // delete_rollout_records cleanup.
        let db = fresh_db();
        let deadline = Utc::now() + chrono::Duration::seconds(120);
        for host in ["ohm", "krach"] {
            db.host_dispatch_state()
                .record_dispatch(&dispatch_insert(host, "stable@r1", "system", deadline))
                .unwrap();
        }
        let n = db
            .dispatch_history()
            .mark_rollout_converged("stable@r1", Utc::now())
            .unwrap();
        assert_eq!(n, 2);
        for host in ["ohm", "krach"] {
            let rows = db.dispatch_history().recent_for_host(host, 10).unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].terminal_state.as_deref(), Some("converged"));
            assert!(rows[0].terminal_at.is_some());
        }
    }

    #[test]
    fn mark_rollout_converged_skips_terminal_rows() {
        // A host already RolledBack on this rollout (e.g. wave-
        // earlier failure that triggered cleanup) must NOT have its
        // terminal_state overwritten by a subsequent ConvergeRollout
        // for the same rollout.
        let db = fresh_db();
        let deadline = Utc::now() + chrono::Duration::seconds(120);
        db.host_dispatch_state()
            .record_dispatch(&dispatch_insert("krach", "stable@r1", "system", deadline))
            .unwrap();
        db.dispatch_history()
            .mark_terminal_for_rollout_host(
                "stable@r1",
                "krach",
                TerminalState::RolledBack,
                Utc::now(),
            )
            .unwrap();
        db.host_dispatch_state()
            .record_dispatch(&dispatch_insert("ohm", "stable@r1", "system", deadline))
            .unwrap();
        let n = db
            .dispatch_history()
            .mark_rollout_converged("stable@r1", Utc::now())
            .unwrap();
        assert_eq!(n, 1, "only ohm's open row flips; krach already terminal");
        let krach = db.dispatch_history().recent_for_host("krach", 1).unwrap();
        assert_eq!(krach[0].terminal_state.as_deref(), Some("rolled-back"));
    }

    #[test]
    fn prune_history_drops_old_terminal_rows_only() {
        let db = fresh_db();
        let deadline = Utc::now() + chrono::Duration::seconds(120);
        // Old terminal row.
        db.host_dispatch_state()
            .record_dispatch(&dispatch_insert("ohm", "stable@old", "system", deadline))
            .unwrap();
        let old_terminal_at = Utc::now() - chrono::Duration::days(200);
        db.dispatch_history()
            .mark_terminal_for_rollout_host(
                "stable@old",
                "ohm",
                TerminalState::RolledBack,
                old_terminal_at,
            )
            .unwrap();
        // Recent open row: stays.
        db.host_dispatch_state()
            .record_dispatch(&dispatch_insert("krach", "stable@live", "system", deadline))
            .unwrap();
        // Recent terminal row: stays (within 90d retention).
        db.host_dispatch_state()
            .record_dispatch(&dispatch_insert("pixel", "stable@recent", "system", deadline))
            .unwrap();
        db.dispatch_history()
            .mark_terminal_for_rollout_host(
                "stable@recent",
                "pixel",
                TerminalState::Converged,
                Utc::now(),
            )
            .unwrap();

        let pruned = db.dispatch_history().prune_history(24 * 90).unwrap();
        assert_eq!(pruned, 1);
        assert!(db
            .dispatch_history()
            .recent_for_host("ohm", 10)
            .unwrap()
            .is_empty());
        assert_eq!(
            db.dispatch_history().recent_for_host("krach", 10).unwrap().len(),
            1,
            "open row must not be pruned",
        );
        assert_eq!(
            db.dispatch_history().recent_for_host("pixel", 10).unwrap().len(),
            1,
            "fresh terminal row must not be pruned",
        );
    }
}
