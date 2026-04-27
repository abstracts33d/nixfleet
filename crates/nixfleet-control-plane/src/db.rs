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
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

mod embedded {
    use refinery::embed_migrations;
    embed_migrations!("migrations");
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
    }

    #[test]
    fn token_replay_round_trip() {
        let db = fresh_db();
        assert!(!db.token_seen("nonce-1").unwrap());
        db.record_token_nonce("nonce-1", "krach").unwrap();
        assert!(db.token_seen("nonce-1").unwrap());
        // Idempotent on re-record.
        db.record_token_nonce("nonce-1", "krach").unwrap();
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
            "krach",
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
        assert_eq!(host, "krach");
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
            "krach",
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
    fn cert_revocation_upserts() {
        let db = fresh_db();
        assert!(db.cert_revoked_before("krach").unwrap().is_none());
        let t1 = Utc::now();
        db.revoke_cert("krach", t1, Some("compromised"), Some("operator"))
            .unwrap();
        let r1 = db.cert_revoked_before("krach").unwrap().unwrap();
        // Stored as rfc3339; round-trip loses sub-second precision.
        assert_eq!(r1.timestamp(), t1.timestamp());
        // Upsert moves not_before forward.
        let t2 = Utc::now() + chrono::Duration::seconds(60);
        db.revoke_cert("krach", t2, None, None).unwrap();
        let r2 = db.cert_revoked_before("krach").unwrap().unwrap();
        assert!(r2 >= r1);
    }
}
