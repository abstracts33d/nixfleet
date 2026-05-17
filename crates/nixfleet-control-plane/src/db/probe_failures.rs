//! Per-(rollout, host) typed denormalization of enforce-mode probe
//! failures. Source of truth is `event_log` (canonical, append-only);
//! this table is a derived projection providing the indexed columns
//! the compliance-wave gate needs cheaply.
//!
//! RFC-0010 §7.2 shape:
//! - `event_log_seq`  — FK back to the source `event_log` row
//! - `rollout_id`     — gate aggregates per rollout
//! - `host_id`        — gate aggregates per host
//! - `probe_name`     — operator-facing probe identifier
//! - `control_id`     — set for evidence-kind sub-result rows; NULL for
//!   non-evidence enforce-mode probe failures
//! - `framework`      — set for evidence sub-result rows; NULL otherwise
//! - `observed_at`    — agent-supplied observation timestamp
//!
//! Indexed on `(rollout_id, host_id, control_id)` so the compliance-wave
//! gate's distinct-control count query is cheap.
//!
//! ## Phase 9a state (stub)
//!
//! The schema lands in this commit. The writer side (single-transaction
//! co-write from the applier's `RemoteAppendEventLog` handler on
//! enforce-mode `ProbeResult { status = Fail }` events) lands in 9b.
//! Until then the table is unwritten and every projection returns the
//! empty map — `outstanding_failing_enforce_probes_by_rollout` exposes
//! the SHAPE that 9b will fill with data.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use std::collections::HashMap;
use std::sync::Mutex;

pub struct ProbeFailures<'a> {
    pub(super) conn: &'a Mutex<Connection>,
}

/// One `probe_failures` row insert. Borrowed for ergonomics — the
/// applier's hot path passes references off the inbound payload.
#[derive(Debug, Clone)]
pub struct ProbeFailureInsert<'a> {
    pub rollout_id: &'a str,
    pub host_id: &'a str,
    pub probe_name: &'a str,
    pub control_id: Option<&'a str>,
    pub framework: Option<&'a str>,
    pub observed_at: DateTime<Utc>,
}

impl ProbeFailures<'_> {
    /// Insert a batch of probe_failures rows. The applier calls this
    /// from the `RemoteAppendEventLog` handler on enforce-mode
    /// `ProbeResult { status = Fail }`; for evidence probes it walks
    /// `sub_results` and inserts one row per failing control.
    ///
    /// 9b shape: one transaction wraps the whole batch. `event_log_seq`
    /// is left NULL — the FK column in the schema permits NULL; a
    /// follow-up tightening lands when the event_log_writer task can
    /// hand back the row's `seq` synchronously (today's writer is
    /// fire-and-forget on a bounded channel).
    pub fn insert_many(&self, rows: &[ProbeFailureInsert<'_>]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        super::txn(self.conn, "probe_failures.insert_many", |tx| {
            let mut stmt = tx.prepare(
                "INSERT INTO probe_failures
                   (event_log_seq, rollout_id, host_id, probe_name,
                    control_id, framework, observed_at)
                 VALUES (NULL, ?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for r in rows {
                stmt.execute(params![
                    r.rollout_id,
                    r.host_id,
                    r.probe_name,
                    r.control_id,
                    r.framework,
                    r.observed_at.to_rfc3339(),
                ])
                .context("insert probe_failures row")?;
            }
            Ok(())
        })
    }

    /// Per-(rollout, host) distinct-control failure counts. Gate's
    /// canonical input projection — empty in 9a, populated by 9b's
    /// applier co-write.
    pub fn outstanding_failing_enforce_probes_by_rollout(
        &self,
    ) -> Result<HashMap<nixfleet_proto::RolloutId, HashMap<String, usize>>> {
        super::read(self.conn, |c| {
            let mut stmt = c.prepare(
                "SELECT rollout_id, host_id,
                        COUNT(DISTINCT COALESCE(control_id, probe_name))
                 FROM probe_failures
                 GROUP BY rollout_id, host_id",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, nixfleet_proto::RolloutId>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)? as usize,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("query outstanding_failing_enforce_probes_by_rollout")?;
            let mut out: HashMap<nixfleet_proto::RolloutId, HashMap<String, usize>> =
                HashMap::new();
            for (rollout, host, n) in rows {
                out.entry(rollout).or_default().insert(host, n);
            }
            Ok(out)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::Db;
    use super::ProbeFailureInsert;
    use chrono::Utc;

    /// Projection returns empty before any writer call (9a baseline).
    #[test]
    fn empty_when_table_unwritten() {
        let db = Db::open_in_memory().expect("open in-memory db");
        db.migrate().expect("apply migrations");
        let got = db
            .probe_failures()
            .outstanding_failing_enforce_probes_by_rollout()
            .expect("query");
        assert!(got.is_empty(), "baseline: projection must be empty");
    }

    /// Evidence probe with 3 distinct failing controls on (R1, h1) →
    /// projection returns 3. Distinct counted on control_id.
    #[test]
    fn insert_many_and_project_counts_distinct_controls() {
        let db = Db::open_in_memory().expect("open in-memory db");
        db.migrate().expect("apply migrations");
        let now = Utc::now();
        let rows: Vec<ProbeFailureInsert> = ["c1", "c2", "c3"]
            .into_iter()
            .map(|c| ProbeFailureInsert {
                rollout_id: "R1",
                host_id: "h1",
                probe_name: "evidence-nis2",
                control_id: Some(c),
                framework: Some("nis2-essential"),
                observed_at: now,
            })
            .collect();
        db.probe_failures().insert_many(&rows).expect("insert");
        let got = db
            .probe_failures()
            .outstanding_failing_enforce_probes_by_rollout()
            .expect("query");
        assert_eq!(got.get("R1").and_then(|m| m.get("h1")).copied(), Some(3));
    }

    /// Non-evidence enforce-fail probe (control_id=NULL) → projection
    /// counts it as 1, keyed by probe_name fallback.
    #[test]
    fn insert_many_non_evidence_uses_probe_name_fallback() {
        let db = Db::open_in_memory().expect("open in-memory db");
        db.migrate().expect("apply migrations");
        let row = ProbeFailureInsert {
            rollout_id: "R2",
            host_id: "h1",
            probe_name: "heartbeat",
            control_id: None,
            framework: None,
            observed_at: Utc::now(),
        };
        db.probe_failures().insert_many(&[row]).expect("insert");
        let got = db
            .probe_failures()
            .outstanding_failing_enforce_probes_by_rollout()
            .expect("query");
        assert_eq!(got.get("R2").and_then(|m| m.get("h1")).copied(), Some(1));
    }

    /// Distinct-counting collapses duplicate inserts on the same
    /// control_id (the projection uses COUNT(DISTINCT ...)).
    #[test]
    fn insert_many_duplicate_control_ids_collapse() {
        let db = Db::open_in_memory().expect("open in-memory db");
        db.migrate().expect("apply migrations");
        let now = Utc::now();
        let rows: Vec<ProbeFailureInsert> = (0..5)
            .map(|_| ProbeFailureInsert {
                rollout_id: "R1",
                host_id: "h1",
                probe_name: "evidence-nis2",
                control_id: Some("same-control"),
                framework: Some("nis2-essential"),
                observed_at: now,
            })
            .collect();
        db.probe_failures().insert_many(&rows).expect("insert");
        let got = db
            .probe_failures()
            .outstanding_failing_enforce_probes_by_rollout()
            .expect("query");
        assert_eq!(
            got.get("R1").and_then(|m| m.get("h1")).copied(),
            Some(1),
            "DISTINCT control_id collapses 5 duplicates to 1",
        );
    }
}
