//! Operational dispatch row, one per host (soft state); orphan-confirm recovers from loss.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::sync::Mutex;

use crate::state::{HostRolloutState, PendingConfirmState, TerminalState};

#[derive(Debug, Clone)]
pub struct DispatchInsert<'a> {
    pub hostname: &'a str,
    pub rollout_id: &'a str,
    /// Persisted explicitly: rolloutIds are content hashes that don't encode the channel.
    pub channel: &'a str,
    pub wave: u32,
    pub target_closure_hash: &'a str,
    pub target_channel_ref: &'a str,
    pub confirm_deadline: DateTime<Utc>,
}

/// Joined snapshot for observed-state projection; terminal rows filtered out.
#[derive(Debug, Clone)]
pub struct RolloutDbSnapshot {
    pub rollout_id: String,
    pub channel: String,
    pub target_closure_hash: String,
    pub target_channel_ref: String,
    /// `host_rollout_state` wins when present; otherwise derived from operational state.
    pub host_states: HashMap<String, String>,
    /// Excludes hosts not currently Healthy.
    pub last_healthy_since: HashMap<String, DateTime<Utc>>,
}

/// `(hostname, rollout_id, wave, target_closure_hash)` for rows with a passed deadline.
pub type ExpiredDispatch = (String, String, u32, String);

pub struct HostDispatchState<'a> {
    pub(super) conn: &'a Mutex<Connection>,
}

impl HostDispatchState<'_> {
    /// Upserts operational row + appends audit row in one transaction.
    pub fn record_dispatch(&self, row: &DispatchInsert<'_>) -> Result<()> {
        let mut guard = super::lock_conn(self.conn)?;
        let txn = guard.transaction().context("begin dispatch txn")?;
        upsert_operational(&txn, row, PendingConfirmState::Pending, None)?;
        super::dispatch_history::insert_history(&txn, row)?;
        txn.commit().context("commit dispatch txn")?;
        Ok(())
    }

    /// Orphan-confirm recovery: synthesises a row directly in `'confirmed'`.
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

    /// True if the host has a `'pending'` row.
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

    /// Flips pending → confirmed; deadline gate prevents late confirms bypassing rollback.
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

    /// `datetime(...)` wrapper is required: naked string compare ranks 'T' > ' ' and breaks the timer.
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

    /// Idempotent: only flips rows still in 'pending'.
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

    /// Race-resistant: WHERE rollout_id guard makes a stale id a no-op when overwritten.
    pub fn record_terminal(
        &self,
        hostname: &str,
        rollout_id: &str,
        terminal: TerminalState,
    ) -> Result<usize> {
        // LOADBEARING: Converged stays Confirmed at the operational level; only RolledBack/Cancelled flip the column.
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

    /// Filtering terminal rows prevents the reconciler defaulting absent host-states to Queued and re-dispatching.
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
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
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

            // Legacy fallback: parse `<channel>@<ref>` for pre-content-addressed rows.
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
        let n = db
            .host_dispatch_state()
            .mark_rolled_back(&[("ohm".to_string(), "stable@r1".to_string())])
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn record_terminal_no_op_when_rollout_id_mismatches() {
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
