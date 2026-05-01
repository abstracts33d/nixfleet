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

    /// Record `nonce` as seen. No-op if the nonce already exists
    /// (caller is expected to check `token_seen` first if it cares;
    /// this is just `INSERT OR IGNORE`).
    pub fn record_token_nonce(&self, nonce: &str, hostname: &str) -> Result<()> {
        let guard = super::lock_conn(self.conn)?;
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
        db.tokens()
            .record_token_nonce("nonce-1", "test-host")
            .unwrap();
        assert!(db.tokens().token_seen("nonce-1").unwrap());
        // Idempotent on re-record.
        db.tokens()
            .record_token_nonce("nonce-1", "test-host")
            .unwrap();
        assert!(db.tokens().token_seen("nonce-1").unwrap());
    }
}
