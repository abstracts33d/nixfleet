//! Darwin (nix-darwin) activation primitives.
//!
//! Compiled only on `target_os = "macos"`. The `activation` parent
//! module exports `DarwinBackend` as the cfg-selected `DefaultBackend`
//! type alias; callers in the rest of the agent never see
//! `cfg(target_os)`.
//!
//! Platform contract:
//!
//! - `is_switch_in_progress` always returns `false` — Darwin has no
//!   equivalent to NixOS's `/run/nixos/switch-to-configuration.lock`.
//!   Nothing serialises concurrent darwin activations today; if a
//!   future tool adds a lock primitive, wire it here.
//! - `read_unit_exit_code` always returns `None` — there's no
//!   systemd surface to query. The agent's poll loop is the
//!   authoritative success signal on darwin.
//! - `fire_switch` runs `<store>/activate-user` (legacy; modern
//!   closures often omit it) followed by `<store>/activate`, both
//!   `setsid`-detached so launchd's process-group SIGTERM during
//!   plist reload doesn't propagate to the activation child.
//!   `nohup` doesn't work in launchd-daemon context (no controlling
//!   TTY); only `setsid` gives the survivable session.
//! - `fire_rollback` runs `<store>/activate` for the rolled-back
//!   target, same setsid-detached pattern.

use std::process::Stdio;

use anyhow::Result;
use nixfleet_proto::agent_wire::EvaluatedTarget;

use super::{ActivationBackend, ActivationOutcome, RollbackOutcome};

/// Unit-struct backend; method bodies hold the darwin-specific logic.
#[derive(Clone, Copy, Debug, Default)]
pub struct DarwinBackend;

impl ActivationBackend for DarwinBackend {
    async fn is_switch_in_progress(&self) -> bool {
        false
    }
    async fn read_unit_exit_code(&self, _unit_name: &str) -> Option<i32> {
        None
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

/// `setsid` puts the activate child in its own session so launchd's
/// process-group SIGTERM (issued during plist reload when the new
/// closure changes the agent binary path) doesn't propagate to it.
async fn fire_switch(
    target: &EvaluatedTarget,
    store_path: &str,
) -> Result<Option<ActivationOutcome>> {
    use std::os::unix::process::CommandExt;

    tracing::info!(
        target_closure = %target.closure_hash,
        "agent: firing darwin activation (setsid-detached activate-user + activate)",
    );

    // Step 1: activate-user (legacy; modern closures often omit it).
    // Errors here are non-fatal.
    let activate_user = format!("{store_path}/activate-user");
    if std::path::Path::new(&activate_user).exists() {
        let mut cmd = std::process::Command::new(&activate_user);
        cmd.stdin(Stdio::null());
        attach_activate_log(&mut cmd);
        // SAFETY: setsid is async-signal-safe; closure does no
        // allocation or lock acquisition.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        match cmd.spawn() {
            Ok(_child) => {
                tracing::debug!(
                    target_closure = %target.closure_hash,
                    "agent: darwin activate-user fired (detached)",
                );
            }
            Err(err) => {
                tracing::warn!(
                    target_closure = %target.closure_hash,
                    error = %err,
                    "agent: darwin activate-user spawn failed (non-fatal); continuing to system activate",
                );
            }
        }
    } else {
        tracing::debug!(
            target_closure = %target.closure_hash,
            "agent: darwin activate-user absent; skipping (modern closure shape)",
        );
    }

    // Step 2: system activate. May unload+reload the launchd plist,
    // killing the agent if the binary path changed. setsid keeps the
    // child alive; if the agent dies, launchd restarts it and
    // `recovery::run_boot_recovery` posts the retroactive confirm.
    let activate = format!("{store_path}/activate");
    let mut cmd = std::process::Command::new(&activate);
    cmd.stdin(Stdio::null());
    attach_activate_log(&mut cmd);
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    match cmd.spawn() {
        Ok(_child) => {
            tracing::info!(
                target_closure = %target.closure_hash,
                "agent: darwin activate fired (setsid-detached); polling current-system",
            );
            Ok(None)
        }
        Err(err) => {
            tracing::error!(
                target_closure = %target.closure_hash,
                error = %err,
                "agent: darwin activate spawn failed",
            );
            Ok(Some(ActivationOutcome::SwitchFailed {
                phase: "darwin-activate-spawn".to_string(),
                exit_code: None,
            }))
        }
    }
}

async fn fire_rollback(target_basename: &str) -> Result<Option<RollbackOutcome>> {
    use std::os::unix::process::CommandExt;

    let store_path = format!("/nix/store/{target_basename}");
    let activate = format!("{store_path}/activate");
    if !std::path::Path::new(&activate).exists() {
        tracing::error!(
            activate = %activate,
            "agent: darwin rollback target has no activate script",
        );
        return Ok(Some(RollbackOutcome::Failed {
            phase: "darwin-activate-missing".to_string(),
            exit_code: None,
        }));
    }

    tracing::info!(
        target = %target_basename,
        "agent: firing darwin rollback (setsid-detached activate)",
    );
    let mut cmd = std::process::Command::new(&activate);
    cmd.stdin(Stdio::null());
    attach_activate_log(&mut cmd);
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    match cmd.spawn() {
        Ok(_child) => Ok(None),
        Err(err) => {
            tracing::error!(
                target = %target_basename,
                error = %err,
                "agent: darwin rollback activate spawn failed",
            );
            Ok(Some(RollbackOutcome::Failed {
                phase: "darwin-activate-spawn".to_string(),
                exit_code: None,
            }))
        }
    }
}

/// Falls back to inherit on permission/IO error; launchd's
/// StandardOutPath/StandardErrorPath catches the inherited stream.
fn attach_activate_log(cmd: &mut std::process::Command) {
    const ACTIVATE_LOG: &str = "/var/log/nixfleet-activate.log";
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(ACTIVATE_LOG)
    {
        Ok(out) => {
            // stdout + stderr each consume one handle.
            let err = match out.try_clone() {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        path = ACTIVATE_LOG,
                        error = %e,
                        "could not clone activate log handle; using inherit",
                    );
                    cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());
                    return;
                }
            };
            cmd.stdout(out).stderr(err);
        }
        Err(e) => {
            tracing::warn!(
                path = ACTIVATE_LOG,
                error = %e,
                "could not open activate log; using inherit",
            );
            cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());
        }
    }
}
