//! SQLite persistence (rusqlite + refinery, WAL + FK).
//!
//! A single `Mutex<Connection>` is sufficient for fleet sizes O(100).
//! Schema lives under `migrations/`; `migrate()` is idempotent +
//! version-tracked. Mutex poisoning surfaces as anyhow errors.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use crate::state::{HealthyMarker, HostRolloutState, PendingConfirmState};

mod embedded {
    use refinery::embed_migrations;
    embed_migrations!("migrations");
}

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

/// `(id, hostname, rollout_id, wave, target_closure_hash)`. Aliased
/// to keep the signature readable and silence `type_complexity`.
pub type ExpiredPendingConfirm = (i64, String, String, u32, String);

/// `signature_status` is the raw kebab-case string; caller
/// deserialises into `nixfleet_reconciler::evidence::SignatureStatus`.
#[derive(Debug, Clone)]
pub struct HostReportRow {
    pub event_id: String,
    pub received_at: DateTime<Utc>,
    pub event_kind: String,
    pub rollout: Option<String>,
    pub signature_status: Option<String>,
    pub report_json: String,
}

/// Bundled to keep call sites readable (avoids `too_many_arguments`).
#[derive(Debug, Clone)]
pub struct HostReportInsert<'a> {
    pub hostname: &'a str,
    pub event_id: &'a str,
    pub received_at: DateTime<Utc>,
    pub event_kind: &'a str,
    pub rollout: Option<&'a str>,
    pub signature_status: Option<&'a str>,
    pub report_json: &'a str,
}

/// Bundled args for [`Db::record_pending_confirm`]. Mirrors the
/// [`HostReportInsert`] precedent — both `rollout_id` and
/// `target_channel_ref` are `&str` literals shaped like
/// `"stable@abc12345"`, easy to swap positionally; the named struct
/// makes that class of bug a compile error at the call site.
#[derive(Debug, Clone)]
pub struct PendingConfirmInsert<'a> {
    pub hostname: &'a str,
    pub rollout_id: &'a str,
    pub wave: u32,
    pub target_closure_hash: &'a str,
    pub target_channel_ref: &'a str,
    pub confirm_deadline: DateTime<Utc>,
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

    /// Run all pending migrations. Idempotent under refinery
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

    /// — prune terminal `pending_confirms` rows older than
    /// `max_age`. Mirror of `prune_token_replay`. `pending_confirms`
    /// is soft-state (ARCHITECTURE.md §6 Phase 10) and rows in
    /// terminal states `RolledBack` / `Cancelled` carry no
    /// load-bearing semantics — they accumulate one row per
    /// dispatch + churn cycle and bloat the table indefinitely
    /// without retention. Lab observed 39 such rows from 3 days of
    /// deploy thrash. Default retention 7 days (caller chooses).
    /// Returns number of pruned rows.
    pub fn prune_pending_confirms(&self, max_age_hours: i64) -> Result<usize> {
        let rolled_back = PendingConfirmState::RolledBack.as_db_str();
        let cancelled = PendingConfirmState::Cancelled.as_db_str();
        let guard = self.conn()?;
        let n = guard
            .execute(
                "DELETE FROM pending_confirms
                 WHERE state IN (?1, ?2)
                   AND dispatched_at < datetime('now', ?3)",
                params![
                    rolled_back,
                    cancelled,
                    format!("-{max_age_hours} hours")
                ],
            )
            .context("prune pending_confirms")?;
        Ok(n)
    }

    // =================================================================
    // cert_revocations
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
    // pending_confirms — activation confirmations
    // + magic rollback timer support
    // ===============================================================

    /// Record a dispatched activation. Called from the dispatch loop
    /// when CP populates `target` in a checkin response. The agent
    /// will later post `/v1/agent/confirm` with the same `rollout_id`
    /// once it boots the new closure.
    pub fn record_pending_confirm(&self, row: &PendingConfirmInsert<'_>) -> Result<i64> {
        let guard = self.conn()?;
        guard
            .execute(
                "INSERT INTO pending_confirms(hostname, rollout_id, wave,
                                              target_closure_hash,
                                              target_channel_ref,
                                              confirm_deadline)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    row.hostname,
                    row.rollout_id,
                    row.wave,
                    row.target_closure_hash,
                    row.target_channel_ref,
                    row.confirm_deadline.to_rfc3339()
                ],
            )
            .context("insert pending_confirms")?;
        Ok(guard.last_insert_rowid())
    }

    /// Insert a `pending_confirms` row directly in `'confirmed'`
    /// state — used by the orphan-confirm recovery path when
    /// an agent posts `/v1/agent/confirm` but no matching `pending`
    /// row exists (typically because the CP was rebuilt mid-flight).
    /// The orphan handler verifies the agent's `closure_hash` matches
    /// the host's declared target before calling this; the synthetic
    /// row preserves the audit trail of "this host activated this
    /// closure" without forcing a spurious rollback. `confirm_deadline`
    /// is set to `confirmed_at` since the deadline is moot for an
    /// already-confirmed row.
    pub fn record_confirmed_pending(
        &self,
        hostname: &str,
        rollout_id: &str,
        wave: u32,
        target_closure_hash: &str,
        target_channel_ref: &str,
        confirmed_at: DateTime<Utc>,
    ) -> Result<i64> {
        let guard = self.conn()?;
        let ts = confirmed_at.to_rfc3339();
        guard
            .execute(
                "INSERT INTO pending_confirms(hostname, rollout_id, wave,
                                              target_closure_hash,
                                              target_channel_ref,
                                              confirm_deadline,
                                              confirmed_at,
                                              state)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    hostname,
                    rollout_id,
                    wave,
                    target_closure_hash,
                    target_channel_ref,
                    &ts,
                    &ts,
                    PendingConfirmState::Confirmed.as_db_str(),
                ],
            )
            .context("insert pending_confirms (orphan recovery)")?;
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
                 WHERE hostname = ?1 AND state = ?2",
                params![hostname, PendingConfirmState::Pending.as_db_str()],
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
                     state = ?3
                 WHERE hostname = ?1
                   AND rollout_id = ?2
                   AND state = ?4",
                params![
                    hostname,
                    rollout_id,
                    PendingConfirmState::Confirmed.as_db_str(),
                    PendingConfirmState::Pending.as_db_str(),
                ],
            )
            .context("update pending_confirms confirmed")?;
        Ok(n)
    }


    /// Pending confirms whose deadline has passed and which haven't
    /// been confirmed yet. Used by the magic-rollback timer task
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
    pub fn pending_confirms_expired(&self) -> Result<Vec<ExpiredPendingConfirm>> {
        let guard = self.conn()?;
        let mut stmt = guard.prepare(
            "SELECT id, hostname, rollout_id, wave, target_closure_hash
             FROM pending_confirms
             WHERE state = ?1
               AND datetime(confirm_deadline) < datetime('now')",
        )?;
        let rows = stmt
            .query_map(params![PendingConfirmState::Pending.as_db_str()], |row| {
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
        // SQLite IN clause via repeated `?` placeholders. The state
        // literals come from the typed enum so a future variant rename
        // can't drift between this UPDATE and the rest of db.rs.
        let placeholders = (1..=ids.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let new_state_idx = ids.len() + 1;
        let pending_idx = ids.len() + 2;
        let sql = format!(
            "UPDATE pending_confirms
             SET state = ?{new_state_idx}
             WHERE state = ?{pending_idx} AND id IN ({placeholders})"
        );
        let mut stmt = guard.prepare(&sql)?;
        let mut bound: Vec<&dyn rusqlite::ToSql> =
            ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
        let rolled_back = PendingConfirmState::RolledBack.as_db_str();
        let pending = PendingConfirmState::Pending.as_db_str();
        bound.push(&rolled_back);
        bound.push(&pending);
        let n = stmt.execute(rusqlite::params_from_iter(bound.iter()))?;
        Ok(n)
    }

    // ===============================================================
    // host_rollout_state — / §4.4 soak timer support
    // ===============================================================

    /// Transition (rollout, host) into `new_state`, optionally
    /// stamping `last_healthy_since` via `marker`. Replaces the
    /// per-state pair of methods (`record_host_healthy`,
    /// `mark_host_soaked`) with a single typed entry routed through
    /// [`HostRolloutState`] — magic strings stop leaking into db.rs
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
        let guard = self.conn()?;
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

    /// True iff any `host_rollout_state` row exists for the given
    /// (rollout_id, hostname). Used by 's soak-state
    /// recovery path to avoid overwriting existing host state when
    /// the agent's attestation arrives — an existing row reflects
    /// the actual lifecycle (Healthy/Soaked/Reverted/...) and is
    /// more authoritative than a re-attestation.
    pub fn host_rollout_state_exists(
        &self,
        hostname: &str,
        rollout_id: &str,
    ) -> Result<bool> {
        let guard = self.conn()?;
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
    /// they entered Healthy. Step 2 (next session, the
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
    /// the confirm-handler-success path's `transition_host_state`
    /// is the only emitter of Healthy rows. `DISTINCT` collapses the rare case of
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

    /// Snapshot the active rollouts derived from the DB for the
    /// observed-state projection (step 2). For each
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
                 WHERE pc.state IN (?1, ?2)
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
            .query_map(
                params![
                    PendingConfirmState::Pending.as_db_str(),
                    PendingConfirmState::Confirmed.as_db_str(),
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?, // rollout_id
                        row.get::<_, String>(1)?, // hostname
                        row.get::<_, String>(2)?, // target_closure_hash
                        row.get::<_, String>(3)?, // target_channel_ref
                        row.get::<_, String>(4)?, // pc_state
                        row.get::<_, Option<String>>(5)?, // hrs.host_state
                        row.get::<_, Option<String>>(6)?, // hrs.last_healthy_since
                    ))
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        // BTreeMap keeps rollout_ids ordered without an extra sort.
        let mut by_rollout: BTreeMap<String, RolloutDbSnapshot> = BTreeMap::new();
        for (rollout_id, hostname, target_closure, target_ref, pc_state, hrs_state, hrs_ts) in rows
        {
            // Derive the host's state name. host_rollout_state
            // wins when present — it carries the post-confirm
            // machine (Healthy/Soaked/...) that step 3 will write.
            // Otherwise infer from pending_confirms.state. Routing
            // through `HostRolloutState::from_db_str` guards against
            // schema drift; routing the inferred fallbacks through
            // `HostRolloutState::as_db_str` keeps the literals
            // single-sourced from the V003 enum.
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

    // ===============================================================
    // host_reports — durable per-host event log
    // ===============================================================

    /// Persist an event report. Mirrors the in-memory ring buffer
    /// write in `server::handlers::report` so survives CP restart.
    /// `signature_status` is the kebab-case `SignatureStatus` serde
    /// representation (or `None` for events that don't carry the
    /// contract). `report_json` is the canonical JSON envelope of
    /// the wire `ReportRequest`.
    pub fn record_host_report(&self, row: &HostReportInsert<'_>) -> Result<()> {
        let guard = self.conn()?;
        guard
            .execute(
                "INSERT INTO host_reports
                   (hostname, event_id, received_at, event_kind,
                    rollout, signature_status, report_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    row.hostname,
                    row.event_id,
                    row.received_at.to_rfc3339(),
                    row.event_kind,
                    row.rollout,
                    row.signature_status,
                    row.report_json
                ],
            )
            .context("insert host_reports")?;
        Ok(())
    }

    /// Hydrate the in-memory ring buffer at CP startup. Returns up
    /// to `limit_per_host` most-recent rows per `hostname`,
    /// chronological order. Used by `server::serve` after migration
    /// completes — the dispatch path consults the ring buffer for
    /// hot-path latency, but durability lives in this table.
    pub fn host_reports_recent_per_host(
        &self,
        hostname: &str,
        limit_per_host: usize,
    ) -> Result<Vec<HostReportRow>> {
        let guard = self.conn()?;
        let mut stmt = guard.prepare(
            "SELECT event_id, received_at, event_kind, rollout, signature_status, report_json
             FROM host_reports
             WHERE hostname = ?1
             ORDER BY received_at DESC
             LIMIT ?2",
        )?;
        let rows: rusqlite::Result<Vec<HostReportRow>> = stmt
            .query_map(params![hostname, limit_per_host as i64], |row| {
                let received_str: String = row.get(1)?;
                let received_at = received_str
                    .parse::<DateTime<Utc>>()
                    .map_err(|e| rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    ))?;
                Ok(HostReportRow {
                    event_id: row.get::<_, String>(0)?,
                    received_at,
                    event_kind: row.get::<_, String>(2)?,
                    rollout: row.get::<_, Option<String>>(3)?,
                    signature_status: row.get::<_, Option<String>>(4)?,
                    report_json: row.get::<_, String>(5)?,
                })
            })?
            .collect();
        let mut rows = rows.context("query host_reports")?;
        // Caller wants chronological (oldest first) for ring-buffer
        // insertion order; DB returns newest first.
        rows.reverse();
        Ok(rows)
    }

    /// List every hostname that has at least one host_reports row.
    /// Used at CP startup to drive the per-host hydration loop.
    pub fn host_reports_known_hostnames(&self) -> Result<Vec<String>> {
        let guard = self.conn()?;
        let mut stmt = guard.prepare("SELECT DISTINCT hostname FROM host_reports")?;
        let names: rusqlite::Result<Vec<String>> = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect();
        names.context("query host_reports hostnames")
    }

    /// Drop host_reports rows older than `max_age_hours`. Mirror of
    /// `prune_pending_confirms`; same 7-day retention default. Wired
    /// into `prune_timer.rs`.
    pub fn prune_host_reports(&self, max_age_hours: i64) -> Result<usize> {
        let guard = self.conn()?;
        let n = guard
            .execute(
                "DELETE FROM host_reports
                 WHERE received_at < datetime('now', ?1)",
                params![format!("-{max_age_hours} hours")],
            )
            .context("prune host_reports")?;
        Ok(n)
    }

    /// Count outstanding ComplianceFailure / RuntimeGateError events
    /// per `(rollout_id, hostname)`. Used by the reconciler's
    /// wave-staging gate emission . The per-rollout
    /// grouping is what enforces resolution-by-replacement: an
    /// event posted against rollout R₀ contributes to `(R₀, host)`
    /// not to `host`-globally, so once the host moves to R₁ and the
    /// reconciler iterates active rollouts, R₀'s events don't
    /// appear under R₁'s key — correct behaviour.
    ///
    /// Events with `rollout IS NULL` (enrollment errors, trust-root
    /// problems — pre-cert-bound paths) are excluded; those are
    /// not rollout-scoped and don't gate wave promotion.
    ///
    /// `signature_status` filter mirrors the
    /// `nixfleet_reconciler::evidence::SignatureStatus::counts_for_gate` rule:
    /// `mismatch` and `malformed` are forged FAIL events from a
    /// compromised mTLS cert and don't count; everything else
    /// (verified, unsigned, no-pubkey, wrong-algorithm, NULL) does.
    ///
    /// Returns a nested map keyed first by rollout id, then by
    /// hostname → count. Empty inner maps are absent (rollouts with
    /// zero outstanding events don't appear at all).
    pub fn outstanding_compliance_events_by_rollout(
        &self,
    ) -> Result<HashMap<String, HashMap<String, usize>>> {
        let guard = self.conn()?;
        let mut stmt = guard.prepare(
            "SELECT rollout, hostname, COUNT(*) FROM host_reports
             WHERE rollout IS NOT NULL
               AND event_kind IN ('compliance-failure', 'runtime-gate-error')
               AND COALESCE(signature_status, '') NOT IN ('mismatch', 'malformed')
             GROUP BY rollout, hostname",
        )?;
        let mut out: HashMap<String, HashMap<String, usize>> = HashMap::new();
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)? as usize,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("query outstanding_compliance_events_by_rollout")?;
        for (rollout, host, n) in rows {
            out.entry(rollout).or_default().insert(host, n);
        }
        Ok(out)
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
        db.record_pending_confirm(&pc_insert(
            "test-host",
            "stable@abc",
            "decl-system",
            past_deadline,
        ))
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
        db.record_pending_confirm(&pc_insert(
            "test-host",
            "stable@def",
            "decl-system",
            future_deadline,
        ))
        .unwrap();
        let expired = db.pending_confirms_expired().unwrap();
        assert!(expired.is_empty(), "row in window should not expire: {expired:?}");
    }

    /// Test helper: shorthand for the legacy "record host as Healthy
    /// with marker stamp" call, expressed via the new typed
    /// transition. Reduces churn in the broader test corpus and
    /// keeps each assertion focused on its scenario.
    fn mark_healthy(db: &Db, host: &str, rollout: &str, now: DateTime<Utc>) {
        db.transition_host_state(
            host,
            rollout,
            HostRolloutState::Healthy,
            HealthyMarker::Set(now),
            None,
        )
        .unwrap();
    }

    /// Test helper: build a `PendingConfirmInsert` with the common
    /// shape used across the test module (rollout_id reused as
    /// channel_ref, mirroring how `dispatch.rs` populates the row).
    fn pc_insert<'a>(
        host: &'a str,
        rollout: &'a str,
        target_closure: &'a str,
        deadline: DateTime<Utc>,
    ) -> PendingConfirmInsert<'a> {
        PendingConfirmInsert {
            hostname: host,
            rollout_id: rollout,
            wave: 0,
            target_closure_hash: target_closure,
            target_channel_ref: rollout,
            confirm_deadline: deadline,
        }
    }

    #[test]
    fn transition_to_healthy_round_trips() {
        let db = fresh_db();
        let now = Utc::now();
        mark_healthy(&db, "test-host", "stable@abc12345", now);
        let map = db.host_soak_state_for_rollout("stable@abc12345").unwrap();
        assert_eq!(map.len(), 1, "expected one Healthy host: {map:?}");
        let stored = map.get("test-host").expect("test-host present");
        // RFC3339 round-trip drops sub-second precision.
        assert_eq!(stored.timestamp(), now.timestamp());
    }

    #[test]
    fn transition_to_healthy_upserts_timestamp() {
        // Re-recording Healthy moves last_healthy_since forward
        // without breaking the row. Step 3 (the reconciler arm)
        // relies on the latest Healthy entry winning so the soak
        // timer always reflects the most recent Healthy moment.
        let db = fresh_db();
        let t1 = Utc::now() - chrono::Duration::seconds(120);
        let t2 = Utc::now();
        mark_healthy(&db, "test-host", "stable@r1", t1);
        mark_healthy(&db, "test-host", "stable@r1", t2);
        let map = db.host_soak_state_for_rollout("stable@r1").unwrap();
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
        let n = db.clear_healthy_marker("test-host", "stable@r1").unwrap();
        assert_eq!(n, 1);
        let map = db.host_soak_state_for_rollout("stable@r1").unwrap();
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
        let n = db.clear_healthy_marker("test-host", "stable@r1").unwrap();
        assert_eq!(n, 0, "clear on missing row is no-op");
        mark_healthy(&db, "test-host", "stable@r1", Utc::now());
        assert_eq!(db.clear_healthy_marker("test-host", "stable@r1").unwrap(), 1);
        // Second clear: row exists, marker already NULL.
        assert_eq!(db.clear_healthy_marker("test-host", "stable@r1").unwrap(), 0);
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
        db.record_pending_confirm(&pc_insert(
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
        db.record_pending_confirm(&pc_insert(
            "test-host",
            "stable@r1",
            "target-system-r1",
            future,
        ))
        .unwrap();
        db.confirm_pending("test-host", "stable@r1").unwrap();
        mark_healthy(&db, "test-host", "stable@r1", Utc::now());
        assert_eq!(db.healthy_rollouts_for_host("test-host").unwrap().len(), 1);

        // After clear_healthy_marker, the row falls out — it's no
        // longer Healthy, so checkin doesn't need to re-clear.
        db.clear_healthy_marker("test-host", "stable@r1").unwrap();
        assert!(db.healthy_rollouts_for_host("test-host").unwrap().is_empty());
    }

    #[test]
    fn record_confirmed_pending_writes_confirmed_state() {
        // Gap A orphan-confirm recovery path. Synthetic row must
        // land in 'confirmed' state with confirmed_at populated and
        // be picked up by active_rollouts_snapshot just like a row
        // that went through the normal pending → confirmed flow.
        let db = fresh_db();
        let now = Utc::now();
        db.record_confirmed_pending(
            "test-host",
            "stable@orphan",
            0,
            "target-system",
            "stable@orphan",
            now,
        )
        .unwrap();
        // The host is not yet recorded as Healthy — the handler
        // does that as a separate step. So the snapshot's
        // host_states maps to the defensive "Healthy" fallback for
        // confirmed rows without an hrs row.
        let snap = db.active_rollouts_snapshot().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].rollout_id, "stable@orphan");
        assert_eq!(snap[0].target_closure_hash, "target-system");
        assert_eq!(
            snap[0].host_states.get("test-host").map(String::as_str),
            Some("Healthy"),
        );
        // healthy_rollouts_for_host requires both the confirmed row
        // and the Healthy marker to surface — exercising the join.
        // Pre-Healthy: empty.
        assert!(db.healthy_rollouts_for_host("test-host").unwrap().is_empty());
        mark_healthy(&db, "test-host", "stable@orphan", now);
        let healthy = db.healthy_rollouts_for_host("test-host").unwrap();
        assert_eq!(healthy.len(), 1);
        assert_eq!(healthy[0].0, "stable@orphan");
        assert_eq!(healthy[0].1, "target-system");
    }

    #[test]
    fn transition_to_soaked_only_from_healthy() {
        // Step 3 SoakHost handler. Only Healthy → Soaked is valid.
        // The guarded UPDATE shape encodes that as
        // `expected_from = Some(Healthy)`.
        let db = fresh_db();
        let to_soaked = |db: &Db, host: &str, rollout: &str| {
            db.transition_host_state(
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
        db.record_pending_confirm(&pc_insert("ohm", "stable@r1", "target", future))
            .unwrap();
        db.confirm_pending("ohm", "stable@r1").unwrap();
        let snap = db.active_rollouts_snapshot().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(
            snap[0].host_states.get("ohm").map(String::as_str),
            Some("Soaked"),
        );
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
        db.record_pending_confirm(&pc_insert(
            "ohm",
            "stable@abc12345",
            "system-r1",
            future,
        ))
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
        db.record_pending_confirm(&pc_insert(
            "ohm",
            "stable@abc12345",
            "system-r1",
            future,
        ))
        .unwrap();
        db.confirm_pending("ohm", "stable@abc12345").unwrap();
        mark_healthy(&db, "ohm", "stable@abc12345", now);

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
        db.record_pending_confirm(&pc_insert("ohm", "stable@dead", "system-x", past))
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
            db.record_pending_confirm(&pc_insert(host, rollout, "target", future))
                .unwrap();
        }
        // ohm + pixel confirm; krach + aether stay in ConfirmWindow.
        db.confirm_pending("ohm", "stable@r1").unwrap();
        db.confirm_pending("pixel", "edge@r2").unwrap();
        mark_healthy(&db, "ohm", "stable@r1", Utc::now());
        mark_healthy(&db, "pixel", "edge@r2", Utc::now());

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
        db.record_pending_confirm(&pc_insert("ohm", "stable@r1", "old", past))
            .unwrap();
        let expired = db.pending_confirms_expired().unwrap();
        let ids: Vec<i64> = expired.iter().map(|(id, _, _, _, _)| *id).collect();
        db.mark_rolled_back(&ids).unwrap();

        // Second dispatch with a fresh deadline.
        let future = Utc::now() + chrono::Duration::seconds(120);
        db.record_pending_confirm(&pc_insert("ohm", "stable@r1", "new", future))
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

    // ===============================================================
    // host_reports — round-trip + outstanding-event query
    // ===============================================================

    fn fail_event<'a>(rollout: Option<&'a str>, sig: Option<&'a str>) -> HostReportInsert<'a> {
        HostReportInsert {
            hostname: "lab",
            event_id: "evt-test",
            received_at: Utc::now(),
            event_kind: "compliance-failure",
            rollout,
            signature_status: sig,
            report_json: r#"{"hostname":"lab","agentVersion":"test"}"#,
        }
    }

    #[test]
    fn host_reports_round_trip_preserves_envelope() {
        let db = fresh_db();
        let row = HostReportInsert {
            hostname: "lab",
            event_id: "evt-rt-1",
            received_at: Utc::now(),
            event_kind: "compliance-failure",
            rollout: Some("edge-slow@abc"),
            signature_status: Some("verified"),
            report_json: r#"{"hostname":"lab","agentVersion":"0.2.0"}"#,
        };
        db.record_host_report(&row).unwrap();
        let mut got = db.host_reports_recent_per_host("lab", 8).unwrap();
        assert_eq!(got.len(), 1);
        let r = got.pop().unwrap();
        assert_eq!(r.event_id, "evt-rt-1");
        assert_eq!(r.event_kind, "compliance-failure");
        assert_eq!(r.rollout.as_deref(), Some("edge-slow@abc"));
        assert_eq!(r.signature_status.as_deref(), Some("verified"));
    }

    #[test]
    fn outstanding_events_by_rollout_filters_tampered() {
        // Verified + unsigned + no-pubkey count toward the gate;
        // mismatch + malformed do NOT (defends against forged FAIL
        // events from a compromised mTLS cert).
        let db = fresh_db();
        for (eid, sig) in [
            ("e1", Some("verified")),
            ("e2", Some("unsigned")),
            ("e3", Some("no-pubkey")),
            ("e4", Some("mismatch")),
            ("e5", Some("malformed")),
        ] {
            let mut row = fail_event(Some("R1"), sig);
            row.event_id = eid;
            db.record_host_report(&row).unwrap();
        }
        let by_rollout = db.outstanding_compliance_events_by_rollout().unwrap();
        // verified + unsigned + no-pubkey = 3; mismatch + malformed
        // are filtered out.
        assert_eq!(
            by_rollout.get("R1").and_then(|m| m.get("lab")).copied(),
            Some(3),
        );
    }

    #[test]
    fn outstanding_events_by_rollout_groups_per_rollout() {
        // Resolution-by-replacement test: events for R0 stay under R0,
        // events for R1 stay under R1. The reconciler iterates active
        // rollouts and looks up its own ID's outstanding events; an
        // R0-bound event must NOT contaminate R1's count.
        let db = fresh_db();
        let mut e0 = fail_event(Some("R0"), Some("verified"));
        e0.event_id = "evt-r0-1";
        db.record_host_report(&e0).unwrap();
        let mut e1 = fail_event(Some("R1"), Some("verified"));
        e1.event_id = "evt-r1-1";
        db.record_host_report(&e1).unwrap();
        let by_rollout = db.outstanding_compliance_events_by_rollout().unwrap();
        assert_eq!(
            by_rollout.get("R0").and_then(|m| m.get("lab")).copied(),
            Some(1),
        );
        assert_eq!(
            by_rollout.get("R1").and_then(|m| m.get("lab")).copied(),
            Some(1),
        );
    }

    #[test]
    fn outstanding_events_by_rollout_excludes_null_rollout() {
        // Events with rollout=NULL (enrollment, trust-root errors)
        // are not rollout-scoped and don't appear in the projection.
        let db = fresh_db();
        let mut row = fail_event(None, Some("verified"));
        row.event_id = "evt-orphan";
        db.record_host_report(&row).unwrap();
        let by_rollout = db.outstanding_compliance_events_by_rollout().unwrap();
        assert!(
            by_rollout.is_empty(),
            "rollout=NULL events should not appear: {:?}",
            by_rollout,
        );
    }

    #[test]
    fn prune_host_reports_drops_old_rows() {
        let db = fresh_db();
        // Insert with a past received_at so the prune sweep matches.
        let past = Utc::now() - chrono::Duration::hours(48);
        let row = HostReportInsert {
            hostname: "lab",
            event_id: "evt-old",
            received_at: past,
            event_kind: "compliance-failure",
            rollout: None,
            signature_status: None,
            report_json: "{}",
        };
        db.record_host_report(&row).unwrap();
        // 24h retention drops the past row.
        let n = db.prune_host_reports(24).unwrap();
        assert_eq!(n, 1);
        let names = db.host_reports_known_hostnames().unwrap();
        assert!(names.is_empty(), "old row should be pruned");
    }
}
