//! System introspection for checkin body assembly.
//!
//! Reads what the agent reports about itself: closure hash, pending
//! generation, boot ID. All file I/O is `std::fs::*` — these are
//! tiny reads of /run + /proc, no async needed.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use nixfleet_proto::agent_wire::{GenerationRef, PendingGeneration};

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
fn closure_hash_from_path(p: &PathBuf) -> String {
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
