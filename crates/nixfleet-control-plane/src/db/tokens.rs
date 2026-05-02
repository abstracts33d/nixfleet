//! `token_replay` — bootstrap-token nonces.
//!
//! Recovery class: **soft state** (ARCHITECTURE.md §6 Phase 10).
//! Loss extends the replay window by up to one TTL (24h); bounded,
//! no security regression on rebuild.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::sync::Mutex;

pub struct Tokens<'a> {
    pub(super) conn: &'a Mutex<Connection>,
}

/// Outcome of an attempt to record a bootstrap-token nonce.
///
/// The variants exist so the `/v1/enroll` handler can distinguish a
/// concurrent-replay race (`AlreadyRecorded` → 409 CONFLICT) from a
/// transient DB failure (`Err` → 500 INTERNAL_SERVER_ERROR). The old
/// `INSERT OR IGNORE` collapsed both into `Ok(())`, which let two
/// simultaneous enroll requests for the same nonce both succeed —
/// only one row inserted, but both code paths returned `Ok` and
/// minted certs.
#[derive(Debug, PartialEq, Eq)]
pub enum RecordTokenOutcome {
    /// This call inserted the nonce row. The caller is the
    /// authoritative consumer of the bootstrap token.
    Recorded,
    /// The nonce was already recorded (PRIMARY KEY conflict). Either
    /// a benign retry of an idempotent enroll OR a concurrent replay.
    /// The handler treats this as 409 CONFLICT — only one of the
    /// concurrent callers should proceed to mint a cert.
    AlreadyRecorded,
}

impl Tokens<'_> {
    /// True iff `nonce` was previously recorded.
    pub fn token_seen(&self, nonce: &str) -> Result<bool> {
        let guard = super::lock_conn(self.conn)?;
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

    /// Atomically record `nonce` as seen. Returns `Recorded` if THIS
    /// call inserted the row; `AlreadyRecorded` if a concurrent
    /// caller (or earlier successful enroll) won the race. Genuine
    /// IO/SQL failures bubble up as `Err`.
    ///
    /// Plain `INSERT` (not `INSERT OR IGNORE`): on PRIMARY KEY
    /// conflict SQLite returns `SQLITE_CONSTRAINT_PRIMARYKEY`, which
    /// we map to `AlreadyRecorded`. This is the atomic check-and-set
    /// the enroll handler relies on for replay defence under
    /// concurrent requests.
    pub fn record_token_nonce(
        &self,
        nonce: &str,
        hostname: &str,
    ) -> Result<RecordTokenOutcome> {
        let guard = super::lock_conn(self.conn)?;
        match guard.execute(
            "INSERT INTO token_replay(nonce, hostname) VALUES (?1, ?2)",
            params![nonce, hostname],
        ) {
            Ok(_) => Ok(RecordTokenOutcome::Recorded),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Ok(RecordTokenOutcome::AlreadyRecorded)
            }
            Err(e) => Err(anyhow::Error::from(e).context("insert token_replay")),
        }
    }

    /// Drop replay records older than `max_age` (typical: 24h, the
    /// token validity window). Returns the number of pruned rows.
    /// A periodic background task invokes this.
    pub fn prune_token_replay(&self, max_age_hours: i64) -> Result<usize> {
        let guard = super::lock_conn(self.conn)?;
        let n = guard
            .execute(
                "DELETE FROM token_replay
                 WHERE first_seen < datetime('now', ?1)",
                params![format!("-{max_age_hours} hours")],
            )
            .context("prune token_replay")?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::fresh_db;

    #[test]
    fn token_replay_round_trip() {
        let db = fresh_db();
        assert!(!db.tokens().token_seen("nonce-1").unwrap());
        let outcome = db
            .tokens()
            .record_token_nonce("nonce-1", "test-host")
            .unwrap();
        assert_eq!(outcome, super::RecordTokenOutcome::Recorded);
        assert!(db.tokens().token_seen("nonce-1").unwrap());
    }

    #[test]
    fn record_token_nonce_returns_already_recorded_on_repeat() {
        // The TOCTOU race fix: a second record_token_nonce for the
        // same nonce must surface the conflict (not silently no-op
        // as the old `INSERT OR IGNORE` did). The /v1/enroll handler
        // turns this into a 409 CONFLICT.
        let db = fresh_db();
        let first = db
            .tokens()
            .record_token_nonce("nonce-1", "test-host")
            .unwrap();
        assert_eq!(first, super::RecordTokenOutcome::Recorded);

        let second = db
            .tokens()
            .record_token_nonce("nonce-1", "test-host")
            .unwrap();
        assert_eq!(second, super::RecordTokenOutcome::AlreadyRecorded);
    }
}
