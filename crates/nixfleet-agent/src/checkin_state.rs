//! System introspection for checkin body assembly.
//!
//! Reads what the agent reports about itself: closure hash, pending
//! generation, boot ID. All file I/O is `std::fs::*` — these are
//! tiny reads of /run + /proc, no async needed.

use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use nixfleet_proto::agent_wire::{GenerationRef, PendingGeneration};

/// Filename inside `--state-dir` that carries the agent's most
/// recent successful confirm. Two-line plaintext format:
///   `<closure_hash>\n<rfc3339-timestamp>\n`
/// Written after every Acknowledged `/v1/agent/confirm`; read on
/// every checkin. The closure_hash binds the timestamp to the
/// generation it applies to — if the agent rolls back (current
/// generation no longer matches the recorded closure), the
/// timestamp is suppressed from the next checkin.
///
/// Closes the agent half of gap B (issue #47): the CP-side
/// projection ([`crates/nixfleet-control-plane/src/server/handlers.rs`]
/// `recover_soak_state_from_attestation`) consumes
/// `CheckinRequest.last_confirmed_at` to repopulate
/// `host_rollout_state.last_healthy_since` after a CP rebuild,
/// clamped to `min(now, attested)`.
pub const LAST_CONFIRM_FILENAME: &str = "last_confirmed_at";

/// Filename inside `--state-dir` that carries the most-recently
/// dispatched target. Written by `main.rs` after a checkin response
/// returns a target, BEFORE `activate()` is called. Read at agent
/// startup by `check_boot_recovery()` to detect "we got killed
/// mid-self-switch but the new closure is now live" — in which case
/// the agent posts the retroactive `/v1/agent/confirm` instead of
/// waiting for the next checkin to re-dispatch.
///
/// Format is JSON-serialized [`LastDispatchRecord`] (atomic
/// tempfile + rename write, same as `last_confirmed_at`). JSON over
/// the line-oriented format used by `last_confirmed_at` because the
/// dispatch record carries multiple fields whose sizes/escaping
/// would be awkward to line-encode (channel_ref containing `@`,
/// future fields).
///
/// Closes the agent half of ADR-011's "boot recovery" path. With
/// fire-and-forget activation, the agent commonly gets killed
/// mid-poll when the new closure restarts `nixfleet-agent.service`
/// — the post-self-switch agent boots into the new closure and
/// needs to know "what was I dispatching" to confirm it.
pub const LAST_DISPATCH_FILENAME: &str = "last_dispatched";

/// Persisted record of the most-recently dispatched target.
///
/// Carries enough fields to reconstruct a `confirm_target()` call
/// after an agent restart. Channel + closure are the wire-essential
/// keys; `rollout_id` is included for diagnostic correlation but
/// the CP also re-derives it from the channel + closure on confirm.
/// `dispatched_at` is the agent's wall-clock at write time —
/// purely diagnostic.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct LastDispatchRecord {
    pub closure_hash: String,
    pub channel_ref: String,
    /// The rollout id the CP populated on the EvaluatedTarget.
    /// `None` for legacy/synthetic targets (matches the proto's
    /// optional shape).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollout_id: Option<String>,
    pub dispatched_at: DateTime<Utc>,
}

/// Persist a freshly-dispatched target before firing activation.
/// Atomic via tempfile + rename so a crash mid-write doesn't leave
/// a partial file. Best-effort on the caller's side: failures log,
/// non-fatal — the worst case is the boot-recovery path can't
/// retroactively confirm and the next regular checkin re-dispatches
/// (which is correct behavior, just one cycle slower).
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

/// Read the persisted last-dispatch record, if present and well-formed.
/// Returns `Ok(None)` for both "file absent" (first boot, never
/// dispatched) and "file malformed" (treat as if we'd never written
/// it — the next checkin will re-dispatch). Errors only on
/// filesystem I/O failures.
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

/// Remove the persisted last-dispatch record. Called after a
/// successful confirm so the file doesn't linger past its useful
/// lifetime. Idempotent — absent file returns `Ok`.
pub fn clear_last_dispatched(state_dir: &Path) -> Result<()> {
    let path = state_dir.join(LAST_DISPATCH_FILENAME);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("remove {}", path.display())),
    }
}

/// Path to the symlink pointing at the currently active system
/// closure. Reading it as a symlink target gives us the store
/// path; the basename of that path IS the closure_hash on the
/// wire (the same shape the CP populates into `EvaluatedTarget.
/// closure_hash` and `fleet.resolved.hosts[h].closureHash`). The
/// agent must report the FULL basename, not the 32-char hash
/// prefix — `dispatch::decide_target` does string-equality on the
/// two values; a hash-prefix-vs-full-basename mismatch means
/// dispatch always returns Decision::Dispatch even when the host
/// is on the declared closure.
const CURRENT_SYSTEM: &str = "/run/current-system";

/// Path to the symlink pointing at the system that booted. When
/// this differs from `/run/current-system`, the host has a pending
/// generation queued for next reboot.
const BOOTED_SYSTEM: &str = "/run/booted-system";

/// Linux's per-boot UUID. Stable for a single boot; rotates on
/// reboot. Used by the CP to detect that a host actually rebooted
/// (e.g. correlated with `pendingGeneration` clearing on next
/// checkin).
const BOOT_ID_PATH: &str = "/proc/sys/kernel/random/boot_id";

/// Read `/run/current-system`'s symlink target and extract the
/// store-path closure hash (the 32-char nix-store hash before the
/// `-` separator). Returns the full store path on platforms where
/// the symlink target shape doesn't match the expected pattern, so
/// the agent still reports something rather than failing the
/// checkin.
pub fn current_closure_hash() -> Result<String> {
    let target = std::fs::read_link(CURRENT_SYSTEM)
        .with_context(|| format!("readlink {CURRENT_SYSTEM}"))?;
    Ok(closure_hash_from_path(&target))
}

/// Same as [`current_closure_hash`] for `/run/booted-system`. The
/// caller compares the two to decide whether to populate
/// `pendingGeneration`.
fn booted_closure_hash() -> Result<String> {
    let target = std::fs::read_link(BOOTED_SYSTEM)
        .with_context(|| format!("readlink {BOOTED_SYSTEM}"))?;
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
fn closure_hash_from_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    s.rsplit('/')
        .next()
        .map(str::to_string)
        .unwrap_or_else(|| s.to_string())
}

/// Read `/proc/sys/kernel/random/boot_id`. The file is a single
/// UUID + newline; we trim and return.
pub fn boot_id() -> Result<String> {
    let raw = std::fs::read_to_string(BOOT_ID_PATH)
        .with_context(|| format!("read {BOOT_ID_PATH}"))?;
    Ok(raw.trim().to_string())
}

/// Build the `currentGeneration` GenerationRef. `channel_ref` is
/// `None` until the agent's channel is correlated by the projection.
pub fn current_generation_ref() -> Result<GenerationRef> {
    Ok(GenerationRef {
        closure_hash: current_closure_hash()?,
        channel_ref: None,
        boot_id: boot_id()?,
    })
}

/// Build the `pendingGeneration` PendingGeneration when
/// `/run/booted-system` differs from `/run/current-system`. Returns
/// `Ok(None)` when they match (no pending), `Err` only on read
/// failures of either symlink.
pub fn pending_generation() -> Result<Option<PendingGeneration>> {
    let current = current_closure_hash()?;
    let booted = booted_closure_hash()?;
    if current == booted {
        return Ok(None);
    }
    Ok(Some(PendingGeneration {
        closure_hash: current,
        scheduled_for: None,
    }))
}

/// Wall-clock seconds since the agent process started. The caller
/// passes the start `Instant` (captured in `main` before the poll
/// loop starts).
pub fn uptime_secs(started_at: Instant) -> u64 {
    started_at.elapsed().as_secs()
}

/// Persist the moment of a successful confirm so the next checkin
/// can attest it (and it survives an agent restart). Writes
/// atomically via `tempfile + rename` so a crash mid-write doesn't
/// leave a partially-written file. Best-effort: failures log + are
/// non-fatal — soak attestation is recovery hygiene, not the
/// activation contract.
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

/// Read the persisted last-confirm timestamp, if it applies to the
/// agent's current generation. Returns `None` when:
/// - the state file doesn't exist (first boot, never confirmed);
/// - the file's recorded `closure_hash` differs from the agent's
///   current generation (host rolled back; the timestamp no longer
///   describes the live system);
/// - the file is malformed (parse error, missing line);
/// - the timestamp is in the future of `now` (clock skew or
///   tamper — we'd be attesting future-dated state which the CP
///   clamps to `min(now, attested)` anyway, so just suppress).
///
/// Errors only on filesystem I/O failures; logical mismatches
/// return `Ok(None)` so the caller can include the field as
/// `Option::None` in the checkin without aborting the request.
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
