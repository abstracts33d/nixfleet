//! Periodic SQLite hygiene sweep.
//!
//! Every hour, walks the soft-state tables that accumulate without
//! their own retention semantics:
//!
//! - `token_replay` — bootstrap nonces past the 24h validity window
//!   (`Db::prune_token_replay`)
//! - `pending_confirms` — terminal rows (`rolled-back` / `cancelled`)
//!   past 7 days (`Db::prune_pending_confirms`, )
//! - `host_reports` — event log past 7 days (`Db::prune_host_reports`,
//!   )
//! - filesystem `state.db.pre-*` pre-migration backups past 14 days
//!   (#51 — refinery / module activation creates these for safety;
//!   they're vestigial after a couple of weeks)
//!
//! All helpers are idempotent — the task can be killed at any tick
//! boundary without losing semantics. Mirrors the rollback-timer
//! shape so operators see `prune` lines in the same JSON-line journal
//! they already follow.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::db::Db;

const TICK_INTERVAL: Duration = Duration::from_secs(60 * 60);
const TOKEN_REPLAY_RETENTION_HOURS: i64 = 24;
const PENDING_CONFIRMS_RETENTION_HOURS: i64 = 24 * 7;
const HOST_REPORTS_RETENTION_HOURS: i64 = 24 * 7;
const BACKUP_RETENTION_DAYS: u64 = 14;
const BACKUP_FILENAME_PREFIX: &str = "state.db.pre-";

/// Spawn the periodic prune task. Runs forever; one INFO line per
/// tick summarising what was pruned. Failures are non-fatal — the
/// task logs a warn + continues with the next tick.
///
/// `db_path` enables the filesystem backup sweep: when set, the task
/// also deletes `state.db.pre-*` siblings older than
/// [`BACKUP_RETENTION_DAYS`] from the DB's parent directory. Pass
/// `None` for in-memory deployments / tests that don't have a backing
/// file.
pub fn spawn(db: Arc<Db>, db_path: Option<PathBuf>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TICK_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            ticker.tick().await;
            let token_pruned = try_prune("token_replay", || {
                db.prune_token_replay(TOKEN_REPLAY_RETENTION_HOURS)
            });
            let pending_pruned = try_prune("pending_confirms", || {
                db.prune_pending_confirms(PENDING_CONFIRMS_RETENTION_HOURS)
            });
            let reports_pruned = try_prune("host_reports", || {
                db.prune_host_reports(HOST_REPORTS_RETENTION_HOURS)
            });
            let backups_pruned = db_path
                .as_deref()
                .and_then(Path::parent)
                .map(|parent| {
                    try_prune("state.db backup sweep", || {
                        prune_backup_files(parent, BACKUP_FILENAME_PREFIX, BACKUP_RETENTION_DAYS)
                    })
                })
                .unwrap_or(0);
            tracing::info!(
                target: "prune",
                token_replay = token_pruned,
                pending_confirms = pending_pruned,
                host_reports = reports_pruned,
                state_db_backups = backups_pruned,
                "prune timer: hourly sweep complete",
            );
        }
    })
}

/// Run a prune step. On `Ok(n)` returns `n`; on `Err` logs a `warn`
/// with the step's `name` and returns 0 — failures are non-fatal so
/// the sweep continues to the next step.
fn try_prune<E>(name: &str, f: impl FnOnce() -> std::result::Result<usize, E>) -> usize
where
    E: std::fmt::Display,
{
    match f() {
        Ok(n) => n,
        Err(err) => {
            tracing::warn!(error = %err, "prune timer: {name} failed");
            0
        }
    }
}

/// Delete files in `parent` whose basename starts with `prefix` and
/// whose mtime is older than `retention_days`. Returns the count of
/// files actually deleted.
///
/// Errors during enumeration propagate (e.g. parent dir missing); a
/// per-file delete error is logged and counted as not-pruned but does
/// not abort the sweep.
pub(crate) fn prune_backup_files(
    parent: &Path,
    prefix: &str,
    retention_days: u64,
) -> std::io::Result<usize> {
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(retention_days * 24 * 60 * 60))
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let mut deleted = 0usize;
    let entries = match std::fs::read_dir(parent) {
        Ok(it) => it,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(err),
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!(error = %err, "prune timer: read_dir entry failed");
                continue;
            }
        };
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else { continue };
        if !name_str.starts_with(prefix) {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(err) => {
                tracing::warn!(
                    file = %name_str,
                    error = %err,
                    "prune timer: backup metadata failed",
                );
                continue;
            }
        };
        if !metadata.is_file() {
            continue;
        }
        let mtime = match metadata.modified() {
            Ok(t) => t,
            Err(err) => {
                tracing::warn!(
                    file = %name_str,
                    error = %err,
                    "prune timer: backup mtime unavailable",
                );
                continue;
            }
        };
        if mtime >= cutoff {
            continue;
        }
        let path = entry.path();
        match std::fs::remove_file(&path) {
            Ok(()) => {
                tracing::info!(
                    target: "prune",
                    file = %path.display(),
                    "pruned stale state.db backup",
                );
                deleted += 1;
            }
            Err(err) => {
                tracing::warn!(
                    file = %path.display(),
                    error = %err,
                    "prune timer: backup delete failed",
                );
            }
        }
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn touch(path: &Path, age: Duration) {
        let f = std::fs::File::create(path).unwrap();
        f.set_modified(SystemTime::now() - age).unwrap();
    }

    #[test]
    fn prune_backup_files_drops_old_keeps_young() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("state.db.pre-phase2-20240101-000000");
        let young = dir.path().join("state.db.pre-phase2-20260430-235959");
        let unrelated = dir.path().join("state.db");
        touch(&old, Duration::from_secs(30 * 24 * 60 * 60));
        touch(&young, Duration::from_secs(60));
        touch(&unrelated, Duration::from_secs(30 * 24 * 60 * 60));

        let pruned = prune_backup_files(dir.path(), "state.db.pre-", 14).unwrap();
        assert_eq!(pruned, 1);
        assert!(!old.exists(), "old backup should be deleted");
        assert!(young.exists(), "young backup should be kept");
        assert!(unrelated.exists(), "non-backup file should be untouched");
    }

    #[test]
    fn prune_backup_files_returns_zero_when_dir_missing() {
        let n = prune_backup_files(Path::new("/nonexistent/path/that/should/not/exist"), "state.db.pre-", 14).unwrap();
        assert_eq!(n, 0);
    }
}
