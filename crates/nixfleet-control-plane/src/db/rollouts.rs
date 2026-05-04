//! Per-rollout supersession state (soft state; reconstructible after rebuild
//! from channel-refs polling + on-dispatch inserts). Source of truth for
//! "is this rollout still in flight, or has a newer rollout for the same
//! channel replaced it?"

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use std::sync::Mutex;

pub struct Rollouts<'a> {
    pub(super) conn: &'a Mutex<Connection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupersedeStatus {
    pub superseded_at: Option<DateTime<Utc>>,
    pub superseded_by: Option<String>,
}

impl SupersedeStatus {
    pub fn is_superseded(&self) -> bool {
        self.superseded_at.is_some()
    }
}

impl Rollouts<'_> {
    /// Idempotent insert + same-channel supersede in one txn.
    ///
    /// LOADBEARING:
    /// 1. `INSERT OR IGNORE` ensures concurrent dispatches with the same
    ///    `(rollout_id, channel)` don't fight — first writer wins, the rest
    ///    no-op.
    /// 2. The supersede UPDATE has `WHERE rollout_id != ?` so we never mark
    ///    ourselves as superseded.
    /// 3. Channels are namespaces — supersession is strictly intra-channel.
    /// 4. Timestamps are RFC3339 strings to match the convention used by
    ///    the rest of the schema (read paths use `parse::<DateTime<Utc>>()`).
    pub fn record_active_rollout(&self, rollout_id: &str, channel: &str) -> Result<()> {
        let now_rfc = Utc::now().to_rfc3339();
        let mut guard = super::lock_conn(self.conn)?;
        let txn = guard.transaction().context("begin record_active_rollout")?;
        txn.execute(
            "INSERT OR IGNORE INTO rollouts(rollout_id, channel, created_at)
             VALUES (?1, ?2, ?3)",
            params![rollout_id, channel, now_rfc],
        )
        .context("INSERT OR IGNORE rollouts")?;
        txn.execute(
            "UPDATE rollouts
             SET superseded_at = ?3,
                 superseded_by = ?2
             WHERE channel = ?1
               AND rollout_id != ?2
               AND superseded_at IS NULL",
            params![channel, rollout_id, now_rfc],
        )
        .context("UPDATE rollouts supersede prior")?;
        txn.commit().context("commit record_active_rollout")?;
        Ok(())
    }

    /// `Ok(None)` when the rollout isn't tracked. Lifecycle endpoint
    /// returns 404 in that case — callers don't fabricate supersession
    /// state for unknown rids (no historical reconstruction).
    pub fn supersede_status(&self, rollout_id: &str) -> Result<Option<SupersedeStatus>> {
        let guard = super::lock_conn(self.conn)?;
        let row = guard
            .query_row(
                "SELECT superseded_at, superseded_by
                 FROM rollouts
                 WHERE rollout_id = ?1",
                params![rollout_id],
                |row| {
                    let at: Option<String> = row.get(0)?;
                    let by: Option<String> = row.get(1)?;
                    Ok((at, by))
                },
            )
            .optional()
            .context("query rollouts.supersede_status")?;
        let parsed = row
            .map(|(at_raw, by)| -> Result<SupersedeStatus> {
                let superseded_at = match at_raw {
                    Some(s) => Some(
                        s.parse::<DateTime<Utc>>()
                            .with_context(|| format!("parse rollouts.superseded_at: {s}"))?,
                    ),
                    None => None,
                };
                Ok(SupersedeStatus {
                    superseded_at,
                    superseded_by: by,
                })
            })
            .transpose()?;
        Ok(parsed)
    }

    /// Used by `active_rollouts_snapshot` to filter out superseded rollouts
    /// without joining (snapshot is grouped by rollout_id; this returns the
    /// set of superseded ids to exclude).
    pub fn superseded_rollout_ids(&self) -> Result<Vec<String>> {
        let guard = super::lock_conn(self.conn)?;
        let mut stmt =
            guard.prepare("SELECT rollout_id FROM rollouts WHERE superseded_at IS NOT NULL")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use crate::db::Db;

    fn fresh_db() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        db
    }

    #[test]
    fn record_active_rollout_inserts_first_one_as_active() {
        let db = fresh_db();
        db.rollouts()
            .record_active_rollout("r1", "stable")
            .unwrap();
        let status = db.rollouts().supersede_status("r1").unwrap();
        let s = status.expect("rollout present");
        assert!(!s.is_superseded(), "first rollout on a channel must be active");
    }

    #[test]
    fn record_active_rollout_supersedes_prior_on_same_channel() {
        let db = fresh_db();
        db.rollouts()
            .record_active_rollout("r1", "stable")
            .unwrap();
        db.rollouts()
            .record_active_rollout("r2", "stable")
            .unwrap();

        let r1 = db.rollouts().supersede_status("r1").unwrap().unwrap();
        assert!(r1.is_superseded());
        assert_eq!(r1.superseded_by.as_deref(), Some("r2"));

        let r2 = db.rollouts().supersede_status("r2").unwrap().unwrap();
        assert!(!r2.is_superseded());
    }

    #[test]
    fn record_active_rollout_does_not_supersede_across_channels() {
        let db = fresh_db();
        db.rollouts()
            .record_active_rollout("r1", "stable")
            .unwrap();
        db.rollouts()
            .record_active_rollout("r2", "edge-slow")
            .unwrap();

        // Both should be active in their own channel.
        assert!(!db
            .rollouts()
            .supersede_status("r1")
            .unwrap()
            .unwrap()
            .is_superseded());
        assert!(!db
            .rollouts()
            .supersede_status("r2")
            .unwrap()
            .unwrap()
            .is_superseded());
    }

    #[test]
    fn record_active_rollout_is_idempotent_for_same_id_same_channel() {
        let db = fresh_db();
        db.rollouts()
            .record_active_rollout("r1", "stable")
            .unwrap();
        db.rollouts()
            .record_active_rollout("r1", "stable")
            .unwrap();
        // r1 must still be active — re-recording itself never marks it superseded.
        assert!(!db
            .rollouts()
            .supersede_status("r1")
            .unwrap()
            .unwrap()
            .is_superseded());
    }

    #[test]
    fn supersede_status_returns_none_for_unknown_rollout() {
        let db = fresh_db();
        assert!(db.rollouts().supersede_status("ghost").unwrap().is_none());
    }

    #[test]
    fn superseded_rollout_ids_lists_only_superseded() {
        let db = fresh_db();
        db.rollouts()
            .record_active_rollout("r1", "stable")
            .unwrap();
        db.rollouts()
            .record_active_rollout("r2", "stable")
            .unwrap();
        db.rollouts()
            .record_active_rollout("r3", "edge-slow")
            .unwrap();
        let mut ids = db.rollouts().superseded_rollout_ids().unwrap();
        ids.sort();
        assert_eq!(ids, vec!["r1".to_string()]);
    }

    /// LOADBEARING regression: rebuild scenario. After a rebuild the table
    /// starts empty; the polling tick must populate it idempotently for
    /// each channel's current rid. Stale rids that NEVER re-enter the table
    /// stay absent — the lifecycle endpoint returns 404 for them and
    /// render.sh skips, no fabricated supersession state.
    #[test]
    fn rebuild_recovery_repopulates_via_repeated_record_calls() {
        let db = fresh_db();
        db.rollouts()
            .record_active_rollout("r-current", "stable")
            .unwrap();
        db.rollouts()
            .record_active_rollout("r-current", "stable")
            .unwrap();
        let s = db
            .rollouts()
            .supersede_status("r-current")
            .unwrap()
            .expect("current rid present after polling tick");
        assert!(!s.is_superseded());
        assert!(db.rollouts().supersede_status("r-old").unwrap().is_none());
    }
}
