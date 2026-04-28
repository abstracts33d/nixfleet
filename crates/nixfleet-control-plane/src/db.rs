//! SQLite persistence for the control plane.
//!
//! Built on the rusqlite + refinery stack with WAL + FK posture.
//! The schema lives under `migrations/` and grows additively.
//!
//! Concurrency: a `Mutex<Connection>` guards a single SQLite
//! connection. SQLite scales fine for fleet sizes O(100) under WAL;
//! a connection pool is unnecessary. Mutex poisoning is converted
//! to anyhow errors instead of panicking.
//!
//! All schema-modifying operations go through `migrate()` which
//! refinery makes idempotent + version-tracked.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

mod embedded {
    use refinery::embed_migrations;
    embed_migrations!("migrations");
}

/// One active rollout's worth of state, joined from
/// `pending_confirms` + `host_rollout_state`. Returned by
/// [`Db::active_rollouts_snapshot`] for the observed-state
/// projection (step 2 of gap #2 in
/// docs/roadmap/0002-v0.2-completeness-gaps.md). The CP keeps no
/// dedicated rollouts table yet (the migration in V002 flagged a
/// follow-up); rollouts are derived from the union of dispatched
/// targets + per-host soak markers, scoped to those still in
/// `pending` or `confirmed` state — `rolled-back` and `cancelled`
/// rows are filtered out so dead rollouts don't surface as empty-
/// host-states bundles that the reconciler would treat as fresh
/// Queued work.
#[derive(Debug, Clone)]
pub struct RolloutDbSnapshot {
    pub rollout_id: String,
    pub channel: String,
    pub target_closure_hash: String,
    pub target_channel_ref: String,
    /// hostname → RFC-0002 §3.2 state name. `host_rollout_state`
    /// wins when present (carries Soaked / Converged / Failed once
    /// step 3 lands). Otherwise derived from the latest
    /// `pending_confirms.state` for that (rollout, host).
    pub host_states: HashMap<String, String>,
    /// hostname → moment the host most recently entered Healthy.
    /// Excludes hosts whose marker is NULL (not currently Healthy).
    pub last_healthy_since: HashMap<String, DateTime<Utc>>,
}

/// SQLite-backed CP persistence.
pub struct Db {
    conn: Mutex<Connection>,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db").field("conn", &"<sqlite>").finish()
    }
}

impl Db {
    /// Open (or create) the SQLite database at `path`. Creates parent
    /// directories as needed. Enables WAL + FK on the connection
    /// before any migrations run.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create dir {}", parent.display()))?;
            }
        }
        let conn = Connection::open(path)
            .with_context(|| format!("open sqlite {}", path.display()))?;

        // WAL improves concurrent read performance; FK enforces
        // referential integrity that the schema declares.
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .context("set sqlite pragmas")?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open a fresh in-memory database. Used by tests.
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("open sqlite :memory:")?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|e| anyhow::anyhow!("db lock poisoned: {e}"))
    }

    /// Run all pending migrations. Idempotent under refinery —
    /// previously-applied migrations are skipped.
    pub fn migrate(&self) -> Result<()> {
        let mut guard = self.conn()?;
        embedded::migrations::runner()
            .run(&mut *guard)
            .context("run sqlite migrations")?;
        Ok(())
    }

    // =================================================================
    // token_replay — bootstrap-token nonces
    // =================================================================

    /// True iff `nonce` was previously recorded.
    pub fn token_seen(&self, nonce: &str) -> Result<bool> {
        let guard = self.conn()?;
        let exists: bool = guard
            .query_row(
                "SELECT 1 FROM token_replay WHERE nonce = ?1",
                params![nonce],
                |_| Ok(true),
            )
            .or_else(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => Ok(false),
                e => Err(e),
            })
            .context("query token_replay")?;
        Ok(exists)
    }

    /// Record `nonce` as seen. No-op if the nonce already exists
    /// (caller is expected to check `token_seen` first if it cares;
    /// this is just `INSERT OR IGNORE`).
    pub fn record_token_nonce(&self, nonce: &str, hostname: &str) -> Result<()> {
        let guard = self.conn()?;
        guard
            .execute(
                "INSERT OR IGNORE INTO token_replay(nonce, hostname) VALUES (?1, ?2)",
                params![nonce, hostname],
            )
            .context("insert token_replay")?;
        Ok(())
    }

    /// Drop replay records older than `max_age` (typical: 24h, the
    /// token validity window). Returns the number of pruned rows.
    /// A periodic background task invokes this.
    pub fn prune_token_replay(&self, max_age_hours: i64) -> Result<usize> {
        let guard = self.conn()?;
        let n = guard
            .execute(
                "DELETE FROM token_replay
                 WHERE first_seen < datetime('now', ?1)",
                params![format!("-{max_age_hours} hours")],
            )
            .context("prune token_replay")?;
        Ok(n)
    }

    // =================================================================
    // cert_revocations — RFC-0003 §2
    // =================================================================

    /// Record a revocation: any cert for `hostname` with notBefore
    /// older than `not_before` is rejected at mTLS time. Upsert
    /// shape — revoking again moves the not_before forward.
    pub fn revoke_cert(
        &self,
        hostname: &str,
        not_before: DateTime<Utc>,
        reason: Option<&str>,
        revoked_by: Option<&str>,
    ) -> Result<()> {
        let guard = self.conn()?;
        guard
            .execute(
                "INSERT INTO cert_revocations(hostname, not_before, reason, revoked_by)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(hostname) DO UPDATE SET
                   not_before = excluded.not_before,
                   reason     = excluded.reason,
                   revoked_at = datetime('now'),
                   revoked_by = excluded.revoked_by",
                params![hostname, not_before.to_rfc3339(), reason, revoked_by],
            )
            .context("upsert cert_revocations")?;
        Ok(())
    }

    // ===============================================================
    // pending_confirms — RFC-0003 §4.2 activation confirmations
    // + magic rollback timer support
    // ===============================================================

    /// Record a dispatched activation. Called from the dispatch loop
    /// when CP populates `target` in a checkin response. The agent
    /// will later post `/v1/agent/confirm` with the same `rollout_id`
    /// once it boots the new closure.
    pub fn record_pending_confirm(
        &self,
        hostname: &str,
        rollout_id: &str,
        wave: u32,
        target_closure_hash: &str,
        target_channel_ref: &str,
        confirm_deadline: DateTime<Utc>,
    ) -> Result<i64> {
        let guard = self.conn()?;
        guard
            .execute(
                "INSERT INTO pending_confirms(hostname, rollout_id, wave,
                                              target_closure_hash,
                                              target_channel_ref,
                                              confirm_deadline)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    hostname,
                    rollout_id,
                    wave,
                    target_closure_hash,
                    target_channel_ref,
                    confirm_deadline.to_rfc3339()
                ],
            )
            .context("insert pending_confirms")?;
        Ok(guard.last_insert_rowid())
    }

    /// Returns true if the host has any `pending_confirms` row in
    /// state `'pending'`. Used by the dispatch loop to avoid
    /// re-dispatching while an activation is in flight (would create
    /// a duplicate row racing the first).
    pub fn pending_confirm_exists(&self, hostname: &str) -> Result<bool> {
        let guard = self.conn()?;
        let n: i64 = guard
            .query_row(
                "SELECT COUNT(*) FROM pending_confirms
                 WHERE hostname = ?1 AND state = 'pending'",
                params![hostname],
                |row| row.get(0),
            )
            .context("count pending_confirms")?;
        Ok(n > 0)
    }

    /// Mark a pending confirmation as confirmed. Called by the
    /// `/v1/agent/confirm` handler. Returns the number of rows
    /// updated — 0 means no matching pending row (could be: rollout
    /// cancelled, deadline already expired, or agent confirming
    /// twice). Caller decides on the response code.
    pub fn confirm_pending(&self, hostname: &str, rollout_id: &str) -> Result<usize> {
        let guard = self.conn()?;
        let n = guard
            .execute(
                "UPDATE pending_confirms
                 SET confirmed_at = datetime('now'),
                     state = 'confirmed'
                 WHERE hostname = ?1
                   AND rollout_id = ?2
                   AND state = 'pending'",
                params![hostname, rollout_id],
            )
            .context("update pending_confirms confirmed")?;
        Ok(n)
    }


    /// Pending confirms whose deadline has passed and which haven't
    /// been confirmed yet. Used by the magic-rollback timer task —
    /// each row returned is a host that failed to confirm in time
    /// and should be rolled back.
    ///
    /// Returns (id, hostname, rollout_id, wave, target_closure_hash).
    ///
    /// Wraps `confirm_deadline` in `datetime(...)` so SQLite parses the
    /// stored RFC3339 string (`YYYY-MM-DDTHH:MM:SS+00:00`, written by
    /// `chrono::DateTime::to_rfc3339`) into the same canonical
    /// `YYYY-MM-DD HH:MM:SS` shape that `datetime('now')` returns,
    /// before the `<` comparison. Naked string compare would put `T`
    /// (0x54) above ` ` (0x20) at position 10, so deadlines would
    /// always look greater than now — expired rows never matched and
    /// the rollback timer was a no-op. Caught on lab when a deadline
    /// passed by 50s while the row was still `pending`.
    pub fn pending_confirms_expired(&self) -> Result<Vec<(i64, String, String, u32, String)>> {
        let guard = self.conn()?;
        let mut stmt = guard.prepare(
            "SELECT id, hostname, rollout_id, wave, target_closure_hash
             FROM pending_confirms
             WHERE state = 'pending'
               AND datetime(confirm_deadline) < datetime('now')",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, u32>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Mark expired confirms as rolled-back. Called by the magic-
    /// rollback timer after `pending_confirms_expired` for the same
    /// IDs. Idempotent — only updates rows still in 'pending' state,
    /// so a second call with the same IDs is a no-op.
    pub fn mark_rolled_back(&self, ids: &[i64]) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let guard = self.conn()?;
        // SQLite IN clause via repeated `?` placeholders.
        let placeholders = (1..=ids.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "UPDATE pending_confirms
             SET state = 'rolled-back'
             WHERE state = 'pending' AND id IN ({placeholders})"
        );
        let mut stmt = guard.prepare(&sql)?;
        let n = stmt.execute(rusqlite::params_from_iter(ids.iter()))?;
        Ok(n)
    }

    // ===============================================================
    // host_rollout_state — RFC-0002 §3.2 / §4.4 soak timer support
    // ===============================================================

    /// Mark host as Healthy for `rollout_id`, stamping
    /// `last_healthy_since = now`. Step 3 of gap #2
    /// (docs/roadmap/0002-v0.2-completeness-gaps.md) — the
    /// reconciler arm — consults this against `wave.soak_minutes`
    /// to gate the Healthy → Soaked transition. UPSERT shape:
    /// re-entering Healthy after a clear refreshes the timestamp
    /// without resetting any other column.
    pub fn record_host_healthy(
        &self,
        hostname: &str,
        rollout_id: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let guard = self.conn()?;
        guard
            .execute(
                "INSERT INTO host_rollout_state(rollout_id, hostname,
                                                host_state,
                                                last_healthy_since,
                                                updated_at)
                 VALUES (?1, ?2, 'Healthy', ?3, datetime('now'))
                 ON CONFLICT(rollout_id, hostname) DO UPDATE SET
                   host_state = 'Healthy',
                   last_healthy_since = excluded.last_healthy_since,
                   updated_at = datetime('now')",
                params![rollout_id, hostname, now.to_rfc3339()],
            )
            .context("upsert host_rollout_state Healthy")?;
        Ok(())
    }

    /// Clear the Healthy marker for (rollout, host) — the host has
    /// left Healthy (its `current_generation.closure_hash` no
    /// longer matches the rollout's target). Nulls
    /// `last_healthy_since` so the soak timer must restart on the
    /// next Healthy entry. The `host_state` column is intentionally
    /// untouched: step 3's reconciler arm decides what state to
    /// transition to (typically back to ConfirmWindow, or Failed).
    /// Returns the number of rows updated — 0 means there was no
    /// Healthy marker to clear.
    pub fn clear_host_healthy(&self, hostname: &str, rollout_id: &str) -> Result<usize> {
        let guard = self.conn()?;
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

    /// Currently-Healthy hosts in `rollout_id` and the timestamp
    /// they entered Healthy. Step 2 of gap #2 (next session, the
    /// observed-state projection) reads this so the reconciler can
    /// compute `now - last_healthy_since >= wave.soak_minutes`.
    /// Excludes rows whose `last_healthy_since` is NULL.
    pub fn host_soak_state_for_rollout(
        &self,
        rollout_id: &str,
    ) -> Result<HashMap<String, DateTime<Utc>>> {
        let guard = self.conn()?;
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
    /// (rollout, host) in host_rollout_state, since
    /// `record_host_healthy` is only called from the confirm
    /// handler success path. `DISTINCT` collapses the rare case of
    /// multiple confirmed rows for the same (host, rollout); the
    /// target closure is rollout-deterministic so any one is fine.
    pub fn healthy_rollouts_for_host(&self, hostname: &str) -> Result<Vec<(String, String)>> {
        let guard = self.conn()?;
        let mut stmt = guard.prepare(
            "SELECT DISTINCT hrs.rollout_id, pc.target_closure_hash
             FROM host_rollout_state hrs
             JOIN pending_confirms pc
               ON pc.hostname = hrs.hostname
              AND pc.rollout_id = hrs.rollout_id
             WHERE hrs.hostname = ?1
               AND hrs.last_healthy_since IS NOT NULL
               AND pc.state = 'confirmed'",
        )?;
        let rows = stmt
            .query_map(params![hostname], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Snapshot the active rollouts derived from the DB for the
    /// observed-state projection (step 2 of gap #2). For each
    /// (rollout_id, hostname), keep only the latest
    /// `pending_confirms` row by `dispatched_at`, restricted to
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
    pub fn active_rollouts_snapshot(&self) -> Result<Vec<RolloutDbSnapshot>> {
        use std::collections::BTreeMap;

        let guard = self.conn()?;
        let mut stmt = guard.prepare(
            "WITH latest_per_host AS (
                 SELECT pc.rollout_id, pc.hostname,
                        pc.target_closure_hash, pc.target_channel_ref,
                        pc.state AS pc_state
                 FROM pending_confirms pc
                 WHERE pc.state IN ('pending', 'confirmed')
                   AND pc.dispatched_at = (
                     SELECT MAX(p2.dispatched_at)
                     FROM pending_confirms p2
                     WHERE p2.rollout_id = pc.rollout_id
                       AND p2.hostname = pc.hostname
                   )
             )
             SELECT lph.rollout_id, lph.hostname,
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
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?, // rollout_id
                    row.get::<_, String>(1)?, // hostname
                    row.get::<_, String>(2)?, // target_closure_hash
                    row.get::<_, String>(3)?, // target_channel_ref
                    row.get::<_, String>(4)?, // pc_state
                    row.get::<_, Option<String>>(5)?, // hrs.host_state
                    row.get::<_, Option<String>>(6)?, // hrs.last_healthy_since
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        // BTreeMap keeps rollout_ids ordered without an extra sort.
        let mut by_rollout: BTreeMap<String, RolloutDbSnapshot> = BTreeMap::new();
        for (rollout_id, hostname, target_closure, target_ref, pc_state, hrs_state, hrs_ts) in rows
        {
            // Derive the host's state name. host_rollout_state
            // wins when present — it carries the post-confirm
            // machine (Healthy/Soaked/...) that step 3 will write.
            // Otherwise infer from pending_confirms.state.
            let host_state = match hrs_state {
                Some(s) => s,
                None => match pc_state.as_str() {
                    // Dispatched but pre-confirm: agent is in the
                    // ConfirmWindow per RFC §3.2.
                    "pending" => "ConfirmWindow".to_string(),
                    // Defensive: should not happen — confirm
                    // handler upserts a host_rollout_state row on
                    // success. If it does (pre-existing data, or
                    // the upsert failed-but-confirm-succeeded
                    // window), surface as Healthy.
                    "confirmed" => "Healthy".to_string(),
                    // 'pending' / 'confirmed' are the only states
                    // surviving the WHERE filter; anything else
                    // here is a SQLite anomaly.
                    other => other.to_string(),
                },
            };

            let channel = rollout_id
                .split_once('@')
                .map(|(c, _)| c.to_string())
                .unwrap_or_else(|| rollout_id.clone());

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

    /// Return the most recent revocation `not_before` for `hostname`,
    /// or `None` if not revoked. Caller compares against the
    /// presented cert's notBefore at mTLS handshake time.
    pub fn cert_revoked_before(&self, hostname: &str) -> Result<Option<DateTime<Utc>>> {
        let guard = self.conn()?;
        let row: Result<String, _> = guard.query_row(
            "SELECT not_before FROM cert_revocations WHERE hostname = ?1",
            params![hostname],
            |r| r.get(0),
        );
        match row {
            Ok(s) => Ok(Some(s.parse::<DateTime<Utc>>().context("parse revocation timestamp")?)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_db() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        db
    }

    #[test]
    fn migrations_create_expected_tables() {
        let db = fresh_db();
        let conn = db.conn().unwrap();
        let names: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(names.contains(&"token_replay".to_string()), "tables: {names:?}");
        assert!(names.contains(&"cert_revocations".to_string()));
        assert!(names.contains(&"pending_confirms".to_string()));
        assert!(names.contains(&"host_rollout_state".to_string()));
    }

    #[test]
    fn token_replay_round_trip() {
        let db = fresh_db();
        assert!(!db.token_seen("nonce-1").unwrap());
        db.record_token_nonce("nonce-1", "test-host").unwrap();
        assert!(db.token_seen("nonce-1").unwrap());
        // Idempotent on re-record.
        db.record_token_nonce("nonce-1", "test-host").unwrap();
        assert!(db.token_seen("nonce-1").unwrap());
    }

    #[test]
    fn pending_confirms_expired_matches_past_deadline() {
        // Regression: chrono's `to_rfc3339` writes deadlines as
        // `YYYY-MM-DDTHH:MM:SS+00:00` (T separator, +offset), but
        // SQLite's `datetime('now')` returns `YYYY-MM-DD HH:MM:SS`
        // (space separator, no offset). A naked string compare ranks
        // 'T' (0x54) above ' ' (0x20), so deadlines look greater than
        // now forever and `pending_confirms_expired` returns nothing
        // — the rollback timer was a no-op. The query wraps the
        // column in `datetime(...)` to normalise. This test fires on
        // a row whose deadline is firmly in the past.
        let db = fresh_db();
        let past_deadline = Utc::now() - chrono::Duration::seconds(60);
        db.record_pending_confirm(
            "test-host",
            "stable@abc",
            0,
            "decl-system",
            "stable@abc",
            past_deadline,
        )
        .unwrap();

        let expired = db.pending_confirms_expired().unwrap();
        assert_eq!(
            expired.len(),
            1,
            "row past deadline should be picked up, got {expired:?}",
        );
        let (_, host, rollout, _, target) = &expired[0];
        assert_eq!(host, "test-host");
        assert_eq!(rollout, "stable@abc");
        assert_eq!(target, "decl-system");
    }

    #[test]
    fn pending_confirms_expired_skips_future_deadline() {
        // Companion to the regression test above: rows whose deadline
        // is in the future stay out of the expired set.
        let db = fresh_db();
        let future_deadline = Utc::now() + chrono::Duration::seconds(120);
        db.record_pending_confirm(
            "test-host",
            "stable@def",
            0,
            "decl-system",
            "stable@def",
            future_deadline,
        )
        .unwrap();
        let expired = db.pending_confirms_expired().unwrap();
        assert!(expired.is_empty(), "row in window should not expire: {expired:?}");
    }

    #[test]
    fn record_host_healthy_round_trips() {
        let db = fresh_db();
        let now = Utc::now();
        db.record_host_healthy("test-host", "stable@abc12345", now)
            .unwrap();
        let map = db.host_soak_state_for_rollout("stable@abc12345").unwrap();
        assert_eq!(map.len(), 1, "expected one Healthy host: {map:?}");
        let stored = map.get("test-host").expect("test-host present");
        // RFC3339 round-trip drops sub-second precision.
        assert_eq!(stored.timestamp(), now.timestamp());
    }

    #[test]
    fn record_host_healthy_upserts_timestamp() {
        // Re-recording Healthy moves last_healthy_since forward
        // without breaking the row. Step 3 (the reconciler arm)
        // relies on the latest Healthy entry winning so the soak
        // timer always reflects the most recent Healthy moment.
        let db = fresh_db();
        let t1 = Utc::now() - chrono::Duration::seconds(120);
        let t2 = Utc::now();
        db.record_host_healthy("test-host", "stable@r1", t1).unwrap();
        db.record_host_healthy("test-host", "stable@r1", t2).unwrap();
        let map = db.host_soak_state_for_rollout("stable@r1").unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(
            map["test-host"].timestamp(),
            t2.timestamp(),
            "second record_host_healthy must overwrite first"
        );
    }

    #[test]
    fn clear_host_healthy_nulls_timestamp() {
        let db = fresh_db();
        db.record_host_healthy("test-host", "stable@r1", Utc::now())
            .unwrap();
        let n = db.clear_host_healthy("test-host", "stable@r1").unwrap();
        assert_eq!(n, 1);
        let map = db.host_soak_state_for_rollout("stable@r1").unwrap();
        assert!(
            map.is_empty(),
            "cleared host must drop out of soak state: {map:?}"
        );
    }

    #[test]
    fn clear_host_healthy_is_noop_when_already_clear() {
        // Idempotent: calling clear on a row whose marker is
        // already NULL — or on a row that doesn't exist — returns 0
        // and does not fail. The checkin handler may emit clear()
        // every checkin while the host stays diverged.
        let db = fresh_db();
        let n = db.clear_host_healthy("test-host", "stable@r1").unwrap();
        assert_eq!(n, 0, "clear on missing row is no-op");
        db.record_host_healthy("test-host", "stable@r1", Utc::now())
            .unwrap();
        assert_eq!(db.clear_host_healthy("test-host", "stable@r1").unwrap(), 1);
        // Second clear: row exists, marker already NULL.
        assert_eq!(db.clear_host_healthy("test-host", "stable@r1").unwrap(), 0);
    }

    #[test]
    fn host_soak_state_scopes_to_rollout() {
        // Two rollouts, two hosts each — the projection must
        // return only the requested rollout's hosts.
        let db = fresh_db();
        let now = Utc::now();
        db.record_host_healthy("ohm", "stable@r1", now).unwrap();
        db.record_host_healthy("krach", "stable@r1", now).unwrap();
        db.record_host_healthy("pixel", "edge@r2", now).unwrap();

        let r1 = db.host_soak_state_for_rollout("stable@r1").unwrap();
        assert_eq!(r1.len(), 2);
        assert!(r1.contains_key("ohm"));
        assert!(r1.contains_key("krach"));

        let r2 = db.host_soak_state_for_rollout("edge@r2").unwrap();
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
        db.record_pending_confirm(
            "test-host",
            "stable@r1",
            0,
            "target-system-r1",
            "stable@r1",
            future,
        )
        .unwrap();
        // Still pending — healthy_rollouts_for_host must be empty
        // even after recording Healthy (the row exists but the
        // join filter is pc.state = 'confirmed').
        db.record_host_healthy("test-host", "stable@r1", Utc::now())
            .unwrap();
        let pre = db.healthy_rollouts_for_host("test-host").unwrap();
        assert!(
            pre.is_empty(),
            "must not surface rollouts whose pending_confirms is still pending: {pre:?}"
        );

        // Confirm it; now the join hits.
        let n = db.confirm_pending("test-host", "stable@r1").unwrap();
        assert_eq!(n, 1);
        let post = db.healthy_rollouts_for_host("test-host").unwrap();
        assert_eq!(post.len(), 1);
        assert_eq!(post[0].0, "stable@r1");
        assert_eq!(post[0].1, "target-system-r1");
    }

    #[test]
    fn healthy_rollouts_for_host_excludes_cleared_rows() {
        let db = fresh_db();
        let future = Utc::now() + chrono::Duration::seconds(120);
        db.record_pending_confirm(
            "test-host",
            "stable@r1",
            0,
            "target-system-r1",
            "stable@r1",
            future,
        )
        .unwrap();
        db.confirm_pending("test-host", "stable@r1").unwrap();
        db.record_host_healthy("test-host", "stable@r1", Utc::now())
            .unwrap();
        assert_eq!(db.healthy_rollouts_for_host("test-host").unwrap().len(), 1);

        // After clear_host_healthy, the row falls out — it's no
        // longer Healthy, so checkin doesn't need to re-clear.
        db.clear_host_healthy("test-host", "stable@r1").unwrap();
        assert!(db.healthy_rollouts_for_host("test-host").unwrap().is_empty());
    }

    #[test]
    fn active_rollouts_snapshot_empty_when_no_rows() {
        let db = fresh_db();
        let snap = db.active_rollouts_snapshot().unwrap();
        assert!(snap.is_empty());
    }

    #[test]
    fn active_rollouts_snapshot_pending_surfaces_as_confirmwindow() {
        // Dispatch happened but agent has not confirmed yet. The
        // host appears in the rollout with state "ConfirmWindow"
        // (RFC §3.2) and no last_healthy_since marker.
        let db = fresh_db();
        let future = Utc::now() + chrono::Duration::seconds(120);
        db.record_pending_confirm(
            "ohm",
            "stable@abc12345",
            0,
            "system-r1",
            "stable@abc12345",
            future,
        )
        .unwrap();

        let snap = db.active_rollouts_snapshot().unwrap();
        assert_eq!(snap.len(), 1);
        let r = &snap[0];
        assert_eq!(r.rollout_id, "stable@abc12345");
        assert_eq!(r.channel, "stable");
        assert_eq!(r.target_closure_hash, "system-r1");
        assert_eq!(r.target_channel_ref, "stable@abc12345");
        assert_eq!(r.host_states.get("ohm").map(String::as_str), Some("ConfirmWindow"));
        assert!(r.last_healthy_since.is_empty());
    }

    #[test]
    fn active_rollouts_snapshot_confirmed_uses_host_rollout_state() {
        // Once confirm lands, host_rollout_state.host_state takes
        // precedence (matches the path the production handlers
        // write). last_healthy_since surfaces in the side map for
        // step 3's soak gate.
        let db = fresh_db();
        let future = Utc::now() + chrono::Duration::seconds(120);
        let now = Utc::now();
        db.record_pending_confirm(
            "ohm",
            "stable@abc12345",
            0,
            "system-r1",
            "stable@abc12345",
            future,
        )
        .unwrap();
        db.confirm_pending("ohm", "stable@abc12345").unwrap();
        db.record_host_healthy("ohm", "stable@abc12345", now).unwrap();

        let snap = db.active_rollouts_snapshot().unwrap();
        assert_eq!(snap.len(), 1);
        let r = &snap[0];
        assert_eq!(r.host_states.get("ohm").map(String::as_str), Some("Healthy"));
        let stored = r.last_healthy_since.get("ohm").expect("Healthy host has soak ts");
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
        db.record_pending_confirm(
            "ohm",
            "stable@dead",
            0,
            "system-x",
            "stable@dead",
            past,
        )
        .unwrap();
        let expired = db.pending_confirms_expired().unwrap();
        let ids: Vec<i64> = expired.iter().map(|(id, _, _, _, _)| *id).collect();
        db.mark_rolled_back(&ids).unwrap();

        let snap = db.active_rollouts_snapshot().unwrap();
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
            db.record_pending_confirm(host, rollout, 0, "target", rollout, future)
                .unwrap();
        }
        // ohm + pixel confirm; krach + aether stay in ConfirmWindow.
        db.confirm_pending("ohm", "stable@r1").unwrap();
        db.confirm_pending("pixel", "edge@r2").unwrap();
        db.record_host_healthy("ohm", "stable@r1", Utc::now()).unwrap();
        db.record_host_healthy("pixel", "edge@r2", Utc::now()).unwrap();

        let snap = db.active_rollouts_snapshot().unwrap();
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
        db.record_pending_confirm("ohm", "stable@r1", 0, "old", "stable@r1", past)
            .unwrap();
        let expired = db.pending_confirms_expired().unwrap();
        let ids: Vec<i64> = expired.iter().map(|(id, _, _, _, _)| *id).collect();
        db.mark_rolled_back(&ids).unwrap();

        // Second dispatch with a fresh deadline.
        let future = Utc::now() + chrono::Duration::seconds(120);
        db.record_pending_confirm("ohm", "stable@r1", 0, "new", "stable@r1", future)
            .unwrap();

        let snap = db.active_rollouts_snapshot().unwrap();
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
    fn cert_revocation_upserts() {
        let db = fresh_db();
        assert!(db.cert_revoked_before("test-host").unwrap().is_none());
        let t1 = Utc::now();
        db.revoke_cert("test-host", t1, Some("compromised"), Some("operator"))
            .unwrap();
        let r1 = db.cert_revoked_before("test-host").unwrap().unwrap();
        // Stored as rfc3339; round-trip loses sub-second precision.
        assert_eq!(r1.timestamp(), t1.timestamp());
        // Upsert moves not_before forward.
        let t2 = Utc::now() + chrono::Duration::seconds(60);
        db.revoke_cert("test-host", t2, None, None).unwrap();
        let r2 = db.cert_revoked_before("test-host").unwrap().unwrap();
        assert!(r2 >= r1);
    }
}
