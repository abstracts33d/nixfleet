//! Quarantined-closures derived view (RFC-0012 §6.4). Append-only: one
//! row per `RollbackComplete` event (RFC-0008 §4.2). The applier is the
//! sole writer; the `triggering_event_log_seq` FK proves the table is
//! re-derivable from `event_log` (walk RollbackComplete events, group by
//! `(channel, target_closure_hash)`, write one row per group).
//!
//! Trusted-input only: rows are written by the applier on
//! `Effect::RemoteInsertQuarantine`. Agent-emitted `ClosureQuarantined`
//! reports are NOT inserted here (they are unsigned and would let a
//! compromised host DoS the fleet by quarantining arbitrary SHAs).
//!
//! `triggering_event_log_seq` is NULL-able under the v0.2.1 baseline
//! (RFC-0012 §6.1 item 3 + v0.2.1-followups #1).
//!
//! Append-only under the v0.2 derived-view discipline: no `clear`,
//! no `cleared_at`. Operator-driven clearance would land as an
//! explicit event matching the `OperatorClearance` shape (RFC-0012 §4).

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

pub struct QuarantinedClosures<'a> {
    pub(super) conn: &'a Mutex<Connection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantineRow {
    pub channel: String,
    pub closure_hash: String,
    pub quarantined_at: DateTime<Utc>,
    pub triggering_event_log_seq: Option<i64>,
}

impl<'a> QuarantinedClosures<'a> {
    /// Idempotent under `(channel, closure_hash)` PK (ON CONFLICT DO
    /// NOTHING — quarantines are append-only; re-quarantining the same
    /// closure is a no-op rather than a re-stamp).
    pub fn insert(
        &self,
        channel: &str,
        closure_hash: &str,
        quarantined_at: DateTime<Utc>,
        triggering_event_log_seq: Option<i64>,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("poisoned");
        conn.execute(
            "INSERT INTO quarantined_closures(
                 channel, closure_hash, quarantined_at, triggering_event_log_seq)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(channel, closure_hash) DO NOTHING",
            params![
                channel,
                closure_hash,
                quarantined_at.to_rfc3339(),
                triggering_event_log_seq
            ],
        )
        .context("insert quarantine")?;
        Ok(())
    }

    /// Active set keyed by channel -> {closure_hash}. The gate reads this
    /// on every plan tick via `Observed.quarantined_closures` to refuse
    /// dispatch of a known-bad SHA.
    pub fn active_by_channel(&self) -> Result<HashMap<String, HashSet<String>>> {
        let conn = self.conn.lock().expect("poisoned");
        let mut stmt = conn
            .prepare("SELECT channel, closure_hash FROM quarantined_closures")
            .context("prepare active_by_channel")?;
        let mut rows = stmt.query([]).context("query active_by_channel")?;
        let mut out: HashMap<String, HashSet<String>> = HashMap::new();
        while let Some(row) = rows.next().context("step active_by_channel")? {
            let channel: String = row.get(0)?;
            let closure_hash: String = row.get(1)?;
            out.entry(channel).or_default().insert(closure_hash);
        }
        Ok(out)
    }

    /// Operator-surface listing (CLI `nixfleet quarantine list`).
    pub fn list_active(&self) -> Result<Vec<QuarantineRow>> {
        let conn = self.conn.lock().expect("poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT channel, closure_hash, quarantined_at, triggering_event_log_seq
                 FROM quarantined_closures
                 ORDER BY quarantined_at DESC",
            )
            .context("prepare list_active")?;
        let mut rows = stmt.query([]).context("query list_active")?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().context("step list_active")? {
            let channel: String = row.get(0)?;
            let closure_hash: String = row.get(1)?;
            let qat: String = row.get(2)?;
            let trig: Option<i64> = row.get(3)?;
            out.push(QuarantineRow {
                channel,
                closure_hash,
                quarantined_at: qat
                    .parse::<DateTime<Utc>>()
                    .with_context(|| format!("parse quarantined_closures.quarantined_at: {qat}"))?,
                triggering_event_log_seq: trig,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn fresh_db() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        db
    }

    fn t0() -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2026, 5, 16, 1, 0, 0).unwrap()
    }

    #[test]
    fn insert_and_list_active() {
        let db = fresh_db();
        let q = db.quarantined_closures();
        q.insert("stable", "abc123", t0(), None).unwrap();
        let rows = q.list_active().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].channel, "stable");
        assert_eq!(rows[0].closure_hash, "abc123");
        assert!(rows[0].triggering_event_log_seq.is_none());
    }

    #[test]
    fn idempotent_insert_is_no_op_after_conflict() {
        let db = fresh_db();
        let q = db.quarantined_closures();
        // FK is NULL-able under v0.2.1 baseline (RFC-0012 §6.1 item 3);
        // None is the legal "FK not yet known" marker.
        q.insert("stable", "abc", t0(), None).unwrap();
        // Second insert at a later timestamp is a no-op (append-only).
        q.insert("stable", "abc", t0() + chrono::Duration::seconds(1), None)
            .unwrap();
        let rows = q.list_active().unwrap();
        assert_eq!(rows.len(), 1);
        // First insert wins (ON CONFLICT DO NOTHING).
        assert_eq!(rows[0].quarantined_at, t0());
    }

    #[test]
    fn active_by_channel_groups_correctly() {
        let db = fresh_db();
        let q = db.quarantined_closures();
        q.insert("stable", "abc", t0(), None).unwrap();
        q.insert("stable", "def", t0(), None).unwrap();
        q.insert("edge", "ghi", t0(), None).unwrap();
        let by_chan = q.active_by_channel().unwrap();
        assert_eq!(by_chan["stable"].len(), 2);
        assert_eq!(by_chan["edge"].len(), 1);
    }
}
