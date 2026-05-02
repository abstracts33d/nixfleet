//! `host_dispatch_state` — operational dispatch row, one per host.
//!
//! Recovery class: **soft state** (ARCHITECTURE.md §6 Phase 10).
//! Loss could force the agent into an unnecessary local rollback when
//! its confirm POST hits a 410. Mitigated by orphan-confirm recovery:
//! when the agent's reported `closure_hash` matches the verified
//! target, the handler synthesises a confirmed row via
//! [`HostDispatchState::record_confirmed_dispatch`] and returns 204.
//!
//! Paired with [`super::dispatch_history`]: this module owns the live
//! one-row-per-host operational state + the `active_rollouts_snapshot`
//! projection; dispatch_history is the append-only audit log.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::sync::Mutex;

use crate::state::{HostRolloutState, PendingConfirmState, TerminalState};

/// Bundled args for [`HostDispatchState::record_dispatch`] and the
/// audit insert in [`super::dispatch_history::DispatchHistory::record_dispatch`].
/// Both tables share the same row shape at insertion time, so a single
/// struct keeps call sites readable; the named fields make positional
/// `&str` swaps a compile error.
#[derive(Debug, Clone)]
pub struct DispatchInsert<'a> {
    pub hostname: &'a str,
    pub rollout_id: &'a str,
    /// Channel name the rollout was opened on. Persisted explicitly
    /// since #62 made rolloutIds content hashes that no longer encode
    /// the channel.
    pub channel: &'a str,
    pub wave: u32,
    pub target_closure_hash: &'a str,
    pub target_channel_ref: &'a str,
    pub confirm_deadline: DateTime<Utc>,
}

/// Joined snapshot of `host_dispatch_state` + `host_rollout_state` for
/// the observed-state projection. Rollouts are derived (no dedicated
/// table); operational rows in terminal states are filtered out so
/// dead rollouts don't surface as empty-host-states the reconciler
/// would re-dispatch.
#[derive(Debug, Clone)]
pub struct RolloutDbSnapshot {
    pub rollout_id: String,
    pub channel: String,
    pub target_closure_hash: String,
    pub target_channel_ref: String,
    /// `host_rollout_state` wins when present; otherwise derived
    /// from the operational `host_dispatch_state.state`.
    pub host_states: HashMap<String, String>,
    /// Excludes hosts whose marker is NULL (not currently Healthy).
    pub last_healthy_since: HashMap<String, DateTime<Utc>>,
}

/// `(hostname, rollout_id, wave, target_closure_hash)`. Rows whose
/// `confirm_deadline` has passed and which haven't been confirmed.
pub type ExpiredDispatch = (String, String, u32, String);

pub struct HostDispatchState<'a> {
    pub(super) conn: &'a Mutex<Connection>,
}

impl HostDispatchState<'_> {
    /// Record a dispatched activation. Called from the dispatch loop
    /// when CP populates `target` in a checkin response. Writes the
    /// operational row (UPSERT — one row per host, replaced on every
    /// new dispatch) AND appends an audit row to `dispatch_history`,
    /// both inside a single transaction. The agent will later post
    /// `/v1/agent/confirm` with the same `rollout_id` once it boots.
    pub fn record_dispatch(&self, row: &DispatchInsert<'_>) -> Result<()> {
        let mut guard = super::lock_conn(self.conn)?;
        let txn = guard.transaction().context("begin dispatch txn")?;
        upsert_operational(&txn, row, PendingConfirmState::Pending, None)?;
        super::dispatch_history::insert_history(&txn, row)?;
        txn.commit().context("commit dispatch txn")?;
        Ok(())
    }

    /// Insert an operational row directly in `'confirmed'` state and
    /// append the audit row — used by the orphan-confirm recovery
    /// path when an agent posts `/v1/agent/confirm` but no matching
    /// pending row exists (typically because the CP was rebuilt
    /// mid-flight). The orphan handler verifies the agent's
    /// `closure_hash` matches the host's declared target before
    /// calling this; the synthetic row preserves the audit trail of
    /// "this host activated this closure" without forcing a spurious
    /// rollback. `confirm_deadline` is set to `confirmed_at` since
    /// the deadline is moot for an already-confirmed row.
    #[allow(clippy::too_many_arguments)]
    pub fn record_confirmed_dispatch(
        &self,
        hostname: &str,
        rollout_id: &str,
        channel: &str,
        wave: u32,
        target_closure_hash: &str,
        target_channel_ref: &str,
        confirmed_at: DateTime<Utc>,
    ) -> Result<()> {
        let mut guard = super::lock_conn(self.conn)?;
        let txn = guard.transaction().context("begin confirmed dispatch txn")?;
        let row = DispatchInsert {
            hostname,
            rollout_id,
            channel,
            wave,
            target_closure_hash,
            target_channel_ref,
            confirm_deadline: confirmed_at,
        };
        upsert_operational(
            &txn,
            &row,
            PendingConfirmState::Confirmed,
            Some(confirmed_at),
        )?;
        super::dispatch_history::insert_history(&txn, &row)?;
        txn.commit().context("commit confirmed dispatch txn")?;
        Ok(())
    }

    /// Returns true if the host has an operational row in state
    /// `'pending'`. Used by the dispatch loop to avoid re-dispatching
    /// while an activation is in flight.
    pub fn pending_dispatch_exists(&self, hostname: &str) -> Result<bool> {
        let guard = super::lock_conn(self.conn)?;
        let n: i64 = guard
            .query_row(
                "SELECT COUNT(*) FROM host_dispatch_state
                 WHERE hostname = ?1 AND state = ?2",
                params![hostname, PendingConfirmState::Pending.as_db_str()],
                |row| row.get(0),
            )
            .context("count host_dispatch_state pending")?;
        Ok(n > 0)
    }

    /// Mark a pending dispatch as confirmed. Called by the
    /// `/v1/agent/confirm` handler. Returns the number of rows
    /// updated — 0 means no matching pending row (rollout cancelled,
    /// deadline already expired, agent confirming twice, or the host
    /// has been overwritten by a newer dispatch).
    ///
    /// The `confirm_deadline > datetime('now')` clause is load-bearing:
    /// without it, late confirms whose deadline has already passed
    /// would still flip pending → confirmed, sneaking past the
    /// rollback contract. The orphan-confirm-recovery path is the
    /// legitimate "late confirm from a CP-rebuild" escape hatch —
    /// it re-checks the closure hash against the verified target
    /// before synthesizing a row, and is exercised by
    /// `fleet-harness-deadline-expiry`.
    pub fn confirm(&self, hostname: &str, rollout_id: &str) -> Result<usize> {
        let guard = super::lock_conn(self.conn)?;
        let n = guard
            .execute(
                "UPDATE host_dispatch_state
                 SET confirmed_at = datetime('now'),
                     state = ?3
                 WHERE hostname = ?1
                   AND rollout_id = ?2
                   AND state = ?4
                   AND datetime(confirm_deadline) > datetime('now')",
                params![
                    hostname,
                    rollout_id,
                    PendingConfirmState::Confirmed.as_db_str(),
                    PendingConfirmState::Pending.as_db_str(),
                ],
            )
            .context("update host_dispatch_state confirmed")?;
        Ok(n)
    }

    /// Operational rows whose deadline has passed and which haven't
    /// been confirmed. Used by the magic-rollback timer — each row
    /// returned is a host that failed to confirm in time and should
    /// be rolled back.
    ///
    /// Wraps `confirm_deadline` in `datetime(...)` so SQLite parses
    /// the stored RFC3339 string (`YYYY-MM-DDTHH:MM:SS+00:00`) into
    /// the same canonical shape `datetime('now')` returns; naked
    /// string compare ranks 'T' (0x54) above ' ' (0x20) at position
    /// 10, so deadlines look greater than now forever and the timer
    /// is a no-op.
    pub fn pending_deadlines(&self) -> Result<Vec<ExpiredDispatch>> {
        let guard = super::lock_conn(self.conn)?;
        let mut stmt = guard.prepare(
            "SELECT hostname, rollout_id, wave, target_closure_hash
             FROM host_dispatch_state
             WHERE state = ?1
               AND datetime(confirm_deadline) < datetime('now')",
        )?;
        let rows = stmt
            .query_map(params![PendingConfirmState::Pending.as_db_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u32>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Mark expired dispatches as rolled-back at the operational
    /// level. Idempotent — only updates rows still in 'pending', so
    /// a second call with the same pairs is a no-op.
    /// `(hostname, rollout_id)` pairs come from
    /// [`Self::pending_deadlines`]; the rollback timer also stamps
    /// the audit row via
    /// [`super::dispatch_history::DispatchHistory::mark_terminal_for_rollout_host`].
    pub fn mark_rolled_back(&self, pairs: &[(String, String)]) -> Result<usize> {
        if pairs.is_empty() {
            return Ok(0);
        }
        let mut guard = super::lock_conn(self.conn)?;
        let txn = guard.transaction().context("begin mark_rolled_back txn")?;
        let mut updated = 0usize;
        {
            let mut stmt = txn.prepare(
                "UPDATE host_dispatch_state
                 SET state = ?3
                 WHERE hostname = ?1
                   AND rollout_id = ?2
                   AND state = ?4",
            )?;
            for (hostname, rollout_id) in pairs {
                updated += stmt.execute(params![
                    hostname,
                    rollout_id,
                    PendingConfirmState::RolledBack.as_db_str(),
                    PendingConfirmState::Pending.as_db_str(),
                ])?;
            }
        }
        txn.commit().context("commit mark_rolled_back txn")?;
        Ok(updated)
    }

    /// Race-resistant terminal flip. Used by the report handler when
    /// `RollbackTriggered` closes the rollback-and-halt loop —
    /// operational state moves to `terminal_state` for THIS rollout.
    /// If a newer dispatch has overwritten the row (different
    /// rollout_id), the WHERE clause doesn't match and the call is a
    /// no-op (returns 0). The audit row is stamped separately via
    /// [`super::dispatch_history::DispatchHistory::mark_terminal_for_rollout_host`].
    pub fn record_terminal(
        &self,
        hostname: &str,
        rollout_id: &str,
        terminal: TerminalState,
    ) -> Result<usize> {
        // Map TerminalState → operational state literal. Converged is
        // not an operational terminal (operational stays Confirmed
        // post-converge — the row's "current" rollout converged but
        // the host's last known state was Confirmed); only RolledBack
        // and Cancelled flip the operational column.
        let new_state = match terminal {
            TerminalState::Converged => return Ok(0),
            TerminalState::RolledBack => PendingConfirmState::RolledBack,
            TerminalState::Cancelled => PendingConfirmState::Cancelled,
        };
        let guard = super::lock_conn(self.conn)?;
        let n = guard
            .execute(
                "UPDATE host_dispatch_state
                 SET state = ?3
                 WHERE hostname = ?1
                   AND rollout_id = ?2",
                params![hostname, rollout_id, new_state.as_db_str()],
            )
            .context("record_terminal host_dispatch_state")?;
        Ok(n)
    }

    /// Read the operational row for a host. Returns `Ok(None)` when
    /// no row exists.
    pub fn host_state(&self, hostname: &str) -> Result<Option<HostDispatchStateRow>> {
        let guard = super::lock_conn(self.conn)?;
        let row = guard
            .query_row(
                "SELECT hostname, rollout_id, channel, wave,
                        target_closure_hash, target_channel_ref,
                        state, dispatched_at, confirm_deadline,
                        confirmed_at
                 FROM host_dispatch_state
                 WHERE hostname = ?1",
                params![hostname],
                row_to_host_dispatch_state,
            )
            .ok();
        Ok(row)
    }

    /// Snapshot the active rollouts derived from the operational
    /// table for the observed-state projection. One row per host,
    /// terminal states (`rolled-back` / `cancelled`) filtered out.
    /// LEFT JOIN `host_rollout_state` for the per-host machine
    /// state + soak-timer marker.
    ///
    /// Filtering terminal rows is load-bearing: a rollout whose
    /// every host is terminal would otherwise surface as an empty
    /// `host_states` map, and the reconciler defaults absent host-
    /// state lookups to "Queued" — re-dispatching all those hosts.
    /// Skipping terminal rows entirely avoids that trap.
    ///
    /// Output order: rollout_id ascending. Deterministic so
    /// projection tests can compare against expected vectors and
    /// the reconciler's journal lines stay grep-stable.
    ///
    /// Lives on `HostDispatchState` because the operational table is
    /// the authoritative source of "what each host is currently
    /// dispatched to do" — `dispatch_history` is the audit log, not
    /// the snapshot.
    pub fn active_rollouts_snapshot(&self) -> Result<Vec<RolloutDbSnapshot>> {
        use std::collections::BTreeMap;

        let guard = super::lock_conn(self.conn)?;
        let mut stmt = guard.prepare(
            "SELECT hds.rollout_id, hds.channel, hds.hostname,
                    hds.target_closure_hash, hds.target_channel_ref,
                    hds.state,
                    hrs.host_state, hrs.last_healthy_since
             FROM host_dispatch_state hds
             LEFT JOIN host_rollout_state hrs
                    ON hrs.rollout_id = hds.rollout_id
                   AND hrs.hostname = hds.hostname
             WHERE hds.state IN (?1, ?2)
             ORDER BY hds.rollout_id, hds.hostname",
        )?;
        let rows = stmt
            .query_map(
                params![
                    PendingConfirmState::Pending.as_db_str(),
                    PendingConfirmState::Confirmed.as_db_str(),
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,         // rollout_id
                        row.get::<_, String>(1)?,         // channel
                        row.get::<_, String>(2)?,         // hostname
                        row.get::<_, String>(3)?,         // target_closure_hash
                        row.get::<_, String>(4)?,         // target_channel_ref
                        row.get::<_, String>(5)?,         // hds.state
                        row.get::<_, Option<String>>(6)?, // hrs.host_state
                        row.get::<_, Option<String>>(7)?, // hrs.last_healthy_since
                    ))
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut by_rollout: BTreeMap<String, RolloutDbSnapshot> = BTreeMap::new();
        for (
            rollout_id,
            row_channel,
            hostname,
            target_closure,
            target_ref,
            op_state,
            hrs_state,
            hrs_ts,
        ) in rows
        {
            // Derive the host's state literal. `host_rollout_state`
            // wins when present (post-confirm machine: Healthy /
            // Soaked / …); otherwise infer from operational state.
            // The RolledBack/Cancelled match guard is unreachable —
            // the WHERE filter excludes terminal rows.
            let host_state = match hrs_state {
                Some(s) => HostRolloutState::from_db_str(&s)?.as_db_str().to_string(),
                None => match PendingConfirmState::from_db_str(&op_state)? {
                    PendingConfirmState::Pending => HostRolloutState::ConfirmWindow,
                    PendingConfirmState::Confirmed => HostRolloutState::Healthy,
                    PendingConfirmState::RolledBack | PendingConfirmState::Cancelled => {
                        unreachable!(
                            "filtered by WHERE hds.state IN ('pending','confirmed') in the SELECT",
                        )
                    }
                }
                .as_db_str()
                .to_string(),
            };

            // Use the explicit `channel` column when populated; fall
            // back to legacy parsing of the `<channel>@<short-ci-commit>`
            // form for rows that came from a pre-content-addressed
            // dispatch. If both fail, leave empty — the reconciler
            // then emits ChannelUnknown legitimately (drift-detector
            // intent).
            let channel = if !row_channel.is_empty() {
                row_channel
            } else {
                rollout_id
                    .split_once('@')
                    .map(|(c, _)| c.to_string())
                    .unwrap_or_default()
            };

            let entry = by_rollout
                .entry(rollout_id.clone())
                .or_insert_with(|| RolloutDbSnapshot {
                    rollout_id: rollout_id.clone(),
                    channel,
                    target_closure_hash: target_closure.clone(),
                    target_channel_ref: target_ref.clone(),
                    host_states: HashMap::new(),
                    last_healthy_since: HashMap::new(),
                });
            entry.host_states.insert(hostname.clone(), host_state);
            if let Some(ts) = hrs_ts {
                let parsed = ts
                    .parse::<DateTime<Utc>>()
                    .with_context(|| format!("parse last_healthy_since for {hostname}"))?;
                entry.last_healthy_since.insert(hostname, parsed);
            }
        }
        Ok(by_rollout.into_values().collect())
    }
}

/// Operational row returned by [`HostDispatchState::host_state`].
#[derive(Debug, Clone)]
pub struct HostDispatchStateRow {
    pub hostname: String,
    pub rollout_id: String,
    pub channel: String,
    pub wave: u32,
    pub target_closure_hash: String,
    pub target_channel_ref: String,
    pub state: String,
    pub dispatched_at: String,
    pub confirm_deadline: String,
    pub confirmed_at: Option<String>,
}

fn row_to_host_dispatch_state(row: &rusqlite::Row<'_>) -> rusqlite::Result<HostDispatchStateRow> {
    Ok(HostDispatchStateRow {
        hostname: row.get(0)?,
        rollout_id: row.get(1)?,
        channel: row.get(2)?,
        wave: row.get(3)?,
        target_closure_hash: row.get(4)?,
        target_channel_ref: row.get(5)?,
        state: row.get(6)?,
        dispatched_at: row.get(7)?,
        confirm_deadline: row.get(8)?,
        confirmed_at: row.get(9)?,
    })
}

/// UPSERT the operational row. Pulled out so [`HostDispatchState::record_dispatch`]
/// and [`HostDispatchState::record_confirmed_dispatch`] share the
/// same column-write contract.
fn upsert_operational(
    conn: &Connection,
    row: &DispatchInsert<'_>,
    state: PendingConfirmState,
    confirmed_at: Option<DateTime<Utc>>,
) -> Result<()> {
    let confirmed_at_str = confirmed_at.map(|t| t.to_rfc3339());
    conn.execute(
        "INSERT INTO host_dispatch_state(
             hostname, rollout_id, channel, wave,
             target_closure_hash, target_channel_ref,
             state, dispatched_at, confirm_deadline, confirmed_at
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'), ?8, ?9)
         ON CONFLICT(hostname) DO UPDATE SET
             rollout_id = excluded.rollout_id,
             channel = excluded.channel,
             wave = excluded.wave,
             target_closure_hash = excluded.target_closure_hash,
             target_channel_ref = excluded.target_channel_ref,
             state = excluded.state,
             dispatched_at = excluded.dispatched_at,
             confirm_deadline = excluded.confirm_deadline,
             confirmed_at = excluded.confirmed_at",
        params![
            row.hostname,
            row.rollout_id,
            row.channel,
            row.wave,
            row.target_closure_hash,
            row.target_channel_ref,
            state.as_db_str(),
            row.confirm_deadline.to_rfc3339(),
            confirmed_at_str,
        ],
    )
    .context("upsert host_dispatch_state")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::{dispatch_insert, fresh_db, mark_healthy};
    use crate::state::TerminalState;
    use chrono::Utc;

    #[test]
    fn record_dispatch_writes_operational_and_history() {
        let db = fresh_db();
        let deadline = Utc::now() + chrono::Duration::seconds(120);
        db.host_dispatch_state()
            .record_dispatch(&dispatch_insert(
                "ohm",
                "stable@abc",
                "system-r1",
                deadline,
            ))
            .unwrap();
        let row = db.host_dispatch_state().host_state("ohm").unwrap().unwrap();
        assert_eq!(row.rollout_id, "stable@abc");
        assert_eq!(row.state, "pending");
        let history = db.dispatch_history().recent_for_host("ohm", 10).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].rollout_id, "stable@abc");
        assert!(history[0].terminal_state.is_none());
    }

    #[test]
    fn upsert_replaces_existing_row() {
        // Re-dispatch overwrites the operational row in place but
        // appends a new history row each time. After two dispatches
        // there's one operational row (latest wins) and two history
        // rows (full audit trail).
        let db = fresh_db();
        let deadline = Utc::now() + chrono::Duration::seconds(120);
        db.host_dispatch_state()
            .record_dispatch(&dispatch_insert("ohm", "stable@r1", "old", deadline))
            .unwrap();
        db.host_dispatch_state()
            .record_dispatch(&dispatch_insert("ohm", "stable@r2", "new", deadline))
            .unwrap();
        let row = db.host_dispatch_state().host_state("ohm").unwrap().unwrap();
        assert_eq!(row.rollout_id, "stable@r2");
        assert_eq!(row.target_closure_hash, "new");
        let history = db.dispatch_history().recent_for_host("ohm", 10).unwrap();
        assert_eq!(history.len(), 2, "history grows on each dispatch");
    }

    #[test]
    fn confirm_flips_state() {
        let db = fresh_db();
        let deadline = Utc::now() + chrono::Duration::seconds(120);
        db.host_dispatch_state()
            .record_dispatch(&dispatch_insert("ohm", "stable@r1", "system-r1", deadline))
            .unwrap();
        let n = db.host_dispatch_state().confirm("ohm", "stable@r1").unwrap();
        assert_eq!(n, 1);
        let row = db.host_dispatch_state().host_state("ohm").unwrap().unwrap();
        assert_eq!(row.state, "confirmed");
        assert!(row.confirmed_at.is_some());
    }

    #[test]
    fn confirm_no_op_when_deadline_passed() {
        // Regression-pin: late confirms whose deadline has already
        // passed must NOT flip the row pending → confirmed. The
        // rollback contract depends on this gate; absence here is the
        // failure mode `fleet-harness-deadline-expiry` exercises.
        let db = fresh_db();
        let past_deadline = Utc::now() - chrono::Duration::seconds(30);
        db.host_dispatch_state()
            .record_dispatch(&dispatch_insert(
                "ohm",
                "stable@expired",
                "system-r1",
                past_deadline,
            ))
            .unwrap();
        let n = db
            .host_dispatch_state()
            .confirm("ohm", "stable@expired")
            .unwrap();
        assert_eq!(
            n, 0,
            "confirm must not flip a pending row whose deadline has passed",
        );
        let row = db.host_dispatch_state().host_state("ohm").unwrap().unwrap();
        assert_eq!(
            row.state, "pending",
            "row stays pending until rollback_timer or 410-handler transitions it",
        );
    }

    #[test]
    fn confirm_no_op_on_wrong_rollout() {
        let db = fresh_db();
        let deadline = Utc::now() + chrono::Duration::seconds(120);
        db.host_dispatch_state()
            .record_dispatch(&dispatch_insert("ohm", "stable@r1", "system-r1", deadline))
            .unwrap();
        // Wrong rollout id — guard fails, no flip.
        let n = db.host_dispatch_state().confirm("ohm", "stable@r2").unwrap();
        assert_eq!(n, 0);
        let row = db.host_dispatch_state().host_state("ohm").unwrap().unwrap();
        assert_eq!(row.state, "pending");
    }

    #[test]
    fn pending_deadlines_picks_past_window() {
        let db = fresh_db();
        let past = Utc::now() - chrono::Duration::seconds(60);
        db.host_dispatch_state()
            .record_dispatch(&dispatch_insert("ohm", "stable@old", "system", past))
            .unwrap();
        let future = Utc::now() + chrono::Duration::seconds(120);
        db.host_dispatch_state()
            .record_dispatch(&dispatch_insert("krach", "stable@new", "system", future))
            .unwrap();
        let expired = db.host_dispatch_state().pending_deadlines().unwrap();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].0, "ohm");
        assert_eq!(expired[0].1, "stable@old");
    }

    #[test]
    fn mark_rolled_back_flips_pending_only() {
        let db = fresh_db();
        let past = Utc::now() - chrono::Duration::seconds(60);
        db.host_dispatch_state()
            .record_dispatch(&dispatch_insert("ohm", "stable@r1", "system", past))
            .unwrap();
        // First call: row is pending → flips to rolled-back.
        let n = db
            .host_dispatch_state()
            .mark_rolled_back(&[("ohm".to_string(), "stable@r1".to_string())])
            .unwrap();
        assert_eq!(n, 1);
        // Idempotent: repeat call is no-op (row no longer pending).
        let n = db
            .host_dispatch_state()
            .mark_rolled_back(&[("ohm".to_string(), "stable@r1".to_string())])
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn record_terminal_no_op_when_rollout_id_mismatches() {
        // Race-resistance: a newer dispatch landed (different
        // rollout_id), the report handler's RollbackTriggered post
        // arrives carrying the OLD rollout id. The WHERE filter
        // protects the new row.
        let db = fresh_db();
        let deadline = Utc::now() + chrono::Duration::seconds(120);
        db.host_dispatch_state()
            .record_dispatch(&dispatch_insert("ohm", "stable@new", "system-new", deadline))
            .unwrap();
        let n = db
            .host_dispatch_state()
            .record_terminal("ohm", "stable@old", TerminalState::RolledBack)
            .unwrap();
        assert_eq!(n, 0);
        let row = db.host_dispatch_state().host_state("ohm").unwrap().unwrap();
        assert_eq!(row.state, "pending", "newer dispatch must not be flipped");
    }

    #[test]
    fn record_terminal_flips_matching_rollout() {
        let db = fresh_db();
        let deadline = Utc::now() + chrono::Duration::seconds(120);
        db.host_dispatch_state()
            .record_dispatch(&dispatch_insert("ohm", "stable@r1", "system", deadline))
            .unwrap();
        let n = db
            .host_dispatch_state()
            .record_terminal("ohm", "stable@r1", TerminalState::RolledBack)
            .unwrap();
        assert_eq!(n, 1);
        let row = db.host_dispatch_state().host_state("ohm").unwrap().unwrap();
        assert_eq!(row.state, "rolled-back");
    }

    #[test]
    fn record_confirmed_dispatch_writes_confirmed_state() {
        // Orphan-recovery path: synthetic row lands directly in
        // 'confirmed' with confirmed_at populated. Surfaces in the
        // snapshot with derived host_state="Healthy" until a
        // host_rollout_state row is written.
        let db = fresh_db();
        let now = Utc::now();
        db.host_dispatch_state()
            .record_confirmed_dispatch(
                "ohm",
                "stable@orphan",
                "stable",
                0,
                "target-system",
                "stable@orphan",
                now,
            )
            .unwrap();
        let row = db.host_dispatch_state().host_state("ohm").unwrap().unwrap();
        assert_eq!(row.state, "confirmed");
        assert!(row.confirmed_at.is_some());
        let snap = db.host_dispatch_state().active_rollouts_snapshot().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(
            snap[0].host_states.get("ohm").map(String::as_str),
            Some("Healthy"),
        );
    }

    #[test]
    fn active_rollouts_snapshot_excludes_terminal_states() {
        // Operational row in rolled-back must NOT surface — its
        // empty host_states map would default to "Queued" in the
        // reconciler and trigger spurious re-dispatches.
        let db = fresh_db();
        let past = Utc::now() - chrono::Duration::seconds(60);
        db.host_dispatch_state()
            .record_dispatch(&dispatch_insert("ohm", "stable@dead", "system", past))
            .unwrap();
        let pairs = vec![("ohm".to_string(), "stable@dead".to_string())];
        db.host_dispatch_state().mark_rolled_back(&pairs).unwrap();
        let snap = db.host_dispatch_state().active_rollouts_snapshot().unwrap();
        assert!(snap.is_empty());
    }

    #[test]
    fn active_rollouts_snapshot_pending_surfaces_as_confirmwindow() {
        let db = fresh_db();
        let future = Utc::now() + chrono::Duration::seconds(120);
        db.host_dispatch_state()
            .record_dispatch(&dispatch_insert(
                "ohm",
                "stable@abc12345",
                "system-r1",
                future,
            ))
            .unwrap();
        let snap = db.host_dispatch_state().active_rollouts_snapshot().unwrap();
        assert_eq!(snap.len(), 1);
        let r = &snap[0];
        assert_eq!(r.rollout_id, "stable@abc12345");
        assert_eq!(r.channel, "stable");
        assert_eq!(r.target_closure_hash, "system-r1");
        assert_eq!(
            r.host_states.get("ohm").map(String::as_str),
            Some("ConfirmWindow"),
        );
        assert!(r.last_healthy_since.is_empty());
    }

    #[test]
    fn active_rollouts_snapshot_confirmed_uses_host_rollout_state() {
        let db = fresh_db();
        let future = Utc::now() + chrono::Duration::seconds(120);
        let now = Utc::now();
        db.host_dispatch_state()
            .record_dispatch(&dispatch_insert(
                "ohm",
                "stable@abc12345",
                "system-r1",
                future,
            ))
            .unwrap();
        db.host_dispatch_state().confirm("ohm", "stable@abc12345").unwrap();
        mark_healthy(&db, "ohm", "stable@abc12345", now);
        let snap = db.host_dispatch_state().active_rollouts_snapshot().unwrap();
        assert_eq!(snap.len(), 1);
        let r = &snap[0];
        assert_eq!(
            r.host_states.get("ohm").map(String::as_str),
            Some("Healthy"),
        );
        let stored = r.last_healthy_since.get("ohm").expect("Healthy host has soak ts");
        assert_eq!(stored.timestamp(), now.timestamp());
    }

    #[test]
    fn active_rollouts_snapshot_legacy_channel_fallback() {
        // Pre-V005 row whose backfill missed: simulate by writing
        // an empty channel directly. Snapshot must split the
        // <channel>@<ref> form to recover the channel name.
        let db = fresh_db();
        let future = Utc::now() + chrono::Duration::seconds(120);
        let mut row = dispatch_insert("ohm", "stable@abc12345", "system-r1", future);
        row.channel = "";
        db.host_dispatch_state().record_dispatch(&row).unwrap();
        let snap = db.host_dispatch_state().active_rollouts_snapshot().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(
            snap[0].channel, "stable",
            "legacy <channel>@<ref> rolloutIds must still resolve via split fallback",
        );
    }

    #[test]
    fn active_rollouts_snapshot_uses_explicit_channel_for_sha_rollout_id() {
        let db = fresh_db();
        let future = Utc::now() + chrono::Duration::seconds(120);
        let sha_rollout = "1111111111111111111111111111111111111111111111111111111111111111";
        let mut row = dispatch_insert("ohm", sha_rollout, "system-r1", future);
        row.channel = "edge-slow";
        db.host_dispatch_state().record_dispatch(&row).unwrap();
        let snap = db.host_dispatch_state().active_rollouts_snapshot().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].channel, "edge-slow");
    }

    #[test]
    fn pending_dispatch_exists_returns_only_for_pending() {
        let db = fresh_db();
        let future = Utc::now() + chrono::Duration::seconds(120);
        db.host_dispatch_state()
            .record_dispatch(&dispatch_insert("ohm", "stable@r1", "system", future))
            .unwrap();
        assert!(db.host_dispatch_state().pending_dispatch_exists("ohm").unwrap());
        db.host_dispatch_state().confirm("ohm", "stable@r1").unwrap();
        assert!(
            !db.host_dispatch_state().pending_dispatch_exists("ohm").unwrap(),
            "confirmed row is not pending",
        );
    }
}
