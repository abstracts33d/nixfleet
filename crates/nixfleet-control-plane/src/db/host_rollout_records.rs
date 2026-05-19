//! Persistence for `nixfleet_state_machine::HostRolloutState`.
//!
//! Phase 4 (additive): provides the SQL row <-> in-memory struct mapping
//! and basic CRUD. Phase 6 wires this into the CP runtime's reducer-loop
//! applier; until then the old `host_dispatch_state` + `host_rollout_state`
//! tables continue to serve the old reconciler.
//!
//! The probe map is serialised as a JSON column to avoid a side table; at
//! ~5–20 probes per host the row stays well under any practical SQLite
//! row-size limit, and the probe map is opaque to SQL queries by design.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use nixfleet_proto::OnHealthFailure;
use nixfleet_state_machine::{HostRolloutState, HostState, ProbeRecord};
use rusqlite::{Connection, OptionalExtension, params};

pub struct HostRolloutRecords<'a> {
    pub(super) conn: &'a Mutex<Connection>,
}

impl<'a> HostRolloutRecords<'a> {
    /// Upsert the full record for `(rollout_id, hostname)`. Used by the
    /// reducer applier after every successful `step()`.
    pub fn upsert(&self, state: &HostRolloutState) -> Result<()> {
        let conn = super::lock_conn(self.conn)?;
        upsert_inner(&conn, state)
    }

    /// Load the record for `(rollout_id, hostname)`. Returns `None` when
    /// the record doesn't exist yet (first event for this pair).
    pub fn load(&self, rollout_id: &str, hostname: &str) -> Result<Option<HostRolloutState>> {
        let conn = super::lock_conn(self.conn)?;
        load_inner(&conn, rollout_id, hostname)
    }

    /// All records for a given rollout. Used by the planner to derive
    /// `FleetState.host_states` (RFC-0006 §4.1).
    pub fn all_for_rollout(&self, rollout_id: &str) -> Result<Vec<HostRolloutState>> {
        let conn = super::lock_conn(self.conn)?;
        let mut stmt = conn.prepare(
            "SELECT rollout_id, hostname, channel, state, target_closure,
                    current_closure_at_dispatch, current_closure, reverted_to,
                    dispatched_at, dispatch_acked_at, activation_started_at,
                    activation_completed_at, activation_failed_at,
                    probe_observed_first_at, probe_failure_first_at,
                    soak_due_at, converged_at, failed_at, policy_applied,
                    reverted_at, probes_json, last_event_seq
             FROM host_rollout_records
             WHERE rollout_id = ?1",
        )?;
        let rows = stmt.query_map(params![rollout_id], row_to_state)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Non-terminal-non-reverted records for a hostname across all
    /// rollouts. Used by the heartbeat handler to drive boot-recovery
    /// synthesis (RFC-0005 §9.5): when the agent's wire ack drops
    /// (mid-rollout agent restart, post-rollback `nixfleet-agent`
    /// SIGTERM, CP wipe), the next heartbeat carries `current_closure`
    /// but no `rollout_id` — CP scans these records and matches:
    ///   - In-flight rollout (Pending/Activating/Deferred/Soaking)
    ///     with `target_closure == agent_current` → synthesise
    ///     `RemoteActivationCompleted` (the activation took, ack lost).
    ///   - Failed rollout with `current_closure_at_dispatch ==
    ///     agent_current` → synthesise `RemoteRollbackComplete` (the
    ///     rollback took, ack lost).
    ///
    /// `Failed` is included because the rollback-recovery synth runs
    /// against records whose state-machine transition is still
    /// pending CP-side. `Reverted` and `Converged` are excluded —
    /// those are terminal-on-the-agent and need no recovery synth.
    pub fn active_for_host(&self, hostname: &str) -> Result<Vec<HostRolloutState>> {
        let conn = super::lock_conn(self.conn)?;
        let mut stmt = conn.prepare(
            "SELECT rollout_id, hostname, channel, state, target_closure,
                    current_closure_at_dispatch, current_closure, reverted_to,
                    dispatched_at, dispatch_acked_at, activation_started_at,
                    activation_completed_at, activation_failed_at,
                    probe_observed_first_at, probe_failure_first_at,
                    soak_due_at, converged_at, failed_at, policy_applied,
                    reverted_at, probes_json, last_event_seq
             FROM host_rollout_records
             WHERE hostname = ?1
               AND state IN ('Pending', 'Activating', 'Deferred', 'Soaking', 'Failed')",
        )?;
        let rows = stmt.query_map(params![hostname], row_to_state)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

fn upsert_inner(conn: &Connection, s: &HostRolloutState) -> Result<()> {
    let probes_json =
        serde_json::to_string(&s.probes).context("serialize probes_json for upsert")?;
    let policy_applied_db = s.policy_applied.map(policy_to_db);

    conn.execute(
        "INSERT INTO host_rollout_records (
            rollout_id, hostname, channel, state,
            target_closure, current_closure_at_dispatch, current_closure, reverted_to,
            dispatched_at, dispatch_acked_at, activation_started_at,
            activation_completed_at, activation_failed_at,
            probe_observed_first_at, probe_failure_first_at,
            soak_due_at, converged_at, failed_at, policy_applied,
            reverted_at, probes_json, last_event_seq
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
            ?16, ?17, ?18, ?19, ?20, ?21, ?22
        )
        ON CONFLICT (rollout_id, hostname) DO UPDATE SET
            channel                     = excluded.channel,
            state                       = excluded.state,
            target_closure              = excluded.target_closure,
            current_closure_at_dispatch = excluded.current_closure_at_dispatch,
            current_closure             = excluded.current_closure,
            reverted_to                 = excluded.reverted_to,
            dispatched_at               = excluded.dispatched_at,
            dispatch_acked_at           = excluded.dispatch_acked_at,
            activation_started_at       = excluded.activation_started_at,
            activation_completed_at     = excluded.activation_completed_at,
            activation_failed_at        = excluded.activation_failed_at,
            probe_observed_first_at     = excluded.probe_observed_first_at,
            probe_failure_first_at      = excluded.probe_failure_first_at,
            soak_due_at                 = excluded.soak_due_at,
            converged_at                = excluded.converged_at,
            failed_at                   = excluded.failed_at,
            policy_applied              = excluded.policy_applied,
            reverted_at                 = excluded.reverted_at,
            probes_json                 = excluded.probes_json,
            last_event_seq              = excluded.last_event_seq",
        params![
            s.rollout_id,
            s.hostname,
            s.channel,
            state_to_db(s.state),
            s.target_closure,
            s.current_closure_at_dispatch,
            s.current_closure,
            s.reverted_to,
            s.dispatched_at.to_rfc3339(),
            s.dispatch_acked_at.map(|t: DateTime<Utc>| t.to_rfc3339()),
            s.activation_started_at
                .map(|t: DateTime<Utc>| t.to_rfc3339()),
            s.activation_completed_at
                .map(|t: DateTime<Utc>| t.to_rfc3339()),
            s.activation_failed_at
                .map(|t: DateTime<Utc>| t.to_rfc3339()),
            s.probe_observed_first_at
                .map(|t: DateTime<Utc>| t.to_rfc3339()),
            s.probe_failure_first_at
                .map(|t: DateTime<Utc>| t.to_rfc3339()),
            s.soak_due_at.map(|t: DateTime<Utc>| t.to_rfc3339()),
            s.converged_at.map(|t: DateTime<Utc>| t.to_rfc3339()),
            s.failed_at.map(|t: DateTime<Utc>| t.to_rfc3339()),
            policy_applied_db,
            s.reverted_at.map(|t: DateTime<Utc>| t.to_rfc3339()),
            probes_json,
            s.last_event_seq as i64,
        ],
    )
    .context("upsert host_rollout_records")?;
    Ok(())
}

fn load_inner(
    conn: &Connection,
    rollout_id: &str,
    hostname: &str,
) -> Result<Option<HostRolloutState>> {
    conn.query_row(
        "SELECT rollout_id, hostname, channel, state, target_closure,
                current_closure_at_dispatch, current_closure, reverted_to,
                dispatched_at, dispatch_acked_at, activation_started_at,
                activation_completed_at, activation_failed_at,
                probe_observed_first_at, probe_failure_first_at,
                soak_due_at, converged_at, failed_at, policy_applied,
                reverted_at, probes_json, last_event_seq
         FROM host_rollout_records
         WHERE rollout_id = ?1 AND hostname = ?2",
        params![rollout_id, hostname],
        row_to_state,
    )
    .optional()
    .context("load host_rollout_records")
}

fn row_to_state(row: &rusqlite::Row<'_>) -> rusqlite::Result<HostRolloutState> {
    let probes_json: String = row.get(20)?;
    let probes: HashMap<String, ProbeRecord> = serde_json::from_str(&probes_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(20, rusqlite::types::Type::Text, Box::new(e))
    })?;

    Ok(HostRolloutState {
        rollout_id: row.get(0)?,
        hostname: row.get(1)?,
        channel: row.get(2)?,
        state: state_from_db(&row.get::<_, String>(3)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                format!("unknown state: {e}").into(),
            )
        })?,
        target_closure: row.get(4)?,
        current_closure_at_dispatch: row.get(5)?,
        current_closure: row.get(6)?,
        reverted_to: row.get(7)?,
        dispatched_at: parse_rfc3339_required(row, 8, "dispatched_at")?,
        dispatch_acked_at: parse_rfc3339_optional(row, 9)?,
        activation_started_at: parse_rfc3339_optional(row, 10)?,
        activation_completed_at: parse_rfc3339_optional(row, 11)?,
        activation_failed_at: parse_rfc3339_optional(row, 12)?,
        probe_observed_first_at: parse_rfc3339_optional(row, 13)?,
        probe_failure_first_at: parse_rfc3339_optional(row, 14)?,
        soak_due_at: parse_rfc3339_optional(row, 15)?,
        converged_at: parse_rfc3339_optional(row, 16)?,
        failed_at: parse_rfc3339_optional(row, 17)?,
        policy_applied: row
            .get::<_, Option<String>>(18)?
            .map(|s| {
                policy_from_db(&s).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        18,
                        rusqlite::types::Type::Text,
                        format!("unknown policy_applied: {e}").into(),
                    )
                })
            })
            .transpose()?,
        reverted_at: parse_rfc3339_optional(row, 19)?,
        probes,
        last_event_seq: row.get::<_, i64>(21)? as u64,
    })
}

fn parse_rfc3339_required(
    row: &rusqlite::Row<'_>,
    idx: usize,
    field: &'static str,
) -> rusqlite::Result<DateTime<Utc>> {
    let s: String = row.get(idx)?;
    DateTime::parse_from_rfc3339(&s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                idx,
                rusqlite::types::Type::Text,
                format!("parse {field}: {e}").into(),
            )
        })
}

fn parse_rfc3339_optional(
    row: &rusqlite::Row<'_>,
    idx: usize,
) -> rusqlite::Result<Option<DateTime<Utc>>> {
    row.get::<_, Option<String>>(idx)?
        .map(|s| {
            DateTime::parse_from_rfc3339(&s)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        idx,
                        rusqlite::types::Type::Text,
                        format!("parse rfc3339: {e}").into(),
                    )
                })
        })
        .transpose()
}

fn state_to_db(s: HostState) -> &'static str {
    match s {
        HostState::Pending => "Pending",
        HostState::Activating => "Activating",
        HostState::Deferred => "Deferred",
        HostState::Soaking => "Soaking",
        HostState::Converged => "Converged",
        HostState::Failed => "Failed",
        HostState::Reverted => "Reverted",
    }
}

fn state_from_db(s: &str) -> Result<HostState, String> {
    match s {
        "Pending" => Ok(HostState::Pending),
        "Activating" => Ok(HostState::Activating),
        "Deferred" => Ok(HostState::Deferred),
        "Soaking" => Ok(HostState::Soaking),
        "Converged" => Ok(HostState::Converged),
        "Failed" => Ok(HostState::Failed),
        "Reverted" => Ok(HostState::Reverted),
        other => Err(other.to_string()),
    }
}

fn policy_to_db(p: OnHealthFailure) -> &'static str {
    match p {
        OnHealthFailure::Halt => "halt",
        OnHealthFailure::RollbackAndHalt => "rollback-and-halt",
    }
}

fn policy_from_db(s: &str) -> Result<OnHealthFailure, String> {
    match s {
        "halt" => Ok(OnHealthFailure::Halt),
        "rollback-and-halt" => Ok(OnHealthFailure::RollbackAndHalt),
        other => Err(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use chrono::{Duration, TimeZone};

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 16, 1, 0, 0).unwrap()
    }

    fn fresh_db() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        db
    }

    #[test]
    fn upsert_and_load_round_trip() {
        let db = fresh_db();
        let table = HostRolloutRecords { conn: &db.conn };

        let mut s = HostRolloutState::new_pending(
            "r1".into(),
            "h1".into(),
            "stable".into(),
            "abc123".into(),
            t0(),
            t0() + Duration::minutes(5),
        );
        s.policy_applied = Some(OnHealthFailure::RollbackAndHalt);
        s.last_event_seq = 7;

        table.upsert(&s).unwrap();
        let loaded = table.load("r1", "h1").unwrap().unwrap();
        assert_eq!(loaded.state, HostState::Pending);
        assert_eq!(loaded.target_closure, "abc123");
        assert_eq!(loaded.last_event_seq, 7);
        assert_eq!(
            loaded.policy_applied,
            Some(OnHealthFailure::RollbackAndHalt)
        );
        assert_eq!(loaded.dispatched_at, s.dispatched_at);
        assert_eq!(loaded.soak_due_at, s.soak_due_at);
    }

    #[test]
    fn upsert_overwrites_state_transition() {
        let db = fresh_db();
        let table = HostRolloutRecords { conn: &db.conn };

        let mut s = HostRolloutState::new_pending(
            "r1".into(),
            "h1".into(),
            "stable".into(),
            "abc123".into(),
            t0(),
            t0() + Duration::minutes(5),
        );
        table.upsert(&s).unwrap();

        s.state = HostState::Activating;
        s.dispatch_acked_at = Some(t0() + Duration::seconds(1));
        s.last_event_seq = 1;
        table.upsert(&s).unwrap();

        let loaded = table.load("r1", "h1").unwrap().unwrap();
        assert_eq!(loaded.state, HostState::Activating);
        assert_eq!(loaded.last_event_seq, 1);
    }

    #[test]
    fn load_missing_returns_none() {
        let db = fresh_db();
        let table = HostRolloutRecords { conn: &db.conn };
        let got = table.load("nope", "nope").unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn all_for_rollout_returns_multiple_hosts() {
        let db = fresh_db();
        let table = HostRolloutRecords { conn: &db.conn };
        for host in ["h1", "h2", "h3"] {
            let s = HostRolloutState::new_pending(
                "r1".into(),
                host.into(),
                "stable".into(),
                "abc123".into(),
                t0(),
                t0() + Duration::minutes(5),
            );
            table.upsert(&s).unwrap();
        }
        let got = table.all_for_rollout("r1").unwrap();
        assert_eq!(got.len(), 3);
    }

    /// Pins the recovery-synth contract: `active_for_host` returns
    /// the non-terminal-non-reverted set, which includes `Failed`.
    /// The rollback-recovery synth path
    /// (`maybe_synthesize_recovery_completion`'s Failed arm) iterates
    /// the output of this query, so the filter must include every
    /// state that needs a wire-ack synth — drop `Failed` from here
    /// and the synth becomes silently dead code.
    #[test]
    fn active_for_host_includes_failed_excludes_terminal() {
        let db = fresh_db();
        let table = HostRolloutRecords { conn: &db.conn };

        let states_and_expected = [
            (HostState::Pending, true),
            (HostState::Activating, true),
            (HostState::Deferred, true),
            (HostState::Soaking, true),
            (HostState::Failed, true),
            (HostState::Converged, false),
            (HostState::Reverted, false),
        ];

        for (idx, (state, _)) in states_and_expected.iter().enumerate() {
            let rollout_id = format!("r{idx}");
            let mut s = HostRolloutState::new_pending(
                rollout_id.into(),
                "h1".into(),
                "stable".into(),
                "abc123".into(),
                t0(),
                t0() + Duration::minutes(5),
            );
            s.state = *state;
            if matches!(state, HostState::Soaking | HostState::Converged) {
                s.current_closure = Some("abc123".into());
                s.activation_completed_at = Some(t0() + Duration::seconds(5));
            }
            if matches!(state, HostState::Failed | HostState::Reverted) {
                s.failed_at = Some(t0() + Duration::seconds(125));
                s.current_closure_at_dispatch = Some("prior-closure".into());
            }
            if matches!(state, HostState::Reverted) {
                s.reverted_at = Some(t0() + Duration::seconds(135));
                s.reverted_to = Some("prior-closure".into());
                s.current_closure = Some("prior-closure".into());
            }
            if matches!(state, HostState::Converged) {
                s.converged_at = Some(t0() + Duration::minutes(6));
            }
            table.upsert(&s).unwrap();
        }

        let returned = table.active_for_host("h1").unwrap();
        let returned_states: Vec<HostState> = returned.iter().map(|r| r.state).collect();

        for (state, expected_included) in states_and_expected {
            let included = returned_states.contains(&state);
            assert_eq!(
                included, expected_included,
                "state {state:?}: expected included={expected_included}, got {included}",
            );
        }
    }

    #[test]
    fn check_constraint_rejects_invalid_state() {
        let db = fresh_db();
        let conn = super::super::lock_conn(&db.conn).unwrap();
        // Inserting an old-enum value must fail the CHECK constraint.
        let err = conn
            .execute(
                "INSERT INTO host_rollout_records (
                    rollout_id, hostname, channel, state,
                    target_closure, dispatched_at, probes_json, last_event_seq
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, '{}', 0)",
                params!["r1", "h1", "stable", "Healthy", "abc", t0().to_rfc3339()],
            )
            .unwrap_err();
        let s = format!("{err:?}");
        assert!(s.contains("CHECK"), "expected CHECK violation, got {s}");
    }
}
