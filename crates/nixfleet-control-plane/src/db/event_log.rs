//! Append-only canonical event log (RFC-0008 §4.3 + the broader log
//! pattern: PlanActions, Effects, gate decisions, verifications, manifest
//! polls all land here too).
//!
//! ## Relationship to `probe_failures`
//!
//! `event_log` is the **sole canonical store** — every PlanAction,
//! Effect, GateDecision, and inbound agent event lands here exactly
//! once. Indexed by `seq`, `(host_id, ts)`, `(rollout_id, seq)`,
//! `(kind, ts)` for chronological / per-rollout / per-host /
//! per-kind queries.
//!
//! `probe_failures` (RFC-0010 §7.2) is a **derived view** carrying the
//! typed denormalization the compliance-wave gate needs cheaply
//! (`probe_name`, `control_id`, `framework`, `observed_at` indexed on
//! `(rollout_id, host_id, control_id)`). Single writer: the applier's
//! `RemoteAppendEventLog` handler, on enforce-mode
//! `ProbeResult { status = Fail }`, writes the event_log row AND the
//! per-sub_result probe_failures rows in one transaction. Each
//! probe_failures row carries an `event_log_seq` FK back to its
//! source event_log entry — the table is provably re-derivable from
//! canonical state and can be rebuilt by walking event_log from the
//! beginning.
//!
//! Phase 9a deleted the prior `host_reports` denormalization (its
//! query patterns + dedup invariant moved to probe_failures + the
//! event_log writer respectively).

use std::sync::Mutex;

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};

pub struct EventLog<'a> {
    pub(super) conn: &'a Mutex<Connection>,
}

/// What kind of log entry. Disambiguates the JSON `payload` shape and
/// drives the operator-API filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventLogKind {
    /// Inbound POST to `/v1/agent/events` (RFC-0008 §4.2 outbound).
    AgentEvent,
    /// Output of CP's `plan_next()` (RFC-0009 §4.1).
    PlanAction,
    /// `Effect` emitted by either side's reducer (RFC-0009 §9).
    Effect,
    /// One per gate evaluation (channel-edges, disruption-budget, ...).
    GateDecision,
    /// Signature-verification outcome (manifest, revocations,
    /// bootstrap-nonces).
    VerifyOutcome,
    /// `channel_refs` poll outcome.
    ManifestPoll,
    /// CP-internal rollout-level state transition (RFC-0012 §4). Synthesized
    /// by the applier from per-host events; written to event_log alongside
    /// the `rollouts` derived-view update (RFC-0012 §6.3 + §7).
    RolloutEvent,
}

impl EventLogKind {
    pub fn as_db_str(&self) -> &'static str {
        match self {
            EventLogKind::AgentEvent => "agent_event",
            EventLogKind::PlanAction => "plan_action",
            EventLogKind::Effect => "effect",
            EventLogKind::GateDecision => "gate_decision",
            EventLogKind::VerifyOutcome => "verify_outcome",
            EventLogKind::ManifestPoll => "manifest_poll",
            EventLogKind::RolloutEvent => "rollout_event",
        }
    }
}

#[derive(Debug, Clone)]
pub struct EventLogEntry {
    pub kind: EventLogKind,
    /// Caller-supplied timestamp. Must come from a `ClockHandle`, not
    /// `Utc::now()` ad-hoc — the table has no SQL DEFAULT so test
    /// fixtures using `FakeClock` cannot accidentally produce rows
    /// timestamped with wallclock-now while the reducer's `now` is
    /// frozen elsewhere.
    pub ts: DateTime<Utc>,
    pub host_id: Option<String>,
    pub rollout_id: Option<String>,
    /// JSON-encoded payload. Producer-side responsibility to canonicalise
    /// per RFC-0003 §3 if cross-version-stable hashing matters; the table
    /// itself just stores opaque text. `append()` validates that this
    /// parses as JSON before inserting — a malformed row would poison the
    /// replay tool permanently and silently.
    pub payload: String,
}

#[derive(Debug, Clone)]
pub struct EventLogRow {
    pub seq: i64,
    pub ts: DateTime<Utc>,
    pub kind: String,
    pub host_id: Option<String>,
    pub rollout_id: Option<String>,
    pub payload: String,
}

impl<'a> EventLog<'a> {
    /// Append a single entry. Returns the assigned `seq`. Validates the
    /// `payload` parses as JSON before insert (a malformed row would
    /// silently poison the replay tool — see `EventLogEntry::payload`
    /// docstring).
    pub fn append(&self, entry: &EventLogEntry) -> Result<i64> {
        serde_json::from_str::<serde_json::Value>(&entry.payload)
            .map_err(|e| anyhow!("event_log payload is not valid JSON: {e}"))?;
        let conn = super::lock_conn(self.conn)?;
        conn.execute(
            "INSERT INTO event_log (ts, kind, host_id, rollout_id, payload)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                entry.ts.to_rfc3339(),
                entry.kind.as_db_str(),
                entry.host_id,
                entry.rollout_id,
                entry.payload,
            ],
        )
        .context("append event_log")?;
        Ok(conn.last_insert_rowid())
    }

    /// Latest seq in the log. Useful for "Replay-From" handshake in
    /// RFC-0008 §4.3 and as a sanity check.
    pub fn last_seq(&self) -> Result<i64> {
        let conn = super::lock_conn(self.conn)?;
        let n: Option<i64> = conn
            .query_row("SELECT MAX(seq) FROM event_log", [], |r| r.get(0))
            .context("last_seq event_log")?;
        Ok(n.unwrap_or(0))
    }

    /// Entries for a host, ordered by seq ascending. Used by the
    /// operator API and the replay tool.
    pub fn query_by_host(&self, host_id: &str, limit: i64) -> Result<Vec<EventLogRow>> {
        let conn = super::lock_conn(self.conn)?;
        let mut stmt = conn.prepare(
            "SELECT seq, ts, kind, host_id, rollout_id, payload
             FROM event_log
             WHERE host_id = ?1
             ORDER BY seq ASC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![host_id, limit], row_to_entry)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Entries for a rollout, ordered by seq ascending.
    pub fn query_by_rollout(&self, rollout_id: &str, limit: i64) -> Result<Vec<EventLogRow>> {
        let conn = super::lock_conn(self.conn)?;
        let mut stmt = conn.prepare(
            "SELECT seq, ts, kind, host_id, rollout_id, payload
             FROM event_log
             WHERE rollout_id = ?1
             ORDER BY seq ASC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![rollout_id, limit], row_to_entry)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Entries of a particular kind, ordered by seq ascending.
    pub fn query_by_kind(&self, kind: EventLogKind, limit: i64) -> Result<Vec<EventLogRow>> {
        let conn = super::lock_conn(self.conn)?;
        let mut stmt = conn.prepare(
            "SELECT seq, ts, kind, host_id, rollout_id, payload
             FROM event_log
             WHERE kind = ?1
             ORDER BY seq ASC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![kind.as_db_str(), limit], row_to_entry)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventLogRow> {
    let ts_str: String = row.get(1)?;
    let ts = DateTime::parse_from_rfc3339(&ts_str)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Text,
                format!("parse ts: {e}").into(),
            )
        })?;
    Ok(EventLogRow {
        seq: row.get(0)?,
        ts,
        kind: row.get(2)?,
        host_id: row.get(3)?,
        rollout_id: row.get(4)?,
        payload: row.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use chrono::TimeZone;

    fn fresh_db() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        db
    }

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 16, 1, 0, 0).unwrap()
    }

    #[test]
    fn append_assigns_monotonic_seqs() {
        let db = fresh_db();
        let log = EventLog { conn: &db.conn };
        let entry = EventLogEntry {
            kind: EventLogKind::AgentEvent,
            ts: t0(),
            host_id: Some("h1".into()),
            rollout_id: Some("r1".into()),
            payload: r#"{"kind":"DispatchAck"}"#.into(),
        };
        let s1 = log.append(&entry).unwrap();
        let s2 = log.append(&entry).unwrap();
        let s3 = log.append(&entry).unwrap();
        assert!(s2 > s1);
        assert!(s3 > s2);
        assert_eq!(log.last_seq().unwrap(), s3);
    }

    #[test]
    fn query_by_host_returns_only_matching_host() {
        let db = fresh_db();
        let log = EventLog { conn: &db.conn };
        log.append(&EventLogEntry {
            kind: EventLogKind::AgentEvent,
            ts: t0(),
            host_id: Some("h1".into()),
            rollout_id: Some("r1".into()),
            payload: r#"{"k":"a"}"#.into(),
        })
        .unwrap();
        log.append(&EventLogEntry {
            kind: EventLogKind::AgentEvent,
            ts: t0(),
            host_id: Some("h2".into()),
            rollout_id: Some("r1".into()),
            payload: r#"{"k":"b"}"#.into(),
        })
        .unwrap();
        log.append(&EventLogEntry {
            kind: EventLogKind::AgentEvent,
            ts: t0(),
            host_id: Some("h1".into()),
            rollout_id: Some("r1".into()),
            payload: r#"{"k":"c"}"#.into(),
        })
        .unwrap();

        let got = log.query_by_host("h1", 100).unwrap();
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|r| r.host_id.as_deref() == Some("h1")));
    }

    #[test]
    fn query_by_kind_filters_correctly() {
        let db = fresh_db();
        let log = EventLog { conn: &db.conn };
        log.append(&EventLogEntry {
            kind: EventLogKind::AgentEvent,
            ts: t0(),
            host_id: Some("h1".into()),
            rollout_id: None,
            payload: r#"{}"#.into(),
        })
        .unwrap();
        log.append(&EventLogEntry {
            kind: EventLogKind::PlanAction,
            ts: t0(),
            host_id: None,
            rollout_id: Some("r1".into()),
            payload: r#"{}"#.into(),
        })
        .unwrap();
        let agent_only = log.query_by_kind(EventLogKind::AgentEvent, 100).unwrap();
        assert_eq!(agent_only.len(), 1);
        assert_eq!(agent_only[0].kind, "agent_event");
    }

    #[test]
    fn entries_with_null_host_and_rollout_round_trip() {
        // CP-side plan actions and manifest polls don't pin to a host.
        let db = fresh_db();
        let log = EventLog { conn: &db.conn };
        let seq = log
            .append(&EventLogEntry {
                kind: EventLogKind::ManifestPoll,
                ts: t0(),
                host_id: None,
                rollout_id: None,
                payload: r#"{"channel":"stable","outcome":"304"}"#.into(),
            })
            .unwrap();
        assert!(seq > 0);
        let got = log.query_by_kind(EventLogKind::ManifestPoll, 10).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].host_id, None);
        assert_eq!(got[0].rollout_id, None);
    }

    #[test]
    fn malformed_payload_rejected_at_append() {
        let db = fresh_db();
        let log = EventLog { conn: &db.conn };
        let err = log
            .append(&EventLogEntry {
                kind: EventLogKind::AgentEvent,
                ts: t0(),
                host_id: Some("h1".into()),
                rollout_id: None,
                payload: "not-json{".into(),
            })
            .unwrap_err();
        let s = format!("{err}");
        assert!(
            s.contains("not valid JSON"),
            "expected JSON validation error, got {s}"
        );
        // Table must remain empty.
        assert_eq!(log.last_seq().unwrap(), 0);
    }

    #[test]
    fn caller_supplied_ts_round_trips() {
        let db = fresh_db();
        let log = EventLog { conn: &db.conn };
        let supplied = t0() + chrono::Duration::seconds(42);
        log.append(&EventLogEntry {
            kind: EventLogKind::AgentEvent,
            ts: supplied,
            host_id: Some("h1".into()),
            rollout_id: None,
            payload: r#"{}"#.into(),
        })
        .unwrap();
        let got = log.query_by_host("h1", 10).unwrap();
        assert_eq!(got[0].ts, supplied);
    }
}
