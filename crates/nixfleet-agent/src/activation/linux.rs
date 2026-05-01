//! Linux (NixOS) activation primitives.
//!
//! Compiled only on `target_os = "linux"`. The `activation` parent
//! module re-exports `fire_switch`, `fire_rollback`,
//! `is_switch_in_progress`, and `read_unit_exit_code` from this
//! module via `#[cfg(target_os = "linux")] pub use linux::*` —
//! callers in the rest of the agent never see `cfg(target_os)`.
//!
//! Platform contract:
//!
//! - `is_switch_in_progress` checks `/run/nixos/switch-to-configuration.lock`
//!   via `flock --nonblock`; absent file or missing binary → false
//!   (fail-open).
//! - `fire_switch` runs `switch-to-configuration switch` under a
//!   transient `systemd-run --unit=nixfleet-switch` so the agent's
//!   own SIGTERM (during plist-style restart on closure swap) can't
//!   kill the activation mid-run. Same rationale as
//!   `fire_rollback`.
//! - `read_unit_exit_code` queries `systemctl show
//!   --property=ExecMainStatus`; returns `None` when the value is
//!   empty / non-numeric / systemctl call fails.

use anyhow::{Context, Result};
use nixfleet_proto::agent_wire::EvaluatedTarget;
use tokio::process::Command;

use super::{ActivationOutcome, RollbackOutcome};

/// Held exclusive by any running `switch-to-configuration`
/// (nixos-rebuild, our own systemd-run, operator manual run, etc.).
const SWITCH_LOCK_PATH: &str = "/run/nixos/switch-to-configuration.lock";

/// Returns `true` only when the lock file exists AND a non-blocking
/// `flock(1)` attempt fails. Absent file / missing binary → false
/// (fail-open).
pub async fn is_switch_in_progress() -> bool {
    if !std::path::Path::new(SWITCH_LOCK_PATH).exists() {
        return false;
    }
    let status = Command::new("flock")
        .arg("--nonblock")
        .arg("--shared")
        .arg(SWITCH_LOCK_PATH)
        .arg("true")
        .status()
        .await;
    match status {
        // flock acquired and released the lock immediately — no contender.
        Ok(s) if s.success() => false,
        // flock exited non-zero (typically code=1, "lock contended").
        Ok(_) => true,
        // flock binary missing or spawn failed → fail-open.
        Err(_) => false,
    }
}

/// Returns `None` on failure / empty / non-numeric — caller treats
/// as unknown rather than synthesising a misleading 0.
pub async fn read_unit_exit_code(unit_name: &str) -> Option<i32> {
    let output = Command::new("systemctl")
        .arg("show")
        .arg("--property=ExecMainStatus")
        .arg("--value")
        .arg(unit_name)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed.parse::<i32>().ok()
}

/// `systemd-run --unit=...` creates an independent transient service
/// with its own cgroup, so the agent's death (from the switch script
/// restarting nixfleet-agent.service) cannot kill it. `--scope` and
/// `--pipe --wait` both inherit the caller's cgroup and die with it.
/// `--collect` reuses the fixed unit name across activations;
/// `reset-failed` (idempotent) handles the case where a prior run
/// left the unit in failed state.
///
/// `Ok(None)` on clean fire (caller polls); `Ok(Some(outcome))` on
/// fire-step failure; `Err` only on spawn-level I/O errors.
pub async fn fire_switch(
    target: &EvaluatedTarget,
    store_path: &str,
) -> Result<Option<ActivationOutcome>> {
    let _ = Command::new("systemctl")
        .arg("reset-failed")
        .arg("nixfleet-switch.service")
        .status()
        .await;

    let switch_bin = format!("{store_path}/bin/switch-to-configuration");
    tracing::info!(
        target_closure = %target.closure_hash,
        "agent: firing switch via systemd-run --unit=nixfleet-switch (detached)",
    );
    let fire_status = Command::new("systemd-run")
        .arg("--unit=nixfleet-switch")
        .arg("--collect")
        .arg("--")
        .arg(&switch_bin)
        .arg("switch")
        .status()
        .await
        .with_context(|| "spawn systemd-run --unit=nixfleet-switch")?;

    if !fire_status.success() {
        tracing::error!(
            target_closure = %target.closure_hash,
            exit_code = ?fire_status.code(),
            "agent: systemd-run failed to queue switch unit",
        );
        return Ok(Some(ActivationOutcome::SwitchFailed {
            phase: "systemd-run-fire".to_string(),
            exit_code: fire_status.code(),
        }));
    }
    Ok(None)
}

/// `nix-env --rollback` already re-pointed
/// `/run/current-system`, so this fires its switch-to-configuration
/// to actually run activation. `_target_basename` is unused on linux
/// (the profile flip is the source of truth) — kept on the wire so
/// the parent module's dispatch is uniform across platforms.
pub async fn fire_rollback(_target_basename: &str) -> Result<Option<RollbackOutcome>> {
    let _ = Command::new("systemctl")
        .arg("reset-failed")
        .arg("nixfleet-rollback.service")
        .status()
        .await;

    let switch_bin = "/run/current-system/bin/switch-to-configuration";
    let fire_status = Command::new("systemd-run")
        .arg("--unit=nixfleet-rollback")
        .arg("--collect")
        .arg("--")
        .arg(switch_bin)
        .arg("switch")
        .status()
        .await
        .with_context(|| "spawn systemd-run --unit=nixfleet-rollback")?;

    if !fire_status.success() {
        tracing::error!(
            exit_code = ?fire_status.code(),
            "agent: systemd-run failed to queue rollback unit",
        );
        return Ok(Some(RollbackOutcome::Failed {
            phase: "systemd-run-fire".to_string(),
            exit_code: fire_status.code(),
        }));
    }
    Ok(None)
}
