//! Pending Dispatch payloads awaiting agent long-poll (RFC-0008 §4.1 +
//! plan 06).
//!
//! The runtime applier UPSERTs into this table on every
//! `PlanAction::QueueDispatch` / `Effect::RemoteQueueDispatch`. The
//! `GET /v1/agent/dispatch` long-poll handler `take_for_host()`-s a single
//! row, deleting it atomically; failing to deliver doesn't lose the
//! intent because the reducer will re-emit on the next plan tick (the
//! planner skips hosts with `dispatch_acked_at` set, so re-emission only
//! happens when the prior Dispatch was never acked).

use std::sync::Mutex;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};

pub struct DispatchQueue<'a> {
    pub(super) conn: &'a Mutex<Connection>,
}

#[derive(Debug, Clone)]
pub struct QueuedDispatch {
    pub hostname: String,
    pub rollout_id: nixfleet_proto::RolloutId,
    pub target_closure: String,
    pub soak_due_at: DateTime<Utc>,
    pub enqueued_at: DateTime<Utc>,
}

impl<'a> DispatchQueue<'a> {
    /// Upsert a pending Dispatch for `(hostname, rollout_id)`. Idempotent:
    /// re-emission of `QueueDispatch` for the same pair overwrites
    /// `target_closure` / `soak_due_at` (the reducer's view always wins).
    pub fn upsert(&self, q: &QueuedDispatch) -> Result<()> {
        let conn = super::lock_conn(self.conn)?;
        conn.execute(
            "INSERT INTO dispatch_queue
                (hostname, rollout_id, target_closure, soak_due_at, enqueued_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (hostname, rollout_id) DO UPDATE SET
                target_closure = excluded.target_closure,
                soak_due_at    = excluded.soak_due_at,
                enqueued_at    = excluded.enqueued_at",
            params![
                q.hostname,
                q.rollout_id,
                q.target_closure,
                q.soak_due_at.to_rfc3339(),
                q.enqueued_at.to_rfc3339(),
            ],
        )
        .context("upsert dispatch_queue")?;
        Ok(())
    }

    /// Atomically claim and delete the next queued Dispatch for `hostname`.
    /// Returns `Ok(None)` if no dispatch is pending.
    ///
    /// Atomicity: SELECT + DELETE in one txn under the connection mutex.
    /// SQLite's WAL mode + the single-writer Mutex make this race-free
    /// even with multiple long-poll handlers (no two see the same row).
    pub fn take_for_host(&self, hostname: &str) -> Result<Option<QueuedDispatch>> {
        super::txn(self.conn, "dispatch_queue.take_for_host", |tx| {
            let row: Option<QueuedDispatch> = tx
                .query_row(
                    "SELECT hostname, rollout_id, target_closure,
                            soak_due_at, enqueued_at
                     FROM dispatch_queue
                     WHERE hostname = ?1
                     ORDER BY enqueued_at ASC
                     LIMIT 1",
                    params![hostname],
                    row_to_queued,
                )
                .optional()
                .context("select dispatch_queue row")?;
            if let Some(ref q) = row {
                tx.execute(
                    "DELETE FROM dispatch_queue
                     WHERE hostname = ?1 AND rollout_id = ?2",
                    params![q.hostname, q.rollout_id],
                )
                .context("delete dispatch_queue row")?;
            }
            Ok(row)
        })
    }

    /// Peek without claiming. Used by the long-poll worker after a wake
    /// event to decide whether to actually fire `take_for_host` (avoids a
    /// txn on every wake when the queue is empty for this host).
    pub fn peek_for_host(&self, hostname: &str) -> Result<bool> {
        let conn = super::lock_conn(self.conn)?;
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dispatch_queue WHERE hostname = ?1",
                params![hostname],
                |r| r.get(0),
            )
            .context("count dispatch_queue rows for host")?;
        Ok(n > 0)
    }
}

fn row_to_queued(row: &rusqlite::Row<'_>) -> rusqlite::Result<QueuedDispatch> {
    let soak: String = row.get(3)?;
    let enq: String = row.get(4)?;
    Ok(QueuedDispatch {
        hostname: row.get(0)?,
        rollout_id: row.get(1)?,
        target_closure: row.get(2)?,
        soak_due_at: parse_rfc3339(&soak, "soak_due_at")?,
        enqueued_at: parse_rfc3339(&enq, "enqueued_at")?,
    })
}

fn parse_rfc3339(s: &str, field: &'static str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("parse {field}: {e}").into(),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use chrono::TimeZone;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 16, 1, 0, 0).unwrap()
    }

    fn fresh_db() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        db
    }

    fn sample(host: &str, rollout: &str, closure: &str) -> QueuedDispatch {
        QueuedDispatch {
            hostname: host.into(),
            rollout_id: rollout.into(),
            target_closure: closure.into(),
            soak_due_at: t0() + chrono::Duration::minutes(5),
            enqueued_at: t0(),
        }
    }

    #[test]
    fn upsert_then_take_round_trip() {
        let db = fresh_db();
        let q = DispatchQueue { conn: &db.conn };
        q.upsert(&sample("h1", "r1", "abc")).unwrap();
        let taken = q.take_for_host("h1").unwrap().unwrap();
        assert_eq!(taken.target_closure, "abc");
        // Second take returns None — row was deleted.
        assert!(q.take_for_host("h1").unwrap().is_none());
    }

    #[test]
    fn upsert_overwrites_existing_pair() {
        let db = fresh_db();
        let q = DispatchQueue { conn: &db.conn };
        q.upsert(&sample("h1", "r1", "abc")).unwrap();
        q.upsert(&sample("h1", "r1", "def")).unwrap();
        let taken = q.take_for_host("h1").unwrap().unwrap();
        assert_eq!(taken.target_closure, "def");
        assert!(q.take_for_host("h1").unwrap().is_none());
    }

    #[test]
    fn take_returns_oldest_first() {
        let db = fresh_db();
        let q = DispatchQueue { conn: &db.conn };
        let mut older = sample("h1", "r1", "first");
        older.enqueued_at = t0();
        let mut newer = sample("h1", "r2", "second");
        newer.enqueued_at = t0() + chrono::Duration::seconds(10);
        q.upsert(&older).unwrap();
        q.upsert(&newer).unwrap();
        let first = q.take_for_host("h1").unwrap().unwrap();
        assert_eq!(first.target_closure, "first");
        let second = q.take_for_host("h1").unwrap().unwrap();
        assert_eq!(second.target_closure, "second");
    }

    #[test]
    fn peek_reports_pending() {
        let db = fresh_db();
        let q = DispatchQueue { conn: &db.conn };
        assert!(!q.peek_for_host("h1").unwrap());
        q.upsert(&sample("h1", "r1", "abc")).unwrap();
        assert!(q.peek_for_host("h1").unwrap());
    }

    #[test]
    fn take_for_other_host_does_not_affect_this_one() {
        let db = fresh_db();
        let q = DispatchQueue { conn: &db.conn };
        q.upsert(&sample("h1", "r1", "abc")).unwrap();
        q.upsert(&sample("h2", "r1", "xyz")).unwrap();
        let taken = q.take_for_host("h2").unwrap().unwrap();
        assert_eq!(taken.hostname, "h2");
        // h1's row still there.
        assert!(q.peek_for_host("h1").unwrap());
    }
}
