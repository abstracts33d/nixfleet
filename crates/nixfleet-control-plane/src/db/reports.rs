//! `host_reports` — durable per-host event log.
//!
//! Recovery class: **soft state** (ARCHITECTURE.md §6 Phase 10).
//! In-memory ring buffer with persistence; loss is bounded —
//! outstanding `ComplianceFailure` / `RuntimeGateError` events that
//! gated wave promotion clear briefly on a CP restart, and a host
//! that re-runs the gate and finds the same failure re-posts the
//! event. Elevation candidate when probe-output signing extends to
//! non-compliance variants.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::sync::Mutex;

/// `signature_status` is the raw kebab-case string; caller
/// deserialises into `nixfleet_reconciler::evidence::SignatureStatus`.
#[derive(Debug, Clone)]
pub struct HostReportRow {
    pub event_id: String,
    pub received_at: DateTime<Utc>,
    pub event_kind: String,
    pub rollout: Option<String>,
    pub signature_status: Option<String>,
    pub report_json: String,
}

/// Bundled to keep call sites readable (avoids `too_many_arguments`).
#[derive(Debug, Clone)]
pub struct HostReportInsert<'a> {
    pub hostname: &'a str,
    pub event_id: &'a str,
    pub received_at: DateTime<Utc>,
    pub event_kind: &'a str,
    pub rollout: Option<&'a str>,
    pub signature_status: Option<&'a str>,
    pub report_json: &'a str,
}

pub struct Reports<'a> {
    pub(super) conn: &'a Mutex<Connection>,
}

impl Reports<'_> {
    /// Persist an event report. Mirrors the in-memory ring buffer
    /// write in `server::handlers::report` so the event survives CP
    /// restart. `signature_status` is the kebab-case `SignatureStatus`
    /// serde representation (or `None` for events that don't carry
    /// the contract). `report_json` is the canonical JSON envelope of
    /// the wire `ReportRequest`.
    pub fn record_host_report(&self, row: &HostReportInsert<'_>) -> Result<()> {
        let guard = super::lock_conn(self.conn)?;
        guard
            .execute(
                "INSERT INTO host_reports
                   (hostname, event_id, received_at, event_kind,
                    rollout, signature_status, report_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    row.hostname,
                    row.event_id,
                    row.received_at.to_rfc3339(),
                    row.event_kind,
                    row.rollout,
                    row.signature_status,
                    row.report_json
                ],
            )
            .context("insert host_reports")?;
        Ok(())
    }

    /// Hydrate the in-memory ring buffer at CP startup. Returns up
    /// to `limit_per_host` most-recent rows per `hostname`,
    /// chronological order. Used by `server::serve` after migration
    /// completes — the dispatch path consults the ring buffer for
    /// hot-path latency, but durability lives in this table.
    pub fn host_reports_recent_per_host(
        &self,
        hostname: &str,
        limit_per_host: usize,
    ) -> Result<Vec<HostReportRow>> {
        let guard = super::lock_conn(self.conn)?;
        let mut stmt = guard.prepare(
            "SELECT event_id, received_at, event_kind, rollout, signature_status, report_json
             FROM host_reports
             WHERE hostname = ?1
             ORDER BY received_at DESC
             LIMIT ?2",
        )?;
        let rows: rusqlite::Result<Vec<HostReportRow>> = stmt
            .query_map(params![hostname, limit_per_host as i64], |row| {
                let received_str: String = row.get(1)?;
                let received_at = received_str.parse::<DateTime<Utc>>().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
                Ok(HostReportRow {
                    event_id: row.get::<_, String>(0)?,
                    received_at,
                    event_kind: row.get::<_, String>(2)?,
                    rollout: row.get::<_, Option<String>>(3)?,
                    signature_status: row.get::<_, Option<String>>(4)?,
                    report_json: row.get::<_, String>(5)?,
                })
            })?
            .collect();
        let mut rows = rows.context("query host_reports")?;
        // Caller wants chronological (oldest first) for ring-buffer
        // insertion order; DB returns newest first.
        rows.reverse();
        Ok(rows)
    }

    /// List every hostname that has at least one host_reports row.
    /// Used at CP startup to drive the per-host hydration loop.
    pub fn host_reports_known_hostnames(&self) -> Result<Vec<String>> {
        let guard = super::lock_conn(self.conn)?;
        let mut stmt = guard.prepare("SELECT DISTINCT hostname FROM host_reports")?;
        let names: rusqlite::Result<Vec<String>> =
            stmt.query_map([], |row| row.get::<_, String>(0))?.collect();
        names.context("query host_reports hostnames")
    }

    /// Drop host_reports rows older than `max_age_hours`. Mirror of
    /// `Confirms::prune_pending_confirms`; same 7-day retention
    /// default. Wired into `prune_timer.rs`.
    pub fn prune_host_reports(&self, max_age_hours: i64) -> Result<usize> {
        let guard = super::lock_conn(self.conn)?;
        let n = guard
            .execute(
                "DELETE FROM host_reports
                 WHERE received_at < datetime('now', ?1)",
                params![format!("-{max_age_hours} hours")],
            )
            .context("prune host_reports")?;
        Ok(n)
    }

    /// Count outstanding ComplianceFailure / RuntimeGateError events
    /// per `(rollout_id, hostname)`. Used by the reconciler's
    /// wave-staging gate emission. The per-rollout grouping is what
    /// enforces resolution-by-replacement: an event posted against
    /// rollout R₀ contributes to `(R₀, host)` not to `host`-globally,
    /// so once the host moves to R₁ and the reconciler iterates
    /// active rollouts, R₀'s events don't appear under R₁'s key —
    /// correct behaviour.
    ///
    /// Events with `rollout IS NULL` (enrollment errors, trust-root
    /// problems — pre-cert-bound paths) are excluded; those are
    /// not rollout-scoped and don't gate wave promotion.
    ///
    /// `signature_status` filter mirrors the
    /// `nixfleet_reconciler::evidence::SignatureStatus::counts_for_gate`
    /// rule: `mismatch` and `malformed` are forged FAIL events from
    /// a compromised mTLS cert and don't count; everything else
    /// (verified, unsigned, no-pubkey, wrong-algorithm, NULL) does.
    ///
    /// Returns a nested map keyed first by rollout id, then by
    /// hostname → count. Empty inner maps are absent (rollouts with
    /// zero outstanding events don't appear at all).
    pub fn outstanding_compliance_events_by_rollout(
        &self,
    ) -> Result<HashMap<String, HashMap<String, usize>>> {
        let guard = super::lock_conn(self.conn)?;
        let mut stmt = guard.prepare(
            "SELECT rollout, hostname, COUNT(*) FROM host_reports
             WHERE rollout IS NOT NULL
               AND event_kind IN ('compliance-failure', 'runtime-gate-error')
               AND COALESCE(signature_status, '') NOT IN ('mismatch', 'malformed')
             GROUP BY rollout, hostname",
        )?;
        let mut out: HashMap<String, HashMap<String, usize>> = HashMap::new();
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)? as usize,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("query outstanding_compliance_events_by_rollout")?;
        for (rollout, host, n) in rows {
            out.entry(rollout).or_default().insert(host, n);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::{fail_event, fresh_db};
    use super::HostReportInsert;
    use chrono::Utc;

    #[test]
    fn host_reports_round_trip_preserves_envelope() {
        let db = fresh_db();
        let row = HostReportInsert {
            hostname: "lab",
            event_id: "evt-rt-1",
            received_at: Utc::now(),
            event_kind: "compliance-failure",
            rollout: Some("edge-slow@abc"),
            signature_status: Some("verified"),
            report_json: r#"{"hostname":"lab","agentVersion":"0.2.0"}"#,
        };
        db.reports().record_host_report(&row).unwrap();
        let mut got = db.reports().host_reports_recent_per_host("lab", 8).unwrap();
        assert_eq!(got.len(), 1);
        let r = got.pop().unwrap();
        assert_eq!(r.event_id, "evt-rt-1");
        assert_eq!(r.event_kind, "compliance-failure");
        assert_eq!(r.rollout.as_deref(), Some("edge-slow@abc"));
        assert_eq!(r.signature_status.as_deref(), Some("verified"));
    }

    #[test]
    fn outstanding_events_by_rollout_filters_tampered() {
        // Verified + unsigned + no-pubkey count toward the gate;
        // mismatch + malformed do NOT (defends against forged FAIL
        // events from a compromised mTLS cert).
        let db = fresh_db();
        for (eid, sig) in [
            ("e1", Some("verified")),
            ("e2", Some("unsigned")),
            ("e3", Some("no-pubkey")),
            ("e4", Some("mismatch")),
            ("e5", Some("malformed")),
        ] {
            let mut row = fail_event(Some("R1"), sig);
            row.event_id = eid;
            db.reports().record_host_report(&row).unwrap();
        }
        let by_rollout = db
            .reports()
            .outstanding_compliance_events_by_rollout()
            .unwrap();
        // verified + unsigned + no-pubkey = 3; mismatch + malformed
        // are filtered out.
        assert_eq!(
            by_rollout.get("R1").and_then(|m| m.get("lab")).copied(),
            Some(3),
        );
    }

    #[test]
    fn outstanding_events_by_rollout_groups_per_rollout() {
        // Resolution-by-replacement test: events for R0 stay under R0,
        // events for R1 stay under R1. The reconciler iterates active
        // rollouts and looks up its own ID's outstanding events; an
        // R0-bound event must NOT contaminate R1's count.
        let db = fresh_db();
        let mut e0 = fail_event(Some("R0"), Some("verified"));
        e0.event_id = "evt-r0-1";
        db.reports().record_host_report(&e0).unwrap();
        let mut e1 = fail_event(Some("R1"), Some("verified"));
        e1.event_id = "evt-r1-1";
        db.reports().record_host_report(&e1).unwrap();
        let by_rollout = db
            .reports()
            .outstanding_compliance_events_by_rollout()
            .unwrap();
        assert_eq!(
            by_rollout.get("R0").and_then(|m| m.get("lab")).copied(),
            Some(1),
        );
        assert_eq!(
            by_rollout.get("R1").and_then(|m| m.get("lab")).copied(),
            Some(1),
        );
    }

    #[test]
    fn outstanding_events_by_rollout_excludes_null_rollout() {
        // Events with rollout=NULL (enrollment, trust-root errors)
        // are not rollout-scoped and don't appear in the projection.
        let db = fresh_db();
        let mut row = fail_event(None, Some("verified"));
        row.event_id = "evt-orphan";
        db.reports().record_host_report(&row).unwrap();
        let by_rollout = db
            .reports()
            .outstanding_compliance_events_by_rollout()
            .unwrap();
        assert!(
            by_rollout.is_empty(),
            "rollout=NULL events should not appear: {:?}",
            by_rollout,
        );
    }

    #[test]
    fn prune_host_reports_drops_old_rows() {
        let db = fresh_db();
        // Insert with a past received_at so the prune sweep matches.
        let past = Utc::now() - chrono::Duration::hours(48);
        let row = HostReportInsert {
            hostname: "lab",
            event_id: "evt-old",
            received_at: past,
            event_kind: "compliance-failure",
            rollout: None,
            signature_status: None,
            report_json: "{}",
        };
        db.reports().record_host_report(&row).unwrap();
        // 24h retention drops the past row.
        let n = db.reports().prune_host_reports(24).unwrap();
        assert_eq!(n, 1);
        let names = db.reports().host_reports_known_hostnames().unwrap();
        assert!(names.is_empty(), "old row should be pruned");
    }
}
