//! `cert_revocations` — agent-cert revocation list.
//!
//! Recovery class: **hard state** (ARCHITECTURE.md §6 Phase 10).
//! Loss is a security regression — previously-revoked certs would
//! become valid again. Mitigated by the signed `revocations.json`
//! sidecar (#48): operator commits revocations to the fleet repo,
//! CI signs the artifact with the same key that signs
//! `fleet.resolved.json`, and the CP fetches + verifies + replays on
//! every reconcile tick. Recovery from empty is "one tick later,
//! table populated from the signed artifact."

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::sync::Mutex;

pub struct Revocations<'a> {
    pub(super) conn: &'a Mutex<Connection>,
}

impl Revocations<'_> {
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
        let guard = super::lock_conn(self.conn)?;
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

    /// Return the most recent revocation `not_before` for `hostname`,
    /// or `None` if not revoked. Caller compares against the
    /// presented cert's notBefore at mTLS handshake time.
    pub fn cert_revoked_before(&self, hostname: &str) -> Result<Option<DateTime<Utc>>> {
        let guard = super::lock_conn(self.conn)?;
        let row: Result<String, _> = guard.query_row(
            "SELECT not_before FROM cert_revocations WHERE hostname = ?1",
            params![hostname],
            |r| r.get(0),
        );
        match row {
            Ok(s) => Ok(Some(
                s.parse::<DateTime<Utc>>()
                    .context("parse revocation timestamp")?,
            )),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::fresh_db;
    use chrono::Utc;

    #[test]
    fn cert_revocation_upserts() {
        let db = fresh_db();
        assert!(db
            .revocations()
            .cert_revoked_before("test-host")
            .unwrap()
            .is_none());
        let t1 = Utc::now();
        db.revocations()
            .revoke_cert("test-host", t1, Some("compromised"), Some("operator"))
            .unwrap();
        let r1 = db
            .revocations()
            .cert_revoked_before("test-host")
            .unwrap()
            .unwrap();
        // Stored as rfc3339; round-trip loses sub-second precision.
        assert_eq!(r1.timestamp(), t1.timestamp());
        // Upsert moves not_before forward.
        let t2 = Utc::now() + chrono::Duration::seconds(60);
        db.revocations()
            .revoke_cert("test-host", t2, None, None)
            .unwrap();
        let r2 = db
            .revocations()
            .cert_revoked_before("test-host")
            .unwrap()
            .unwrap();
        assert!(r2 >= r1);
    }
}
