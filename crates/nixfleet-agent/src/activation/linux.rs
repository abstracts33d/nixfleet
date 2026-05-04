//! Linux (NixOS) activation primitives. fire_* uses `systemd-run --unit=...`
//! so the agent's SIGTERM can't kill the activation mid-run.

use std::path::Path;

use anyhow::{Context, Result};
use nixfleet_proto::agent_wire::EvaluatedTarget;
use tokio::process::Command;

use super::{ActivationBackend, ActivationOutcome, RollbackOutcome};

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

const SWITCH_LOCK_PATH: &str = "/run/nixos/switch-to-configuration.lock";

/// Fail-open: absent lock file or missing flock binary → false.
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
        Ok(s) if s.success() => false,
        Ok(_) => true,
        Err(_) => false,
    }
}

/// `None` on failure / empty / non-numeric (never synthesise a misleading 0).
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

// FOOTGUN: --scope / --pipe --wait inherit caller's cgroup — agent SIGTERM kills the switch. Use --unit.
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

/// `_target_basename` is unused on linux (profile flip is source of truth);
/// kept in the signature for cross-platform dispatch uniformity.
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
        let dir = tempfile::tempdir().expect("tempdir");
        let lock = dir.path().join("test.lock");
        std::fs::write(&lock, b"").expect("create lock file");
        assert!(!is_switch_in_progress_at(&lock).await);
    }

    #[test]
    #[allow(clippy::default_constructed_unit_structs)]
    fn linux_backend_default_is_unit_struct() {
        let _b: LinuxBackend = LinuxBackend;
        let _: LinuxBackend = LinuxBackend::default();
    }
}
