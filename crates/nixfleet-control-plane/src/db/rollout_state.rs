//! `host_rollout_state` — per-host soak markers and the joined
//! `active_rollouts` projection.
//!
//! Recovery class: **soft state** (ARCHITECTURE.md §6 Phase 10).
//! Loss restarts soak windows from zero. Mitigated by agent-attested
//! `last_confirmed_at` (#47): the agent persists the moment of its
//! most recent successful confirm and echoes it on every checkin;
//! the CP repopulates `last_healthy_since` from the attestation,
//! clamped to `min(now, attested)`.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::sync::Mutex;

use crate::state::{HealthyMarker, HostRolloutState, PendingConfirmState};

/// Joined snapshot of `pending_confirms` + `host_rollout_state` for
/// the observed-state projection. Rollouts are derived (no dedicated
/// table); `rolled-back`/`cancelled` rows are filtered out so dead
/// rollouts don't surface as empty-host-states the reconciler would
/// re-dispatch.
#[derive(Debug, Clone)]
pub struct RolloutDbSnapshot {
    pub rollout_id: String,
    pub channel: String,
    pub target_closure_hash: String,
    pub target_channel_ref: String,
    /// `host_rollout_state` wins when present; otherwise derived
    /// from the latest `pending_confirms.state`.
    pub host_states: HashMap<String, String>,
    /// Excludes hosts whose marker is NULL (not currently Healthy).
    pub last_healthy_since: HashMap<String, DateTime<Utc>>,
}

pub struct RolloutState<'a> {
    pub(super) conn: &'a Mutex<Connection>,
}

impl RolloutState<'_> {
    /// Transition (rollout, host) into `new_state`, optionally
    /// stamping `last_healthy_since` via `marker`. Replaces the
    /// per-state pair of methods (`record_host_healthy`,
    /// `mark_host_soaked`) with a single typed entry routed through
    /// [`HostRolloutState`] — magic strings stop leaking into db
    /// and a typo'd variant becomes a compile error.
    ///
    /// Semantics:
    /// - `expected_from = None`: UPSERT — insert a new row in
    ///   `new_state` or overwrite the existing row's state. This is
    ///   the shape the confirm handler needs (no precondition; the
    ///   handler authoritatively declares the host Healthy).
    /// - `expected_from = Some(prev)`: UPDATE-only with a
    ///   state-machine guard — only fires when the existing row's
    ///   `host_state` matches `prev`. Returns 0 if the precondition
    ///   fails. Used by the reconciler arm whose actions are
    ///   directional (e.g. Healthy → Soaked).
    ///
    /// `marker` controls `last_healthy_since`:
    /// - `Set(now)`: stamp it (used when entering Healthy).
    /// - `Untouched`: leave it as-is (default for every other
    ///   transition).
    ///
    /// Returns the number of rows written (1 for UPSERT, 0 or 1 for
    /// guarded UPDATE).
    pub fn transition_host_state(
        &self,
        hostname: &str,
        rollout_id: &str,
        new_state: HostRolloutState,
        marker: HealthyMarker,
        expected_from: Option<HostRolloutState>,
    ) -> Result<usize> {
        let guard = super::lock_conn(self.conn)?;
        let new_state_str = new_state.as_db_str();
        // `Untouched` → NULL; the SQL uses COALESCE so a NULL bind
        // preserves the existing column value rather than writing
        // NULL over it. Single point of conversion lets each branch
        // below stay a single static SQL statement (query plan is
        // identical across every call regardless of marker variant).
        let marker_bind: Option<String> = match marker {
            HealthyMarker::Set(ts) => Some(ts.to_rfc3339()),
            HealthyMarker::Untouched => None,
        };

        let n = match expected_from {
            None => {
                // UPSERT path. Mirrors the legacy `record_host_healthy`
                // shape but parameterised over the target state and
                // healthy-marker. Matches the V003 schema's column
                // order. COALESCE on the conflict branch preserves
                // the existing `last_healthy_since` when the marker is
                // Untouched (NULL bind); on insert the column starts
                // NULL anyway so the bind goes straight in.
                guard
                    .execute(
                        "INSERT INTO host_rollout_state(rollout_id, hostname,
                                                        host_state,
                                                        last_healthy_since,
                                                        updated_at)
                         VALUES (?1, ?2, ?3, ?4, datetime('now'))
                         ON CONFLICT(rollout_id, hostname) DO UPDATE SET
                           host_state = excluded.host_state,
                           last_healthy_since = COALESCE(
                               excluded.last_healthy_since,
                               host_rollout_state.last_healthy_since),
                           updated_at = datetime('now')",
                        params![rollout_id, hostname, new_state_str, marker_bind],
                    )
                    .context("upsert host_rollout_state")?
            }
            Some(prev) => {
                // Guarded UPDATE path. Mirrors the legacy
                // `mark_host_soaked` "only from Healthy" filter, now
                // parameterised so any directional transition (Healthy
                // → Soaked, Soaked → Converged, etc.) routes through
                // the same code. COALESCE keeps `last_healthy_since`
                // unchanged when the marker is Untouched.
                guard
                    .execute(
                        "UPDATE host_rollout_state
                         SET host_state = ?3,
                             last_healthy_since = COALESCE(?4, last_healthy_since),
                             updated_at = datetime('now')
                         WHERE rollout_id = ?1 AND hostname = ?2
                           AND host_state = ?5",
                        params![
                            rollout_id,
                            hostname,
                            new_state_str,
                            marker_bind,
                            prev.as_db_str()
                        ],
                    )
                    .context("guarded transition host_rollout_state")?
            }
        };
        Ok(n)
    }

    /// Clear the Healthy marker for (rollout, host) — the host has
    /// left Healthy (its `current_generation.closure_hash` no
    /// longer matches the rollout's target). Nulls
    /// `last_healthy_since` so the soak timer must restart on the
    /// next Healthy entry. The `host_state` column is intentionally
    /// untouched — this is NOT a state transition: step 3's
    /// reconciler arm decides what state to transition to
    /// (typically back to ConfirmWindow, or Failed).
    /// Returns the number of rows updated — 0 means there was no
    /// Healthy marker to clear.
    pub fn clear_healthy_marker(&self, hostname: &str, rollout_id: &str) -> Result<usize> {
        let guard = super::lock_conn(self.conn)?;
        let n = guard
            .execute(
                "UPDATE host_rollout_state
                 SET last_healthy_since = NULL,
                     updated_at = datetime('now')
                 WHERE rollout_id = ?1 AND hostname = ?2
                   AND last_healthy_since IS NOT NULL",
                params![rollout_id, hostname],
            )
            .context("clear host_rollout_state.last_healthy_since")?;
        Ok(n)
    }

    /// Read the current `host_state` for (rollout_id, hostname).
    /// Returns `Ok(None)` when no row exists. Public so tests in
    /// sibling modules can assert state transitions without
    /// re-deriving the projection through `active_rollouts_snapshot`.
    pub fn host_state(&self, hostname: &str, rollout_id: &str) -> Result<Option<String>> {
        let guard = super::lock_conn(self.conn)?;
        let row = guard
            .query_row(
                "SELECT host_state FROM host_rollout_state
                 WHERE rollout_id = ?1 AND hostname = ?2",
                params![rollout_id, hostname],
                |row| row.get::<_, String>(0),
            )
            .ok();
        Ok(row)
    }

    /// Delete `pending_confirms` + `host_rollout_state` rows for a
    /// fully-converged rollout. Called from `apply_actions` when the
    /// reconciler emits `Action::ConvergeRollout`. Without this, every
    /// confirmed `pending_confirms` row from a converged rollout
    /// keeps surfacing in `active_rollouts_snapshot` (state
    /// 'confirmed') forever — bloating the snapshot, double-counting
    /// hosts in disruption-budget calculations, and emitting
    /// ChannelUnknown noise (post-#80, with empty channel; pre-#80,
    /// with the SHA itself).
    ///
    /// Returns `(pending_confirms_deleted, host_rollout_state_deleted)`.
    ///
    /// Lives on `RolloutState` (not `Confirms`) because the reconciler
    /// reaches it through the same accessor that owns
    /// `active_rollouts_snapshot` — the symmetric "create vs purge"
    /// path for the rollout-state projection.
    pub fn delete_rollout_records(&self, rollout_id: &str) -> Result<(usize, usize)> {
        let guard = super::lock_conn(self.conn)?;
        let pc_n = guard
            .execute(
                "DELETE FROM pending_confirms WHERE rollout_id = ?1",
                params![rollout_id],
            )
            .context("delete pending_confirms for converged rollout")?;
        let hrs_n = guard
            .execute(
                "DELETE FROM host_rollout_state WHERE rollout_id = ?1",
                params![rollout_id],
            )
            .context("delete host_rollout_state for converged rollout")?;
        Ok((pc_n, hrs_n))
    }

    /// Per-host counterpart of [`Self::delete_rollout_records`]. Called
    /// from `apply_rollback_state_transition` when a host transitions
    /// `Failed → Reverted` under rollback-and-halt. The rollout itself
    /// may still be active for OTHER hosts (Halted but with siblings
    /// in pre-terminal states), so cleaning the whole rollout would
    /// be wrong; this scoped variant clears only the (rollout_id,
    /// hostname) pair so the Reverted row stops contributing to
    /// `active_rollouts_snapshot` going forward.
    ///
    /// Returns `(pending_confirms_deleted, host_rollout_state_deleted)`.
    pub fn delete_rollout_host_records(
        &self,
        rollout_id: &str,
        hostname: &str,
    ) -> Result<(usize, usize)> {
        let guard = super::lock_conn(self.conn)?;
        let pc_n = guard
            .execute(
                "DELETE FROM pending_confirms WHERE rollout_id = ?1 AND hostname = ?2",
                params![rollout_id, hostname],
            )
            .context("delete pending_confirms for reverted host")?;
        let hrs_n = guard
            .execute(
                "DELETE FROM host_rollout_state WHERE rollout_id = ?1 AND hostname = ?2",
                params![rollout_id, hostname],
            )
            .context("delete host_rollout_state for reverted host")?;
        Ok((pc_n, hrs_n))
    }

    /// True iff any `host_rollout_state` row exists for the given
    /// (rollout_id, hostname). Used by the soak-state recovery path
    /// to avoid overwriting existing host state when the agent's
    /// attestation arrives — an existing row reflects the actual
    /// lifecycle (Healthy/Soaked/Reverted/...) and is more
    /// authoritative than a re-attestation.
    pub fn host_rollout_state_exists(&self, hostname: &str, rollout_id: &str) -> Result<bool> {
        let guard = super::lock_conn(self.conn)?;
        let n: i64 = guard
            .query_row(
                "SELECT COUNT(*) FROM host_rollout_state
                 WHERE rollout_id = ?1 AND hostname = ?2",
                params![rollout_id, hostname],
                |row| row.get(0),
            )
            .context("count host_rollout_state")?;
        Ok(n > 0)
    }

    /// Currently-Healthy hosts in `rollout_id` and the timestamp
    /// they entered Healthy. The reconciler reads this to compute
    /// `now - last_healthy_since >= wave.soak_minutes`. Excludes
    /// rows whose `last_healthy_since` is NULL.
    pub fn host_soak_state_for_rollout(
        &self,
        rollout_id: &str,
    ) -> Result<HashMap<String, DateTime<Utc>>> {
        let guard = super::lock_conn(self.conn)?;
        let mut stmt = guard.prepare(
            "SELECT hostname, last_healthy_since
             FROM host_rollout_state
             WHERE rollout_id = ?1
               AND last_healthy_since IS NOT NULL",
        )?;
        let rows = stmt
            .query_map(params![rollout_id], |row| {
                let hostname: String = row.get(0)?;
                let ts: String = row.get(1)?;
                Ok((hostname, ts))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut out = HashMap::with_capacity(rows.len());
        for (hostname, ts) in rows {
            let parsed = ts
                .parse::<DateTime<Utc>>()
                .with_context(|| format!("parse last_healthy_since for {hostname}"))?;
            out.insert(hostname, parsed);
        }
        Ok(out)
    }

    /// Rollouts in which `hostname` is currently Healthy, paired
    /// with each rollout's `target_closure_hash` (joined from
    /// `pending_confirms`). The checkin handler calls this on every
    /// `/v1/agent/checkin` to detect the "left Healthy" case: if
    /// the host's reported `current_generation.closure_hash` no
    /// longer matches the rollout's target, the host has reverted
    /// away and the Healthy marker must be cleared.
    ///
    /// Joining avoids denormalising the target closure — a
    /// confirmed pending_confirms row always exists for any
    /// (rollout, host) in host_rollout_state, since the confirm-
    /// handler-success path's `transition_host_state` is the only
    /// emitter of Healthy rows. `DISTINCT` collapses the rare case
    /// of multiple confirmed rows for the same (host, rollout); the
    /// target closure is rollout-deterministic so any one is fine.
    pub fn healthy_rollouts_for_host(&self, hostname: &str) -> Result<Vec<(String, String)>> {
        let guard = super::lock_conn(self.conn)?;
        let mut stmt = guard.prepare(
            "SELECT DISTINCT hrs.rollout_id, pc.target_closure_hash
             FROM host_rollout_state hrs
             JOIN pending_confirms pc
               ON pc.hostname = hrs.hostname
              AND pc.rollout_id = hrs.rollout_id
             WHERE hrs.hostname = ?1
               AND hrs.last_healthy_since IS NOT NULL
               AND pc.state = ?2",
        )?;
        let rows = stmt
            .query_map(
                params![hostname, PendingConfirmState::Confirmed.as_db_str()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Rollouts the host is currently `Failed` on. RFC-0002 §5.1
    /// `rollback-and-halt` policy needs (rollout_id, target_ref) at
    /// checkin time so the agent can be told what failed target to
    /// step away from. Joined with `pending_confirms` for the
    /// target_channel_ref; multiple rows per rollout are collapsed
    /// via `DISTINCT` (the target_ref is rollout-deterministic).
    pub fn failed_rollouts_for_host(&self, hostname: &str) -> Result<Vec<(String, String)>> {
        let guard = super::lock_conn(self.conn)?;
        let mut stmt = guard.prepare(
            "SELECT DISTINCT hrs.rollout_id, pc.target_channel_ref
             FROM host_rollout_state hrs
             JOIN pending_confirms pc
               ON pc.hostname = hrs.hostname
              AND pc.rollout_id = hrs.rollout_id
             WHERE hrs.hostname = ?1
               AND hrs.host_state = ?2",
        )?;
        let rows = stmt
            .query_map(
                params![hostname, HostRolloutState::Failed.as_db_str()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Snapshot the active rollouts derived from the DB for the
    /// observed-state projection. For each (rollout_id, hostname),
    /// keep only the latest `pending_confirms` row by
    /// `dispatched_at`, restricted to
    /// `state IN ('pending', 'confirmed')`. LEFT JOIN
    /// `host_rollout_state` for the per-host machine state +
    /// soak-timer marker.
    ///
    /// Filtering out `rolled-back` / `cancelled` rows is load-
    /// bearing: a rollout whose every row is dead would otherwise
    /// surface as an empty `host_states` map, and the reconciler
    /// defaults absent host-state lookups to "Queued" — which means
    /// it would try to re-dispatch all those hosts. Skipping the
    /// dead rollouts entirely avoids that trap.
    ///
    /// Output order: rollout_id ascending. Deterministic so
    /// projection tests can compare against expected vectors and
    /// the reconciler's journal lines stay grep-stable.
    ///
    /// Lives on `RolloutState` rather than `Confirms` because the
    /// post-confirm machine (Healthy/Soaked/...) is what the
    /// observed-state projection actually publishes; the join into
    /// `pending_confirms` is bookkeeping for the target-closure
    /// columns.
    pub fn active_rollouts_snapshot(&self) -> Result<Vec<RolloutDbSnapshot>> {
        use std::collections::BTreeMap;

        let guard = super::lock_conn(self.conn)?;
        let mut stmt = guard.prepare(
            "WITH latest_per_host AS (
                 SELECT pc.rollout_id, pc.channel, pc.hostname,
                        pc.target_closure_hash, pc.target_channel_ref,
                        pc.state AS pc_state
                 FROM pending_confirms pc
                 WHERE pc.state IN (?1, ?2)
                   AND pc.dispatched_at = (
                     SELECT MAX(p2.dispatched_at)
                     FROM pending_confirms p2
                     WHERE p2.rollout_id = pc.rollout_id
                       AND p2.hostname = pc.hostname
                   )
             )
             SELECT lph.rollout_id, lph.channel, lph.hostname,
                    lph.target_closure_hash, lph.target_channel_ref,
                    lph.pc_state,
                    hrs.host_state, hrs.last_healthy_since
             FROM latest_per_host lph
             LEFT JOIN host_rollout_state hrs
                    ON hrs.rollout_id = lph.rollout_id
                   AND hrs.hostname = lph.hostname
             ORDER BY lph.rollout_id, lph.hostname",
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
                        row.get::<_, String>(1)?,         // channel (V005)
                        row.get::<_, String>(2)?,         // hostname
                        row.get::<_, String>(3)?,         // target_closure_hash
                        row.get::<_, String>(4)?,         // target_channel_ref
                        row.get::<_, String>(5)?,         // pc_state
                        row.get::<_, Option<String>>(6)?, // hrs.host_state
                        row.get::<_, Option<String>>(7)?, // hrs.last_healthy_since
                    ))
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        // BTreeMap keeps rollout_ids ordered without an extra sort.
        let mut by_rollout: BTreeMap<String, RolloutDbSnapshot> = BTreeMap::new();
        for (
            rollout_id,
            row_channel,
            hostname,
            target_closure,
            target_ref,
            pc_state,
            hrs_state,
            hrs_ts,
        ) in rows
        {
            // Derive the host's state literal. `host_rollout_state`
            // wins when present (post-confirm machine: Healthy /
            // Soaked / …); otherwise infer from `pending_confirms.state`.
            // Both arms route through `HostRolloutState`'s typed
            // accessors so any schema drift fails loudly here.
            // The RolledBack/Cancelled match guard is unreachable —
            // the CTE's WHERE pc.state IN ('pending','confirmed')
            // filters those out (see V002 migration).
            let host_state = match hrs_state {
                Some(s) => HostRolloutState::from_db_str(&s)?.as_db_str().to_string(),
                None => match PendingConfirmState::from_db_str(&pc_state)? {
                    PendingConfirmState::Pending => HostRolloutState::ConfirmWindow,
                    PendingConfirmState::Confirmed => HostRolloutState::Healthy,
                    PendingConfirmState::RolledBack | PendingConfirmState::Cancelled => {
                        unreachable!("filtered by CTE WHERE pc.state IN ('pending','confirmed')")
                    }
                }
                .as_db_str()
                .to_string(),
            };

            // V005 introduced an explicit `channel` column. Use it
            // when populated; fall back to legacy parsing of the
            // `<channel>@<short-ci-commit>` form for rows that
            // pre-date the migration's backfill (the same forensic
            // shape compute_rollout_id_for_channel emitted before #62).
            // If both fail, leave empty — the reconciler then emits
            // ChannelUnknown legitimately (drift detector intent).
            // See #80.
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

#[cfg(test)]
mod tests {
    use super::super::test_helpers::{fresh_db, mark_healthy, pc_insert};
    use crate::state::{HealthyMarker, HostRolloutState};
    use chrono::Utc;

    #[test]
    fn transition_to_healthy_round_trips() {
        let db = fresh_db();
        let now = Utc::now();
        mark_healthy(&db, "test-host", "stable@abc12345", now);
        let map = db
            .rollout_state()
            .host_soak_state_for_rollout("stable@abc12345")
            .unwrap();
        assert_eq!(map.len(), 1, "expected one Healthy host: {map:?}");
        let stored = map.get("test-host").expect("test-host present");
        // RFC3339 round-trip drops sub-second precision.
        assert_eq!(stored.timestamp(), now.timestamp());
    }

    #[test]
    fn transition_to_healthy_upserts_timestamp() {
        // Re-recording Healthy moves last_healthy_since forward
        // without breaking the row. The reconciler arm relies on the
        // latest Healthy entry winning so the soak timer always
        // reflects the most recent Healthy moment.
        let db = fresh_db();
        let t1 = Utc::now() - chrono::Duration::seconds(120);
        let t2 = Utc::now();
        mark_healthy(&db, "test-host", "stable@r1", t1);
        mark_healthy(&db, "test-host", "stable@r1", t2);
        let map = db
            .rollout_state()
            .host_soak_state_for_rollout("stable@r1")
            .unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(
            map["test-host"].timestamp(),
            t2.timestamp(),
            "second Healthy upsert must overwrite first"
        );
    }

    #[test]
    fn clear_healthy_marker_nulls_timestamp() {
        let db = fresh_db();
        mark_healthy(&db, "test-host", "stable@r1", Utc::now());
        let n = db
            .rollout_state()
            .clear_healthy_marker("test-host", "stable@r1")
            .unwrap();
        assert_eq!(n, 1);
        let map = db
            .rollout_state()
            .host_soak_state_for_rollout("stable@r1")
            .unwrap();
        assert!(
            map.is_empty(),
            "cleared host must drop out of soak state: {map:?}"
        );
    }

    #[test]
    fn clear_healthy_marker_is_noop_when_already_clear() {
        // Idempotent: calling clear on a row whose marker is
        // already NULL — or on a row that doesn't exist — returns 0
        // and does not fail. The checkin handler may emit clear
        // every checkin while the host stays diverged.
        let db = fresh_db();
        let n = db
            .rollout_state()
            .clear_healthy_marker("test-host", "stable@r1")
            .unwrap();
        assert_eq!(n, 0, "clear on missing row is no-op");
        mark_healthy(&db, "test-host", "stable@r1", Utc::now());
        assert_eq!(
            db.rollout_state()
                .clear_healthy_marker("test-host", "stable@r1")
                .unwrap(),
            1
        );
        // Second clear: row exists, marker already NULL.
        assert_eq!(
            db.rollout_state()
                .clear_healthy_marker("test-host", "stable@r1")
                .unwrap(),
            0
        );
    }

    #[test]
    fn host_soak_state_scopes_to_rollout() {
        // Two rollouts, two hosts each — the projection must
        // return only the requested rollout's hosts.
        let db = fresh_db();
        let now = Utc::now();
        mark_healthy(&db, "ohm", "stable@r1", now);
        mark_healthy(&db, "krach", "stable@r1", now);
        mark_healthy(&db, "pixel", "edge@r2", now);

        let r1 = db
            .rollout_state()
            .host_soak_state_for_rollout("stable@r1")
            .unwrap();
        assert_eq!(r1.len(), 2);
        assert!(r1.contains_key("ohm"));
        assert!(r1.contains_key("krach"));

        let r2 = db
            .rollout_state()
            .host_soak_state_for_rollout("edge@r2")
            .unwrap();
        assert_eq!(r2.len(), 1);
        assert!(r2.contains_key("pixel"));
    }

    #[test]
    fn healthy_rollouts_for_host_joins_pending_confirms() {
        // The checkin handler calls this to compare reported
        // current_generation against each rollout's target. The
        // join requires a confirmed pending_confirms row — an
        // un-confirmed (still 'pending') row must NOT surface,
        // since the host has not yet reached Healthy.
        let db = fresh_db();
        let future = Utc::now() + chrono::Duration::seconds(120);
        db.confirms()
            .record_pending_confirm(&pc_insert(
                "test-host",
                "stable@r1",
                "target-system-r1",
                future,
            ))
            .unwrap();
        // Still pending — healthy_rollouts_for_host must be empty
        // even after recording Healthy (the row exists but the
        // join filter is pc.state = 'confirmed').
        mark_healthy(&db, "test-host", "stable@r1", Utc::now());
        let pre = db
            .rollout_state()
            .healthy_rollouts_for_host("test-host")
            .unwrap();
        assert!(
            pre.is_empty(),
            "must not surface rollouts whose pending_confirms is still pending: {pre:?}"
        );

        // Confirm it; now the join hits.
        let n = db
            .confirms()
            .confirm_pending("test-host", "stable@r1")
            .unwrap();
        assert_eq!(n, 1);
        let post = db
            .rollout_state()
            .healthy_rollouts_for_host("test-host")
            .unwrap();
        assert_eq!(post.len(), 1);
        assert_eq!(post[0].0, "stable@r1");
        assert_eq!(post[0].1, "target-system-r1");
    }

    #[test]
    fn healthy_rollouts_for_host_excludes_cleared_rows() {
        let db = fresh_db();
        let future = Utc::now() + chrono::Duration::seconds(120);
        db.confirms()
            .record_pending_confirm(&pc_insert(
                "test-host",
                "stable@r1",
                "target-system-r1",
                future,
            ))
            .unwrap();
        db.confirms()
            .confirm_pending("test-host", "stable@r1")
            .unwrap();
        mark_healthy(&db, "test-host", "stable@r1", Utc::now());
        assert_eq!(
            db.rollout_state()
                .healthy_rollouts_for_host("test-host")
                .unwrap()
                .len(),
            1
        );

        // After clear_healthy_marker, the row falls out — it's no
        // longer Healthy, so checkin doesn't need to re-clear.
        db.rollout_state()
            .clear_healthy_marker("test-host", "stable@r1")
            .unwrap();
        assert!(db
            .rollout_state()
            .healthy_rollouts_for_host("test-host")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn transition_to_soaked_only_from_healthy() {
        // SoakHost handler: only Healthy → Soaked is valid. The
        // guarded UPDATE shape encodes that as
        // `expected_from = Some(Healthy)`.
        let db = fresh_db();
        let to_soaked = |db: &super::super::Db, host: &str, rollout: &str| {
            db.rollout_state()
                .transition_host_state(
                    host,
                    rollout,
                    HostRolloutState::Soaked,
                    HealthyMarker::Untouched,
                    Some(HostRolloutState::Healthy),
                )
                .unwrap()
        };
        // No row → no-op.
        assert_eq!(to_soaked(&db, "ohm", "stable@r1"), 0);
        // Healthy → Soaked.
        mark_healthy(&db, "ohm", "stable@r1", Utc::now());
        assert_eq!(to_soaked(&db, "ohm", "stable@r1"), 1);
        // Already Soaked → idempotent no-op (the WHERE filter
        // guards the transition).
        assert_eq!(to_soaked(&db, "ohm", "stable@r1"), 0);

        // Verify the active-rollout snapshot reflects the
        // transition. Need a confirmed pending_confirms row to
        // pass the snapshot's join filter.
        let future = Utc::now() + chrono::Duration::seconds(120);
        db.confirms()
            .record_pending_confirm(&pc_insert("ohm", "stable@r1", "target", future))
            .unwrap();
        db.confirms().confirm_pending("ohm", "stable@r1").unwrap();
        let snap = db.rollout_state().active_rollouts_snapshot().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(
            snap[0].host_states.get("ohm").map(String::as_str),
            Some("Soaked"),
        );
    }

    #[test]
    fn active_rollouts_snapshot_empty_when_no_rows() {
        let db = fresh_db();
        let snap = db.rollout_state().active_rollouts_snapshot().unwrap();
        assert!(snap.is_empty());
    }

    #[test]
    fn active_rollouts_snapshot_pending_surfaces_as_confirmwindow() {
        // Dispatch happened but agent has not confirmed yet. The
        // host appears in the rollout with state "ConfirmWindow"
        // (RFC §3.2) and no last_healthy_since marker.
        let db = fresh_db();
        let future = Utc::now() + chrono::Duration::seconds(120);
        db.confirms()
            .record_pending_confirm(&pc_insert("ohm", "stable@abc12345", "system-r1", future))
            .unwrap();

        let snap = db.rollout_state().active_rollouts_snapshot().unwrap();
        assert_eq!(snap.len(), 1);
        let r = &snap[0];
        assert_eq!(r.rollout_id, "stable@abc12345");
        assert_eq!(r.channel, "stable");
        assert_eq!(r.target_closure_hash, "system-r1");
        assert_eq!(r.target_channel_ref, "stable@abc12345");
        assert_eq!(
            r.host_states.get("ohm").map(String::as_str),
            Some("ConfirmWindow")
        );
        assert!(r.last_healthy_since.is_empty());
    }

    #[test]
    fn active_rollouts_snapshot_uses_explicit_channel_for_sha_rollout_id() {
        // #80 regression guard. Post-#62 rolloutIds are sha256 hex
        // strings with no `@` separator. Without the V005 channel
        // column, the snapshot fell through to `rollout_id.clone()`
        // and the reconciler then emitted ChannelUnknown with the
        // SHA as the bogus channel name. Asserts that an explicit
        // channel column wins for sha-shaped ids.
        let db = fresh_db();
        let future = Utc::now() + chrono::Duration::seconds(120);
        let sha_rollout = "1111111111111111111111111111111111111111111111111111111111111111";
        let mut row = pc_insert("ohm", sha_rollout, "system-r1", future);
        row.channel = "edge-slow";
        db.confirms().record_pending_confirm(&row).unwrap();

        let snap = db.rollout_state().active_rollouts_snapshot().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(
            snap[0].channel, "edge-slow",
            "snapshot must read channel from V005 column, not the rolloutId fallback",
        );
    }

    #[test]
    fn active_rollouts_snapshot_falls_back_to_legacy_split_when_channel_empty() {
        // V005 backfill covers legacy `<channel>@<ref>` rows, but a
        // sha-shaped row without a backfilled channel must NOT
        // surface a SHA-as-channel; it should leave the channel
        // empty so the reconciler emits ChannelUnknown legitimately
        // (drift detector intent preserved).
        let db = fresh_db();
        // Synthesize a row directly so we can exercise the empty-
        // channel fallback. record_pending_confirm requires non-empty
        // channel by API contract; the migration backfill covers the
        // historical legacy gap.
        let future = Utc::now() + chrono::Duration::seconds(120);
        let mut row = pc_insert("ohm", "stable@abc12345", "system-r1", future);
        row.channel = ""; // simulate a pre-V005 row that the backfill missed
        db.confirms().record_pending_confirm(&row).unwrap();

        let snap = db.rollout_state().active_rollouts_snapshot().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(
            snap[0].channel, "stable",
            "legacy <channel>@<ref> rolloutIds must still resolve via split fallback",
        );
    }

    #[test]
    fn active_rollouts_snapshot_confirmed_uses_host_rollout_state() {
        // Once confirm lands, host_rollout_state.host_state takes
        // precedence (matches the path the production handlers
        // write). last_healthy_since surfaces in the side map for
        // the soak gate.
        let db = fresh_db();
        let future = Utc::now() + chrono::Duration::seconds(120);
        let now = Utc::now();
        db.confirms()
            .record_pending_confirm(&pc_insert("ohm", "stable@abc12345", "system-r1", future))
            .unwrap();
        db.confirms()
            .confirm_pending("ohm", "stable@abc12345")
            .unwrap();
        mark_healthy(&db, "ohm", "stable@abc12345", now);

        let snap = db.rollout_state().active_rollouts_snapshot().unwrap();
        assert_eq!(snap.len(), 1);
        let r = &snap[0];
        assert_eq!(
            r.host_states.get("ohm").map(String::as_str),
            Some("Healthy")
        );
        let stored = r
            .last_healthy_since
            .get("ohm")
            .expect("Healthy host has soak ts");
        assert_eq!(stored.timestamp(), now.timestamp());
    }

    #[test]
    fn active_rollouts_snapshot_filters_rolled_back_rollouts() {
        // The rollback timer marked the row 'rolled-back'. The
        // rollout has no other surviving rows, so it must NOT
        // appear in active_rollouts — otherwise its empty
        // host_states map would default to "Queued" in the
        // reconciler and trigger spurious re-dispatches.
        let db = fresh_db();
        let past = Utc::now() - chrono::Duration::seconds(120);
        db.confirms()
            .record_pending_confirm(&pc_insert("ohm", "stable@dead", "system-x", past))
            .unwrap();
        let expired = db.confirms().pending_confirms_expired().unwrap();
        let ids: Vec<i64> = expired.iter().map(|(id, _, _, _, _)| *id).collect();
        db.confirms().mark_rolled_back(&ids).unwrap();

        let snap = db.rollout_state().active_rollouts_snapshot().unwrap();
        assert!(
            snap.is_empty(),
            "rolled-back rollouts must not surface in active_rollouts: {snap:?}",
        );
    }

    #[test]
    fn active_rollouts_snapshot_groups_hosts_per_rollout() {
        // Two rollouts, two hosts each, mixed states. Each rollout
        // appears once with its hosts grouped under host_states.
        let db = fresh_db();
        let future = Utc::now() + chrono::Duration::seconds(120);
        for (host, rollout) in [
            ("ohm", "stable@r1"),
            ("krach", "stable@r1"),
            ("pixel", "edge@r2"),
            ("aether", "edge@r2"),
        ] {
            db.confirms()
                .record_pending_confirm(&pc_insert(host, rollout, "target", future))
                .unwrap();
        }
        // ohm + pixel confirm; krach + aether stay in ConfirmWindow.
        db.confirms().confirm_pending("ohm", "stable@r1").unwrap();
        db.confirms().confirm_pending("pixel", "edge@r2").unwrap();
        mark_healthy(&db, "ohm", "stable@r1", Utc::now());
        mark_healthy(&db, "pixel", "edge@r2", Utc::now());

        let snap = db.rollout_state().active_rollouts_snapshot().unwrap();
        assert_eq!(snap.len(), 2);
        // BTreeMap-ordered: edge@r2 sorts before stable@r1.
        assert_eq!(snap[0].rollout_id, "edge@r2");
        assert_eq!(snap[0].host_states.len(), 2);
        assert_eq!(
            snap[0].host_states.get("pixel").map(String::as_str),
            Some("Healthy"),
        );
        assert_eq!(
            snap[0].host_states.get("aether").map(String::as_str),
            Some("ConfirmWindow"),
        );
        assert_eq!(snap[0].last_healthy_since.len(), 1);
        assert!(snap[0].last_healthy_since.contains_key("pixel"));

        assert_eq!(snap[1].rollout_id, "stable@r1");
        assert_eq!(
            snap[1].host_states.get("ohm").map(String::as_str),
            Some("Healthy"),
        );
        assert_eq!(
            snap[1].host_states.get("krach").map(String::as_str),
            Some("ConfirmWindow"),
        );
    }

    #[test]
    fn active_rollouts_snapshot_picks_latest_pending_confirm_per_host() {
        // Re-dispatches accumulate pending_confirms rows for the
        // same (host, rollout). The snapshot must reflect the most
        // recent dispatch — older rolled-back rows must not shadow
        // a fresh pending row.
        let db = fresh_db();
        // First dispatch: past deadline, expires + rolls back.
        let past = Utc::now() - chrono::Duration::seconds(120);
        db.confirms()
            .record_pending_confirm(&pc_insert("ohm", "stable@r1", "old", past))
            .unwrap();
        let expired = db.confirms().pending_confirms_expired().unwrap();
        let ids: Vec<i64> = expired.iter().map(|(id, _, _, _, _)| *id).collect();
        db.confirms().mark_rolled_back(&ids).unwrap();

        // Second dispatch with a fresh deadline.
        let future = Utc::now() + chrono::Duration::seconds(120);
        db.confirms()
            .record_pending_confirm(&pc_insert("ohm", "stable@r1", "new", future))
            .unwrap();

        let snap = db.rollout_state().active_rollouts_snapshot().unwrap();
        assert_eq!(snap.len(), 1);
        // Filtered to state IN ('pending','confirmed'), so only
        // the second row matters and its target is "new".
        assert_eq!(snap[0].target_closure_hash, "new");
        assert_eq!(
            snap[0].host_states.get("ohm").map(String::as_str),
            Some("ConfirmWindow"),
        );
    }

    #[test]
    fn delete_rollout_records_clears_both_tables() {
        let db = fresh_db();
        let future = Utc::now() + chrono::Duration::seconds(120);

        // Two rollouts, two hosts each, all confirmed + Soaked.
        for (rollout, host) in [
            ("stable@conv1", "ohm"),
            ("stable@conv1", "krach"),
            ("stable@active", "pixel"),
            ("stable@active", "aether"),
        ] {
            let mut row = pc_insert(host, rollout, "system-r", future);
            row.channel = "stable";
            db.confirms().record_pending_confirm(&row).unwrap();
            db.rollout_state()
                .transition_host_state(
                    host,
                    rollout,
                    HostRolloutState::Soaked,
                    HealthyMarker::Untouched,
                    None,
                )
                .unwrap();
        }
        assert_eq!(db.rollout_state().active_rollouts_snapshot().unwrap().len(), 2);

        // Cleanup the converged one.
        let (pc_n, hrs_n) = db
            .rollout_state()
            .delete_rollout_records("stable@conv1")
            .unwrap();
        assert_eq!(pc_n, 2, "two pending_confirms rows for the converged rollout");
        assert_eq!(hrs_n, 2, "two host_rollout_state rows for the converged rollout");

        // Active rollout untouched.
        let snap = db.rollout_state().active_rollouts_snapshot().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].rollout_id, "stable@active");

        // Re-running is a no-op.
        let (pc_n, hrs_n) = db
            .rollout_state()
            .delete_rollout_records("stable@conv1")
            .unwrap();
        assert_eq!(pc_n, 0);
        assert_eq!(hrs_n, 0);
    }
}
