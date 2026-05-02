//! `host_rollout_state` — per-host soak markers + state machine.
//!
//! Recovery class: **soft state** (ARCHITECTURE.md §6 Phase 10).
//! Loss restarts soak windows from zero. Mitigated by agent-attested
//! `last_confirmed_at` (#47): the agent persists the moment of its
//! most recent successful confirm and echoes it on every checkin;
//! the CP repopulates `last_healthy_since` from the attestation,
//! clamped to `min(now, attested)`.
//!
//! The joined `active_rollouts_snapshot` projection lives on
//! [`super::host_dispatch_state`] alongside the operational table
//! it reads from; this module owns just the per-(host, rollout) soak
//! state.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::sync::Mutex;

use crate::state::{HealthyMarker, HostRolloutState, PendingConfirmState};

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
    /// `host_dispatch_state`). The checkin handler calls this on
    /// every `/v1/agent/checkin` to detect the "left Healthy" case:
    /// if the host's reported `current_generation.closure_hash` no
    /// longer matches the rollout's target, the host has reverted
    /// away and the Healthy marker must be cleared.
    ///
    /// Joining against the operational table avoids denormalising
    /// the target closure. Operational state is `'confirmed'` for
    /// any (rollout, host) where Healthy is the post-confirm machine
    /// state — the confirm handler is the only emitter of Healthy
    /// rows.
    pub fn healthy_rollouts_for_host(&self, hostname: &str) -> Result<Vec<(String, String)>> {
        let guard = super::lock_conn(self.conn)?;
        let mut stmt = guard.prepare(
            "SELECT hrs.rollout_id, hds.target_closure_hash
             FROM host_rollout_state hrs
             JOIN host_dispatch_state hds
               ON hds.hostname = hrs.hostname
              AND hds.rollout_id = hrs.rollout_id
             WHERE hrs.hostname = ?1
               AND hrs.last_healthy_since IS NOT NULL
               AND hds.state = ?2",
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
    /// step away from. Joined with `host_dispatch_state` for the
    /// target_channel_ref.
    pub fn failed_rollouts_for_host(&self, hostname: &str) -> Result<Vec<(String, String)>> {
        let guard = super::lock_conn(self.conn)?;
        let mut stmt = guard.prepare(
            "SELECT hrs.rollout_id, hds.target_channel_ref
             FROM host_rollout_state hrs
             JOIN host_dispatch_state hds
               ON hds.hostname = hrs.hostname
              AND hds.rollout_id = hrs.rollout_id
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

}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::{dispatch_insert, fresh_db, mark_healthy};
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
    fn healthy_rollouts_for_host_joins_dispatch_state() {
        // The checkin handler calls this to compare reported
        // current_generation against each rollout's target. The
        // join requires a confirmed host_dispatch_state row — a
        // still-'pending' row must NOT surface, since the host has
        // not yet reached Healthy.
        let db = fresh_db();
        let future = Utc::now() + chrono::Duration::seconds(120);
        db.host_dispatch_state()
            .record_dispatch(&dispatch_insert(
                "test-host",
                "stable@r1",
                "target-system-r1",
                future,
            ))
            .unwrap();
        // Still pending — healthy_rollouts_for_host must be empty
        // even after recording Healthy (the join filter is
        // hds.state = 'confirmed').
        mark_healthy(&db, "test-host", "stable@r1", Utc::now());
        let pre = db
            .rollout_state()
            .healthy_rollouts_for_host("test-host")
            .unwrap();
        assert!(
            pre.is_empty(),
            "must not surface rollouts whose operational row is still pending: {pre:?}"
        );

        // Confirm it; now the join hits.
        let n = db.host_dispatch_state().confirm("test-host", "stable@r1").unwrap();
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
        db.host_dispatch_state()
            .record_dispatch(&dispatch_insert(
                "test-host",
                "stable@r1",
                "target-system-r1",
                future,
            ))
            .unwrap();
        db.host_dispatch_state().confirm("test-host", "stable@r1").unwrap();
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
        // transition. Need a confirmed operational row to pass the
        // snapshot's join filter.
        let future = Utc::now() + chrono::Duration::seconds(120);
        db.host_dispatch_state()
            .record_dispatch(&dispatch_insert("ohm", "stable@r1", "target", future))
            .unwrap();
        db.host_dispatch_state().confirm("ohm", "stable@r1").unwrap();
        let snap = db.host_dispatch_state().active_rollouts_snapshot().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(
            snap[0].host_states.get("ohm").map(String::as_str),
            Some("Soaked"),
        );
    }
}
