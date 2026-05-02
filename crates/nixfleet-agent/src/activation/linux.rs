//! Linux (NixOS) activation primitives.
//!
//! Compiled only on `target_os = "linux"`. The `activation` parent
//! module exports `LinuxBackend` as the cfg-selected `DefaultBackend`
//! type alias; callers in the rest of the agent never see
//! `cfg(target_os)`.
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

use std::path::Path;

use anyhow::{Context, Result};
use nixfleet_proto::agent_wire::EvaluatedTarget;
use tokio::process::Command;

use super::{ActivationBackend, ActivationOutcome, RollbackOutcome};

/// Unit-struct backend; method bodies hold the linux-specific logic.
#[derive(Clone, Copy, Debug, Default)]
pub struct LinuxBackend;

impl ActivationBackend for LinuxBackend {
    async fn is_switch_in_progress(&self) -> bool {
        is_switch_in_progress().await
    }
    async fn read_unit_exit_code(&self, unit_name: &str) -> Option<i32> {
        read_unit_exit_code(unit_name).await
    }
    async fn fire_switch(
        &self,
        target: &EvaluatedTarget,
        store_path: &str,
    ) -> Result<Option<ActivationOutcome>> {
        fire_switch(target, store_path).await
    }
    async fn fire_rollback(&self, target_basename: &str) -> Result<Option<RollbackOutcome>> {
        fire_rollback(target_basename).await
    }
}

/// Held exclusive by any running `switch-to-configuration`
/// (nixos-rebuild, our own systemd-run, operator manual run, etc.).
const SWITCH_LOCK_PATH: &str = "/run/nixos/switch-to-configuration.lock";

/// Returns `true` only when the lock file exists AND a non-blocking
/// `flock(1)` attempt fails. Absent file / missing binary → false
/// (fail-open).
async fn is_switch_in_progress() -> bool {
    is_switch_in_progress_at(Path::new(SWITCH_LOCK_PATH)).await
}

async fn is_switch_in_progress_at(lock_path: &Path) -> bool {
    if !lock_path.exists() {
        return false;
    }
    let status = Command::new("flock")
        .arg("--nonblock")
        .arg("--shared")
        .arg(lock_path)
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
async fn read_unit_exit_code(unit_name: &str) -> Option<i32> {
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
async fn fire_switch(
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
async fn fire_rollback(_target_basename: &str) -> Result<Option<RollbackOutcome>> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn is_switch_in_progress_returns_false_when_lock_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let absent = dir.path().join("does-not-exist.lock");
        assert!(!is_switch_in_progress_at(&absent).await);
    }

    #[tokio::test]
    async fn is_switch_in_progress_returns_false_for_uncontended_lock() {
        // File exists but no process holds it — flock --nonblock --shared
        // acquires + releases immediately, returning success → not in progress.
        let dir = tempfile::tempdir().expect("tempdir");
        let lock = dir.path().join("test.lock");
        std::fs::write(&lock, b"").expect("create lock file");
        // flock binary may be absent on darwin/CI minimal images; in that
        // case the spawn errors and we fail-open to false. Either way this
        // should report `false` (no contender).
        assert!(!is_switch_in_progress_at(&lock).await);
    }

    #[test]
    fn linux_backend_default_is_unit_struct() {
        // Pin: LinuxBackend is a zero-sized unit struct — switching it to
        // a non-Default would break the cfg-aliased DefaultBackend.
        let _b: LinuxBackend = LinuxBackend;
        let _: LinuxBackend = LinuxBackend::default();
    }
}
