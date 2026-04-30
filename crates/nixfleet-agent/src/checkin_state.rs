//! Persistence + shared closure-hash helpers for the checkin body.
//!
//! Platform-specific introspection (`boot_id`, `pending_generation`)
//! lives behind the `HostFacts` trait in `crate::host_facts`. This
//! module owns:
//!
//! - the `/run/current-system` reader (works the same on Linux and
//!   nix-darwin), and
//! - the on-disk persistence of last-confirm + last-dispatch state.

use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

/// Two-line plaintext: `<closure_hash>\n<rfc3339-timestamp>\n`.
/// closure_hash binds the timestamp to its generation — agent
/// rollback suppresses the timestamp on next checkin. CP repopulates
/// `host_rollout_state.last_healthy_since` from this attestation
/// after a rebuild, clamped to `min(now, attested)`.
pub const LAST_CONFIRM_FILENAME: &str = "last_confirmed_at";

/// JSON [`LastDispatchRecord`] (atomic tempfile + rename). Written
/// after dispatch, BEFORE activate. Read at agent startup to detect
/// "killed mid-self-switch but the new closure is now live" — agent
/// posts the retroactive confirm instead of waiting for re-dispatch.
pub const LAST_DISPATCH_FILENAME: &str = "last_dispatched";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct LastDispatchRecord {
    pub closure_hash: String,
    pub channel_ref: String,
    /// None for legacy/synthetic targets (CP also re-derives it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollout_id: Option<String>,
    pub dispatched_at: DateTime<Utc>,
}

/// Atomic tempfile + rename. Best-effort: failures non-fatal — worst
/// case the boot-recovery path can't retroactively confirm and the
/// next checkin re-dispatches (one cycle slower).
pub fn write_last_dispatched(state_dir: &Path, record: &LastDispatchRecord) -> Result<()> {
    std::fs::create_dir_all(state_dir)
        .with_context(|| format!("create state dir {}", state_dir.display()))?;
    let final_path = state_dir.join(LAST_DISPATCH_FILENAME);
    let tmp_path = state_dir.join(format!("{LAST_DISPATCH_FILENAME}.tmp"));
    let body = serde_json::to_string(record).context("serialize LastDispatchRecord")?;
    std::fs::write(&tmp_path, body)
        .with_context(|| format!("write {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &final_path).with_context(|| {
        format!(
            "rename {} -> {}",
            tmp_path.display(),
            final_path.display()
        )
    })?;
    Ok(())
}

/// `Ok(None)` for both absent and malformed (next checkin
/// re-dispatches). Errors only on FS I/O failures.
pub fn read_last_dispatched(state_dir: &Path) -> Result<Option<LastDispatchRecord>> {
    let path = state_dir.join(LAST_DISPATCH_FILENAME);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    match serde_json::from_str::<LastDispatchRecord>(&raw) {
        Ok(rec) => Ok(Some(rec)),
        Err(_) => Ok(None),
    }
}

/// Idempotent — absent file returns `Ok`.
pub fn clear_last_dispatched(state_dir: &Path) -> Result<()> {
    let path = state_dir.join(LAST_DISPATCH_FILENAME);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("remove {}", path.display())),
    }
}

/// closure_hash on the wire is the FULL `/nix/store` basename, not
/// the 32-char hash prefix — `dispatch::decide_target` does string
/// equality, mismatch causes infinite re-dispatch.
const CURRENT_SYSTEM: &str = "/run/current-system";

/// Works on both NixOS and nix-darwin (both materialise the symlink).
pub fn current_closure_hash() -> Result<String> {
    let target = std::fs::read_link(CURRENT_SYSTEM)
        .with_context(|| format!("readlink {CURRENT_SYSTEM}"))?;
    Ok(closure_hash_from_path(&target))
}

/// Extract the closure-hash identifier from a `/nix/store/<basename>`
/// path. Returns the full basename (e.g.
/// `2zlnf66xlf35xwm7150kx05q93cwp8jk-nixos-system-lab-…`), NOT the
/// 32-char hash prefix. The basename is the wire identifier shared
/// across the proto: `EvaluatedTarget.closure_hash` (CP → agent),
/// `fleet.resolved.hosts[h].closureHash` (CI → CP), and
/// `CheckinRequest.current_generation.closure_hash` (agent → CP)
/// all carry it in the same shape. `dispatch::decide_target` does
/// string-equality between them; any normalisation drift here
/// means converged hosts look diverged forever.
///
/// Falls back to the full path string if the shape doesn't match,
/// so the field is always populated.
pub(crate) fn closure_hash_from_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    s.rsplit('/')
        .next()
        .map(str::to_string)
        .unwrap_or_else(|| s.to_string())
}

pub fn uptime_secs(started_at: Instant) -> u64 {
    started_at.elapsed().as_secs()
}

/// Atomic tempfile + rename. Best-effort: failures non-fatal — soak
/// attestation is recovery hygiene, not the activation contract.
pub fn write_last_confirmed(state_dir: &Path, closure_hash: &str, at: DateTime<Utc>) -> Result<()> {
    std::fs::create_dir_all(state_dir)
        .with_context(|| format!("create state dir {}", state_dir.display()))?;
    let final_path = state_dir.join(LAST_CONFIRM_FILENAME);
    let tmp_path = state_dir.join(format!("{LAST_CONFIRM_FILENAME}.tmp"));
    let body = format!("{closure_hash}\n{}\n", at.to_rfc3339());
    std::fs::write(&tmp_path, body)
        .with_context(|| format!("write {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &final_path).with_context(|| {
        format!(
            "rename {} -> {}",
            tmp_path.display(),
            final_path.display()
        )
    })?;
    Ok(())
}

/// `None` when: file absent (first boot), recorded closure differs
/// from current (rolled back), file malformed, or timestamp is
/// future-dated (clock skew/tamper — CP clamps anyway).
pub fn read_last_confirmed(
    state_dir: &Path,
    current_closure: &str,
    now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>> {
    let path = state_dir.join(LAST_CONFIRM_FILENAME);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    let mut lines = raw.lines();
    let recorded_closure = match lines.next() {
        Some(s) if !s.is_empty() => s,
        _ => return Ok(None),
    };
    let recorded_ts = match lines.next() {
        Some(s) if !s.is_empty() => s,
        _ => return Ok(None),
    };
    if recorded_closure != current_closure {
        return Ok(None);
    }
    let parsed: DateTime<Utc> = match recorded_ts.parse() {
        Ok(t) => t,
        Err(_) => return Ok(None),
    };
    if parsed > now {
        return Ok(None);
    }
    Ok(Some(parsed))
}

#[cfg(test)]
mod write_read_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn write_then_read_round_trips_when_closure_matches() {
        let dir = TempDir::new().unwrap();
        let now = Utc::now();
        let stamp = now - chrono::Duration::seconds(30);
        write_last_confirmed(dir.path(), "abc-system", stamp).unwrap();
        let got = read_last_confirmed(dir.path(), "abc-system", now)
            .unwrap()
            .expect("present");
        assert_eq!(got.timestamp(), stamp.timestamp());
    }

    #[test]
    fn read_returns_none_when_closure_mismatch() {
        let dir = TempDir::new().unwrap();
        let now = Utc::now();
        write_last_confirmed(dir.path(), "old-system", now).unwrap();
        let got = read_last_confirmed(dir.path(), "new-system", now).unwrap();
        assert!(
            got.is_none(),
            "rolled-back closure must not surface stale timestamp",
        );
    }

    #[test]
    fn read_returns_none_when_no_file() {
        let dir = TempDir::new().unwrap();
        let got = read_last_confirmed(dir.path(), "any", Utc::now()).unwrap();
        assert!(got.is_none(), "absent state file is the first-boot case");
    }

    #[test]
    fn read_returns_none_when_timestamp_future() {
        let dir = TempDir::new().unwrap();
        let now = Utc::now();
        let future = now + chrono::Duration::hours(1);
        write_last_confirmed(dir.path(), "abc-system", future).unwrap();
        let got = read_last_confirmed(dir.path(), "abc-system", now).unwrap();
        assert!(
            got.is_none(),
            "future-dated stamp suppressed (clock-skew / tamper guard)",
        );
    }

    #[test]
    fn read_returns_none_on_malformed_body() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(LAST_CONFIRM_FILENAME), "only-one-line").unwrap();
        let got = read_last_confirmed(dir.path(), "anything", Utc::now()).unwrap();
        assert!(got.is_none());
    }

    fn sample_dispatch() -> LastDispatchRecord {
        LastDispatchRecord {
            closure_hash: "abc-nixos-system".into(),
            channel_ref: "stable@deadbeef".into(),
            rollout_id: Some("stable@deadbeef".into()),
            dispatched_at: Utc::now(),
        }
    }

    #[test]
    fn last_dispatched_round_trips() {
        let dir = TempDir::new().unwrap();
        let r = sample_dispatch();
        write_last_dispatched(dir.path(), &r).unwrap();
        let got = read_last_dispatched(dir.path()).unwrap().expect("present");
        assert_eq!(got.closure_hash, r.closure_hash);
        assert_eq!(got.channel_ref, r.channel_ref);
        assert_eq!(got.rollout_id, r.rollout_id);
    }

    #[test]
    fn last_dispatched_absent_returns_none() {
        let dir = TempDir::new().unwrap();
        assert!(read_last_dispatched(dir.path()).unwrap().is_none());
    }

    #[test]
    fn last_dispatched_malformed_returns_none() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(LAST_DISPATCH_FILENAME), "{not-json").unwrap();
        // Caller treats malformed identically to absent — next checkin
        // re-dispatches.
        assert!(read_last_dispatched(dir.path()).unwrap().is_none());
    }

    #[test]
    fn clear_last_dispatched_is_idempotent() {
        let dir = TempDir::new().unwrap();
        // Absent file: ok.
        clear_last_dispatched(dir.path()).unwrap();
        // Present file: ok + removed.
        write_last_dispatched(dir.path(), &sample_dispatch()).unwrap();
        clear_last_dispatched(dir.path()).unwrap();
        assert!(read_last_dispatched(dir.path()).unwrap().is_none());
        // Calling again on absent: still ok.
        clear_last_dispatched(dir.path()).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closure_hash_is_full_basename_not_hash_prefix() {
        // Regression: agent used to strip after the first '-' and
        // report just the 32-char hash. CP populates
        // `fleet.resolved.hosts[h].closureHash` with the FULL
        // basename, so the dispatch comparison was string-not-equal
        // even on converged hosts → infinite re-dispatch loop on
        // every checkin (caught on lab when id=14 dispatched the
        // same target the agent had just confirmed in id=13).
        let p: PathBuf =
            "/nix/store/2zlnf66xlf35xwm7150kx05q93cwp8jk-nixos-system-lab-20260427-0810_5176864f_turbo-otter"
                .into();
        let got = closure_hash_from_path(&p);
        assert_eq!(
            got,
            "2zlnf66xlf35xwm7150kx05q93cwp8jk-nixos-system-lab-20260427-0810_5176864f_turbo-otter",
            "closure_hash must be the full /nix/store basename — same shape the CP declares",
        );
        // Sanity: prefix-only would have been this 32-char string.
        assert_ne!(got, "2zlnf66xlf35xwm7150kx05q93cwp8jk");
    }

    #[test]
    fn closure_hash_falls_back_to_full_path_for_non_store_shape() {
        let p: PathBuf = "/some/odd/path".into();
        let got = closure_hash_from_path(&p);
        assert_eq!(got, "path", "rsplit/next still returns the leaf");
    }
}
