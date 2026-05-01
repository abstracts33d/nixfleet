//! `pending_confirms` — activation confirmations + magic-rollback
//! timer support.
//!
//! Recovery class: **soft state** (ARCHITECTURE.md §6 Phase 10).
//! Loss could force the agent into an unnecessary local rollback when
//! its confirm POST hits a 410. Mitigated by orphan-confirm recovery
//! (#46): when the agent's reported `closure_hash` matches the
//! verified target, the handler synthesises a confirmed row via
//! [`Confirms::record_confirmed_pending`] and returns 204.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::sync::Mutex;

use crate::state::PendingConfirmState;

/// `(id, hostname, rollout_id, wave, target_closure_hash)`. Aliased
/// to keep the signature readable and silence `type_complexity`.
pub type ExpiredPendingConfirm = (i64, String, String, u32, String);

/// Bundled args for [`Confirms::record_pending_confirm`]. Mirrors the
/// [`super::reports::HostReportInsert`] precedent — both `rollout_id`
/// and `target_channel_ref` are `&str` literals shaped like
/// `"stable@abc12345"`, easy to swap positionally; the named struct
/// makes that class of bug a compile error at the call site.
#[derive(Debug, Clone)]
pub struct PendingConfirmInsert<'a> {
    pub hostname: &'a str,
    pub rollout_id: &'a str,
    /// Channel name the rollout was opened on. Persisted explicitly
    /// since #62 made rolloutIds content hashes that no longer encode
    /// the channel. Must be non-empty at insert time.
    pub channel: &'a str,
    pub wave: u32,
    pub target_closure_hash: &'a str,
    pub target_channel_ref: &'a str,
    pub confirm_deadline: DateTime<Utc>,
}

pub struct Confirms<'a> {
    pub(super) conn: &'a Mutex<Connection>,
}

impl Confirms<'_> {
    /// Record a dispatched activation. Called from the dispatch loop
    /// when CP populates `target` in a checkin response. The agent
    /// will later post `/v1/agent/confirm` with the same `rollout_id`
    /// once it boots the new closure.
    pub fn record_pending_confirm(&self, row: &PendingConfirmInsert<'_>) -> Result<i64> {
        let guard = super::lock_conn(self.conn)?;
        guard
            .execute(
                "INSERT INTO pending_confirms(hostname, rollout_id, channel, wave,
                                              target_closure_hash,
                                              target_channel_ref,
                                              confirm_deadline)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    row.hostname,
                    row.rollout_id,
                    row.channel,
                    row.wave,
                    row.target_closure_hash,
                    row.target_channel_ref,
                    row.confirm_deadline.to_rfc3339()
                ],
            )
            .context("insert pending_confirms")?;
        Ok(guard.last_insert_rowid())
    }

    /// Insert a `pending_confirms` row directly in `'confirmed'`
    /// state — used by the orphan-confirm recovery path when
    /// an agent posts `/v1/agent/confirm` but no matching `pending`
    /// row exists (typically because the CP was rebuilt mid-flight).
    /// The orphan handler verifies the agent's `closure_hash` matches
    /// the host's declared target before calling this; the synthetic
    /// row preserves the audit trail of "this host activated this
    /// closure" without forcing a spurious rollback. `confirm_deadline`
    /// is set to `confirmed_at` since the deadline is moot for an
    /// already-confirmed row.
    #[allow(clippy::too_many_arguments)] // 1:1 row shape; bundling adds churn (see PendingConfirmInsert).
    pub fn record_confirmed_pending(
        &self,
        hostname: &str,
        rollout_id: &str,
        channel: &str,
        wave: u32,
        target_closure_hash: &str,
        target_channel_ref: &str,
        confirmed_at: DateTime<Utc>,
    ) -> Result<i64> {
        let guard = super::lock_conn(self.conn)?;
        let ts = confirmed_at.to_rfc3339();
        guard
            .execute(
                "INSERT INTO pending_confirms(hostname, rollout_id, channel, wave,
                                              target_closure_hash,
                                              target_channel_ref,
                                              confirm_deadline,
                                              confirmed_at,
                                              state)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    hostname,
                    rollout_id,
                    channel,
                    wave,
                    target_closure_hash,
                    target_channel_ref,
                    &ts,
                    &ts,
                    PendingConfirmState::Confirmed.as_db_str(),
                ],
            )
            .context("insert pending_confirms (orphan recovery)")?;
        Ok(guard.last_insert_rowid())
    }

    /// Returns true if the host has any `pending_confirms` row in
    /// state `'pending'`. Used by the dispatch loop to avoid
    /// re-dispatching while an activation is in flight (would create
    /// a duplicate row racing the first).
    pub fn pending_confirm_exists(&self, hostname: &str) -> Result<bool> {
        let guard = super::lock_conn(self.conn)?;
        let n: i64 = guard
            .query_row(
                "SELECT COUNT(*) FROM pending_confirms
                 WHERE hostname = ?1 AND state = ?2",
                params![hostname, PendingConfirmState::Pending.as_db_str()],
                |row| row.get(0),
            )
            .context("count pending_confirms")?;
        Ok(n > 0)
    }

    /// Mark a pending confirmation as confirmed. Called by the
    /// `/v1/agent/confirm` handler. Returns the number of rows
    /// updated — 0 means no matching pending row (could be: rollout
    /// cancelled, deadline already expired, or agent confirming
    /// twice). Caller decides on the response code.
    pub fn confirm_pending(&self, hostname: &str, rollout_id: &str) -> Result<usize> {
        let guard = super::lock_conn(self.conn)?;
        let n = guard
            .execute(
                "UPDATE pending_confirms
                 SET confirmed_at = datetime('now'),
                     state = ?3
                 WHERE hostname = ?1
                   AND rollout_id = ?2
                   AND state = ?4",
                params![
                    hostname,
                    rollout_id,
                    PendingConfirmState::Confirmed.as_db_str(),
                    PendingConfirmState::Pending.as_db_str(),
                ],
            )
            .context("update pending_confirms confirmed")?;
        Ok(n)
    }

    /// Pending confirms whose deadline has passed and which haven't
    /// been confirmed yet. Used by the magic-rollback timer task —
    /// each row returned is a host that failed to confirm in time
    /// and should be rolled back.
    ///
    /// Returns (id, hostname, rollout_id, wave, target_closure_hash).
    ///
    /// Wraps `confirm_deadline` in `datetime(...)` so SQLite parses the
    /// stored RFC3339 string (`YYYY-MM-DDTHH:MM:SS+00:00`, written by
    /// `chrono::DateTime::to_rfc3339`) into the same canonical
    /// `YYYY-MM-DD HH:MM:SS` shape that `datetime('now')` returns,
    /// before the `<` comparison. Naked string compare would put `T`
    /// (0x54) above ` ` (0x20) at position 10, so deadlines would
    /// always look greater than now — expired rows never matched and
    /// the rollback timer was a no-op. Caught on lab when a deadline
    /// passed by 50s while the row was still `pending`.
    pub fn pending_confirms_expired(&self) -> Result<Vec<ExpiredPendingConfirm>> {
        let guard = super::lock_conn(self.conn)?;
        let mut stmt = guard.prepare(
            "SELECT id, hostname, rollout_id, wave, target_closure_hash
             FROM pending_confirms
             WHERE state = ?1
               AND datetime(confirm_deadline) < datetime('now')",
        )?;
        let rows = stmt
            .query_map(params![PendingConfirmState::Pending.as_db_str()], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, u32>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Mark expired confirms as rolled-back. Called by the magic-
    /// rollback timer after `pending_confirms_expired` for the same
    /// IDs. Idempotent — only updates rows still in 'pending' state,
    /// so a second call with the same IDs is a no-op.
    pub fn mark_rolled_back(&self, ids: &[i64]) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let guard = super::lock_conn(self.conn)?;
        // SQLite IN clause via repeated `?` placeholders. The state
        // literals come from the typed enum so a future variant rename
        // can't drift between this UPDATE and the rest of db.
        let placeholders = (1..=ids.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let new_state_idx = ids.len() + 1;
        let pending_idx = ids.len() + 2;
        let sql = format!(
            "UPDATE pending_confirms
             SET state = ?{new_state_idx}
             WHERE state = ?{pending_idx} AND id IN ({placeholders})"
        );
        let mut stmt = guard.prepare(&sql)?;
        let mut bound: Vec<&dyn rusqlite::ToSql> =
            ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
        let rolled_back = PendingConfirmState::RolledBack.as_db_str();
        let pending = PendingConfirmState::Pending.as_db_str();
        bound.push(&rolled_back);
        bound.push(&pending);
        let n = stmt.execute(rusqlite::params_from_iter(bound.iter()))?;
        Ok(n)
    }

    /// Prune terminal `pending_confirms` rows older than `max_age`.
    /// Mirror of `Tokens::prune_token_replay`. Rows in terminal states
    /// `RolledBack` / `Cancelled` carry no load-bearing semantics —
    /// they accumulate one row per dispatch + churn cycle and bloat
    /// the table indefinitely without retention. Lab observed 39 such
    /// rows from 3 days of deploy thrash. Default retention 7 days
    /// (caller chooses). Returns number of pruned rows.
    pub fn prune_pending_confirms(&self, max_age_hours: i64) -> Result<usize> {
        let rolled_back = PendingConfirmState::RolledBack.as_db_str();
        let cancelled = PendingConfirmState::Cancelled.as_db_str();
        let guard = super::lock_conn(self.conn)?;
        let n = guard
            .execute(
                "DELETE FROM pending_confirms
                 WHERE state IN (?1, ?2)
                   AND dispatched_at < datetime('now', ?3)",
                params![rolled_back, cancelled, format!("-{max_age_hours} hours")],
            )
            .context("prune pending_confirms")?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::{fresh_db, mark_healthy, pc_insert};
    use chrono::Utc;

    #[test]
    fn pending_confirms_expired_matches_past_deadline() {
        // Regression: chrono's `to_rfc3339` writes deadlines as
        // `YYYY-MM-DDTHH:MM:SS+00:00` (T separator, +offset), but
        // SQLite's `datetime('now')` returns `YYYY-MM-DD HH:MM:SS`
        // (space separator, no offset). A naked string compare ranks
        // 'T' (0x54) above ' ' (0x20), so deadlines look greater than
        // now forever and `pending_confirms_expired` returns nothing
        // — the rollback timer was a no-op. The query wraps the
        // column in `datetime(...)` to normalise. This test fires on
        // a row whose deadline is firmly in the past.
        let db = fresh_db();
        let past_deadline = Utc::now() - chrono::Duration::seconds(60);
        db.confirms()
            .record_pending_confirm(&pc_insert(
                "test-host",
                "stable@abc",
                "decl-system",
                past_deadline,
            ))
            .unwrap();

        let expired = db.confirms().pending_confirms_expired().unwrap();
        assert_eq!(
            expired.len(),
            1,
            "row past deadline should be picked up, got {expired:?}",
        );
        let (_, host, rollout, _, target) = &expired[0];
        assert_eq!(host, "test-host");
        assert_eq!(rollout, "stable@abc");
        assert_eq!(target, "decl-system");
    }

    #[test]
    fn pending_confirms_expired_skips_future_deadline() {
        // Companion to the regression test above: rows whose deadline
        // is in the future stay out of the expired set.
        let db = fresh_db();
        let future_deadline = Utc::now() + chrono::Duration::seconds(120);
        db.confirms()
            .record_pending_confirm(&pc_insert(
                "test-host",
                "stable@def",
                "decl-system",
                future_deadline,
            ))
            .unwrap();
        let expired = db.confirms().pending_confirms_expired().unwrap();
        assert!(
            expired.is_empty(),
            "row in window should not expire: {expired:?}"
        );
    }

    #[test]
    fn record_confirmed_pending_writes_confirmed_state() {
        // Orphan-confirm recovery path. Synthetic row must land in
        // 'confirmed' state with confirmed_at populated and be picked
        // up by active_rollouts_snapshot just like a row that went
        // through the normal pending → confirmed flow.
        let db = fresh_db();
        let now = Utc::now();
        db.confirms()
            .record_confirmed_pending(
                "test-host",
                "stable@orphan",
                "stable",
                0,
                "target-system",
                "stable@orphan",
                now,
            )
            .unwrap();
        // The host is not yet recorded as Healthy — the handler does
        // that as a separate step. So the snapshot's host_states maps
        // to the defensive "Healthy" fallback for confirmed rows
        // without an hrs row.
        let snap = db.rollout_state().active_rollouts_snapshot().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].rollout_id, "stable@orphan");
        assert_eq!(snap[0].target_closure_hash, "target-system");
        assert_eq!(
            snap[0].host_states.get("test-host").map(String::as_str),
            Some("Healthy"),
        );
        // healthy_rollouts_for_host requires both the confirmed row
        // and the Healthy marker to surface — exercising the join.
        // Pre-Healthy: empty.
        assert!(db
            .rollout_state()
            .healthy_rollouts_for_host("test-host")
            .unwrap()
            .is_empty());
        mark_healthy(&db, "test-host", "stable@orphan", now);
        let healthy = db
            .rollout_state()
            .healthy_rollouts_for_host("test-host")
            .unwrap();
        assert_eq!(healthy.len(), 1);
        assert_eq!(healthy[0].0, "stable@orphan");
        assert_eq!(healthy[0].1, "target-system");
    }
}
