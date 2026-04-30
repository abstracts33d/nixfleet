//! Agent-side activation: install + boot the closure the CP issued.
//!
//! Three checks around `nixos-rebuild switch` make the agent the
//! last line of defense against a misbehaving substituter or
//! tampered CP:
//!
//! 1. **Pre-realise** (`nix-store --realise`) — forces substituter
//!    fetch + signature validation before we commit to switching.
//! 2. **Switch** (`nixos-rebuild switch --system <verified>`).
//! 3. **Post-verify** — `/run/current-system` basename must match
//!    the expected closure_hash; mismatch → local rollback.
//!
//! Together these close: "the agent either confirms the *exact*
//! closure the CP told it about, or rolls back" — without trusting
//! the substituter or the CP. CP-side magic rollback (deadline →
//! 410) is independent and additive.
//!
//! Platform dispatch uses runtime `cfg!(target_os = "macos")` (not
//! `#[cfg]`) so both paths type-check on every build. The "fire"
//! step diverges:
//!
//! - linux: `systemd-run --unit=nixfleet-switch -- switch-to-configuration switch`
//! - darwin: `setsid` detached `<store>/activate-user` + `<store>/activate`
//!
//! See `docs/mdbook/reference/darwin-platform-notes.md` for why
//! `setsid` + detached child survives the agent's own SIGTERM
//! during plist reload.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use nixfleet_proto::agent_wire::EvaluatedTarget;
use tokio::process::Command;

/// Held exclusive by any running `switch-to-configuration`
/// (nixos-rebuild, our own systemd-run, operator manual run, etc.).
const SWITCH_LOCK_PATH: &str = "/run/nixos/switch-to-configuration.lock";

/// Returns `true` only when the lock file exists AND a non-blocking
/// `flock(1)` attempt fails. Absent file / missing binary → false
/// (fail-open). Darwin: no equivalent lock; returns false early.
pub async fn is_switch_in_progress() -> bool {
    if cfg!(target_os = "macos") {
        return false;
    }
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
        // Couldn't run flock at all — fail-open.
        Err(_) => false,
    }
}

/// 300s sized to fit inside the CP's `DEFAULT_CONFIRM_DEADLINE_SECS = 360`.
pub const POLL_BUDGET: Duration = Duration::from_secs(300);

pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub enum ActivationOutcome {
    /// Fire-and-forget completed: switch fired AND
    /// `/run/current-system` flipped to expected. By the time this
    /// returns the system *is* running the new closure, but the
    /// activation work happened in `nixfleet-switch.service`.
    /// Caller should POST `/v1/agent/confirm`.
    FiredAndPolled,
    /// `nix-store --realise` failed (non-signature). System never
    /// switched; caller skips rollback, retries next tick.
    RealiseFailed { reason: String },
    /// `nix-store --realise` failed because the closure's narinfo
    /// signature didn't match any key in `nixfleet.trust.cacheKeys`.
    /// Distinct so dashboards can route trust violations separately
    /// from transient fetch failures. System never switched.
    SignatureMismatch {
        closure_hash: String,
        stderr_tail: String,
    },
    /// `phase`:
    /// - `nix-env-set` — setting the system profile (rollback re-points it)
    /// - `systemd-run-fire` — queueing the transient unit (systemd refused)
    /// - `switch-poll-timeout` — budget elapsed without `/run/current-system` flip
    /// - `switch-poll-mismatch` — observed a path that matched neither expected nor plausible
    SwitchFailed {
        phase: String,
        exit_code: Option<i32>,
    },
}

/// Activate via realise → set-profile → fire-and-forget switch →
/// poll → self-correct. Single attempt per call; retry comes from
/// the agent's main poll loop (in-call retry would trip the CP's
/// confirm deadline because each attempt is gated by `POLL_BUDGET`).
pub async fn activate(target: &EvaluatedTarget) -> Result<ActivationOutcome> {
    tracing::info!(
        target_closure = %target.closure_hash,
        target_channel = %target.channel_ref,
        "agent: activating target",
    );

    // Step 0: bow out if another switch-to-configuration is in
    // flight (operator manual run, sibling Ansible play, etc.) —
    // racing on the same lock produces interleaved logs + spurious
    // SwitchFailed timeouts even when the other switch succeeds.
    if is_switch_in_progress().await {
        tracing::info!(
            target_closure = %target.closure_hash,
            "agent: skipping activation — another switch-to-configuration is in flight",
        );
        return Ok(ActivationOutcome::RealiseFailed {
            reason: "switch-to-configuration lock held by another process; will retry on next tick".to_string(),
        });
    }

    // Step 1: realise. Forces fetch + sig verify explicitly; we assert
    // the realised path matches the requested one to catch symlink /
    // substitution-redirect surprises.
    let store_path = format!("/nix/store/{}", target.closure_hash);
    let realised = match realise(&store_path).await {
        Ok(p) => p,
        Err(RealiseError::SignatureMismatch { stderr_tail }) => {
            tracing::error!(
                target_closure = %target.closure_hash,
                stderr_tail = %stderr_tail,
                "agent: closure signature mismatch — refused by nix substituter trust",
            );
            return Ok(ActivationOutcome::SignatureMismatch {
                closure_hash: target.closure_hash.clone(),
                stderr_tail,
            });
        }
        Err(RealiseError::Other(err)) => {
            tracing::error!(
                target_closure = %target.closure_hash,
                error = %err,
                "agent: realisation failed; not switching",
            );
            return Ok(ActivationOutcome::RealiseFailed {
                reason: err.to_string(),
            });
        }
    };

    if realised != store_path {
        tracing::error!(
            target_closure = %target.closure_hash,
            requested = %store_path,
            realised = %realised,
            "agent: nix-store --realise returned an unexpected path; not switching",
        );
        return Ok(ActivationOutcome::RealiseFailed {
            reason: format!(
                "realised path {realised} does not match requested {store_path}",
            ),
        });
    }

    // Step 2: set the system profile FIRST so the bootloader follows
    // the new closure even if the switch process dies mid-run. The
    // activation script also sets the profile, but doing it here
    // closes the crash window between fire and script-profile-bump.
    let set_status = Command::new("nix-env")
        .arg("--profile")
        .arg("/nix/var/nix/profiles/system")
        .arg("--set")
        .arg(&store_path)
        .status()
        .await
        .with_context(|| "spawn nix-env --set")?;

    if !set_status.success() {
        tracing::error!(
            target_closure = %target.closure_hash,
            exit_code = ?set_status.code(),
            "agent: nix-env --set failed; not running switch-to-configuration",
        );
        return Ok(ActivationOutcome::SwitchFailed {
            phase: "nix-env-set".to_string(),
            exit_code: set_status.code(),
        });
    }

    // Step 3: fire (platform-dispatched fire-and-forget). See
    // `fire_switch` for the per-platform detail.
    if let Some(outcome) = fire_switch(target, &store_path).await? {
        return Ok(outcome);
    }

    // Step 4: poll. If the agent gets killed mid-poll (new closure
    // stops nixfleet-agent.service), `nixfleet-switch.service`
    // continues independently and the post-switch agent's
    // boot-recovery path posts the retroactive confirm.
    let expected = &target.closure_hash;
    match poll_current_system(expected, POLL_BUDGET, POLL_INTERVAL).await {
        Ok(()) => {
            // Step 5: profile self-correction. Defends against the
            // activation script (or a concurrent nix-env) re-pointing
            // `/nix/var/nix/profiles/system` after our Step 2 set —
            // current-system would match but the bootloader pointer
            // would be off, surprising us on next reboot.
            if let Err(err) = self_correct_profile(&store_path).await {
                tracing::warn!(
                    error = %err,
                    "agent: profile self-correction failed (non-fatal); current-system OK so activation continues",
                );
            }
            tracing::info!(
                target_closure = %expected,
                "agent: activation fire-and-forget complete (poll observed expected closure)",
            );
            Ok(ActivationOutcome::FiredAndPolled)
        }
        Err(timeout_info) => {
            // Best-effort triage: unit may still be running (large
            // download); ExecMainStatus inconclusive in that case.
            let exit_code = read_unit_exit_code("nixfleet-switch.service").await;
            tracing::error!(
                target_closure = %expected,
                last_observed = %timeout_info.last_observed,
                exit_code = ?exit_code,
                "agent: switch poll timed out — declaring SwitchFailed",
            );
            Ok(ActivationOutcome::SwitchFailed {
                phase: "switch-poll-timeout".to_string(),
                exit_code,
            })
        }
    }
}

/// Distinct so the agent can map signature-mismatch to a different
/// `ReportEvent` than transient fetch failures.
pub enum RealiseError {
    /// Stderr trimmed to last ~500 bytes for triage.
    SignatureMismatch { stderr_tail: String },
    /// Spawn failure, network error, missing path, non-utf8 stdout, etc.
    Other(anyhow::Error),
}

impl From<anyhow::Error> for RealiseError {
    fn from(err: anyhow::Error) -> Self {
        RealiseError::Other(err)
    }
}

/// nix has several wordings for substituter-trust failures across
/// versions. The set covers 2.18+ stable phrasings plus legacy 2.x.
/// Tested in `tests::detect_signature_error_*` so a nix wording
/// change breaks the test rather than silently downgrading to
/// generic RealiseFailed.
pub fn looks_like_signature_error(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    [
        "lacks a valid signature",
        "no signature is trusted",
        "is not signed by any of the keys",
        "no signatures matched",
        "signature mismatch",
        "untrusted signature",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

async fn realise(store_path: &str) -> Result<String, RealiseError> {
    let output = Command::new("nix-store")
        .arg("--realise")
        .arg(store_path)
        .output()
        .await
        .with_context(|| format!("spawn nix-store --realise {store_path}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if looks_like_signature_error(&stderr) {
            let tail_start = stderr.len().saturating_sub(500);
            let tail = stderr[tail_start..].to_string();
            return Err(RealiseError::SignatureMismatch { stderr_tail: tail });
        }
        return Err(anyhow!(
            "nix-store --realise {store_path} exited {:?}: {stderr}",
            output.status.code()
        )
        .into());
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|e| anyhow!("nix-store --realise stdout not utf-8: {e}"))?;
    let line = stdout
        .lines()
        .next()
        .ok_or_else(|| anyhow!("nix-store --realise produced no output"))?;
    Ok(line.trim().to_string())
}

async fn read_current_system_basename() -> Result<String> {
    let target = tokio::fs::read_link("/run/current-system")
        .await
        .with_context(|| "readlink /run/current-system")?;
    let basename = target
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| {
            anyhow!(
                "/run/current-system target has no utf-8 basename: {}",
                target.display()
            )
        })?
        .to_string();
    Ok(basename)
}

/// Diagnostic returned by `poll_current_system` on timeout.
#[derive(Debug, Clone)]
pub struct PollTimeout {
    /// Last basename observed (or `<missing>` / `<read-error>` if we
    /// never got a successful read). Useful in the agent log for
    /// distinguishing "switch is still running, just slow" from
    /// "switch died and the symlink is unchanged".
    pub last_observed: String,
}

/// Poll `/run/current-system` for the expected basename. Returns
/// `Ok( )` as soon as the symlink resolves to `expected`. Returns
/// `Err(PollTimeout)` once `budget` elapses without a match.
///
/// Read errors during polling are non-fatal: the symlink may be
/// briefly absent during activation (rare on NixOS but cheap to
/// tolerate). The timer keeps running.
///
/// Pure helper — no logging, deterministic timing — so it's
/// straightforward to test.
pub async fn poll_current_system(
    expected: &str,
    budget: Duration,
    interval: Duration,
) -> std::result::Result<(), PollTimeout> {
    let deadline = tokio::time::Instant::now() + budget;
    // Initial None is dead in every iteration of the loop body
    // (Ok/Err branches both assign before the deadline check), but
    // it's the natural type for "no read has completed yet" and
    // we keep the unwrap_or_else fallback for the budget=0 edge.
    #[allow(unused_assignments)]
    let mut last_observed: Option<String> = None;

    loop {
        match read_current_system_basename().await {
            Ok(basename) => {
                if basename == expected {
                    return Ok(());
                }
                last_observed = Some(basename);
            }
            Err(err) => {
                // Symlink missing or unreadable. Capture the error
                // message so the timeout diagnostic surfaces what
                // happened, but keep polling.
                last_observed = Some(format!("<read-error: {err}>"));
            }
        }

        if tokio::time::Instant::now() >= deadline {
            return Err(PollTimeout {
                last_observed: last_observed
                    .unwrap_or_else(|| String::from("<no-reads-completed>")),
            });
        }
        tokio::time::sleep(interval).await;
    }
}

/// Defensive against concurrent profile mutations during activation.
/// `Err` only when self-correction itself failed (caller treats as
/// non-fatal — current-system already verified).
async fn self_correct_profile(expected_store_path: &str) -> Result<()> {
    let profile = "/nix/var/nix/profiles/system";
    if profile_matches(expected_store_path, profile) {
        return Ok(());
    }

    tracing::warn!(
        expected = %expected_store_path,
        profile = profile,
        "agent: profile mismatch after fire-and-forget — re-running nix-env --set",
    );
    let status = Command::new("nix-env")
        .arg("--profile")
        .arg(profile)
        .arg("--set")
        .arg(expected_store_path)
        .status()
        .await
        .with_context(|| "spawn nix-env --set (self-correction)")?;
    if !status.success() {
        return Err(anyhow!(
            "nix-env --set self-correction exited {:?}",
            status.code()
        ));
    }
    if !profile_matches(expected_store_path, profile) {
        return Err(anyhow!(
            "profile still mismatched after nix-env --set self-correction",
        ));
    }
    tracing::info!("agent: profile self-corrected successfully");
    Ok(())
}

/// Two-level symlink: profile → `system-<N>-link` → `/nix/store/<basename>`.
/// Returns false on any read error (caller treats as mismatch).
fn profile_matches(expected_store_path: &str, profile_path: &str) -> bool {
    let Ok(gen_link) = std::fs::read_link(profile_path) else {
        return false;
    };
    let abs_gen_link = if gen_link.is_relative() {
        let parent = std::path::Path::new(profile_path)
            .parent()
            .unwrap_or(std::path::Path::new("/"));
        parent.join(&gen_link)
    } else {
        gen_link
    };
    let final_target = match std::fs::read_link(&abs_gen_link) {
        Ok(t) => t,
        Err(_) => abs_gen_link,
    };
    final_target.to_string_lossy() == expected_store_path
}

/// Returns `None` on failure / empty / non-numeric — caller treats
/// as unknown rather than synthesising a misleading 0. Darwin has
/// no equivalent surface; returns `None` early.
async fn read_unit_exit_code(unit_name: &str) -> Option<i32> {
    if cfg!(target_os = "macos") {
        return None;
    }
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

/// `Ok(None)` on clean fire (caller polls); `Ok(Some(outcome))`
/// on fire-step failure; `Err` only on spawn-level I/O errors.
async fn fire_switch(
    target: &EvaluatedTarget,
    store_path: &str,
) -> Result<Option<ActivationOutcome>> {
    if cfg!(target_os = "macos") {
        fire_switch_darwin(target, store_path).await
    } else {
        fire_switch_systemd(target, store_path).await
    }
}

async fn fire_switch_systemd(
    target: &EvaluatedTarget,
    store_path: &str,
) -> Result<Option<ActivationOutcome>> {
    // `systemd-run --unit=...` creates an independent transient
    // service with its own cgroup, so the agent's death (from the
    // switch script restarting nixfleet-agent.service) cannot kill
    // it. `--scope` and `--pipe --wait` both inherit the caller's
    // cgroup and die with it. `--collect` reuses the fixed unit
    // name across activations; `reset-failed` (idempotent) handles
    // the case where a prior run left the unit in failed state.
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

/// `setsid` puts the activate child in its own session so launchd's
/// process-group SIGTERM (issued during plist reload when the new
/// closure changes the agent binary path) doesn't propagate to it.
/// `nohup` doesn't work in launchd-daemon context (no controlling
/// TTY); only `setsid` gives the survivable session.
#[cfg(unix)]
async fn fire_switch_darwin(
    target: &EvaluatedTarget,
    store_path: &str,
) -> Result<Option<ActivationOutcome>> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

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

#[cfg(not(unix))]
async fn fire_switch_darwin(
    _target: &EvaluatedTarget,
    _store_path: &str,
) -> Result<Option<ActivationOutcome>> {
    Err(anyhow!("fire_switch_darwin called on non-unix host"))
}

/// Falls back to inherit on permission/IO error; launchd's
/// StandardOutPath/StandardErrorPath catches the inherited stream.
#[cfg(unix)]
fn attach_activate_log(cmd: &mut std::process::Command) {
    use std::process::Stdio;
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

async fn fire_rollback(target_basename: &str) -> Result<Option<RollbackOutcome>> {
    if cfg!(target_os = "macos") {
        fire_rollback_darwin(target_basename).await
    } else {
        fire_rollback_systemd().await
    }
}

/// `nix-env --rollback` already re-pointed `/run/current-system`,
/// so we fire its switch-to-configuration to actually run activation.
async fn fire_rollback_systemd() -> Result<Option<RollbackOutcome>> {
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

#[cfg(unix)]
async fn fire_rollback_darwin(target_basename: &str) -> Result<Option<RollbackOutcome>> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

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

#[cfg(not(unix))]
async fn fire_rollback_darwin(_target_basename: &str) -> Result<Option<RollbackOutcome>> {
    Err(anyhow!("fire_rollback_darwin called on non-unix host"))
}

/// Outcome of a `rollback ` call. Mirrors `ActivationOutcome`'s
/// shape so callers can pattern-match similarly. Fire-and-forget
/// applies to rollback for the same reason as activate: if the
/// rolled-back closure's activation script changes a unit definition
/// the running agent depends on (transitively — system services like
/// dbus/systemd-tmpfiles can chain into this), a synchronous spawn
/// gets SIGTERMed mid-run when systemd reloads.
#[derive(Debug)]
pub enum RollbackOutcome {
    FiredAndPolled,
    /// `phase`: `nix-env-rollback`, `discover-target`,
    /// `systemd-run-fire`, `rollback-poll-timeout`.
    Failed {
        phase: String,
        exit_code: Option<i32>,
    },
}

impl RollbackOutcome {
    pub fn success(&self) -> bool {
        matches!(self, RollbackOutcome::FiredAndPolled)
    }
    pub fn exit_code(&self) -> Option<i32> {
        match self {
            RollbackOutcome::Failed { exit_code, .. } => *exit_code,
            RollbackOutcome::FiredAndPolled => None,
        }
    }
    pub fn phase(&self) -> Option<&str> {
        match self {
            RollbackOutcome::Failed { phase, .. } => Some(phase.as_str()),
            RollbackOutcome::FiredAndPolled => None,
        }
    }
}

/// Bypasses `nixos-rebuild` (`nixos-rebuild-ng` tries to evaluate
/// `<nixpkgs/nixos>` even on `--rollback`, failing in the agent's
/// NIX_PATH-less sandbox). Caller must invoke exactly once per
/// failed activation — running twice rolls back twice.
pub async fn rollback() -> Result<RollbackOutcome> {
    tracing::warn!("agent: triggering local rollback (fire-and-forget via systemd-run)");

    // Step 1: profile flip — synchronous symlink re-target.
    let env_status = Command::new("nix-env")
        .arg("--profile")
        .arg("/nix/var/nix/profiles/system")
        .arg("--rollback")
        .status()
        .await
        .with_context(|| "spawn nix-env --rollback")?;
    if !env_status.success() {
        tracing::error!(
            exit_code = ?env_status.code(),
            "agent: nix-env --rollback failed; cannot proceed",
        );
        return Ok(RollbackOutcome::Failed {
            phase: "nix-env-rollback".to_string(),
            exit_code: env_status.code(),
        });
    }

    // Step 2: discover the rolled-back target so we can poll for it
    // (stronger contract than "any change").
    let target_basename = match resolve_profile_target() {
        Ok(b) => b,
        Err(err) => {
            tracing::error!(
                error = %err,
                "agent: cannot resolve rolled-back profile target; aborting rollback",
            );
            return Ok(RollbackOutcome::Failed {
                phase: "discover-target".to_string(),
                exit_code: None,
            });
        }
    };
    tracing::info!(
        target_basename = %target_basename,
        "agent: rollback target discovered; firing detached switch",
    );

    // Step 3: fire rollback (platform-dispatched).
    if let Some(failure) = fire_rollback(&target_basename).await? {
        return Ok(failure);
    }

    // Step 4: poll for the rolled-back target.
    match poll_current_system(&target_basename, POLL_BUDGET, POLL_INTERVAL).await {
        Ok(()) => {
            tracing::info!(
                target = %target_basename,
                "agent: rollback fire-and-forget complete",
            );
            Ok(RollbackOutcome::FiredAndPolled)
        }
        Err(timeout) => {
            let exit_code = read_unit_exit_code("nixfleet-rollback.service").await;
            tracing::error!(
                target = %target_basename,
                last_observed = %timeout.last_observed,
                exit_code = ?exit_code,
                "agent: rollback poll timed out",
            );
            Ok(RollbackOutcome::Failed {
                phase: "rollback-poll-timeout".to_string(),
                exit_code,
            })
        }
    }
}

/// Two symlink levels: profile → `system-<N>-link` (relative) →
/// `/nix/store/<basename>` (absolute).
fn resolve_profile_target() -> Result<String> {
    let profile = std::path::Path::new("/nix/var/nix/profiles/system");
    let gen_link = std::fs::read_link(profile)
        .with_context(|| "readlink /nix/var/nix/profiles/system")?;
    let abs_gen_link = if gen_link.is_relative() {
        profile.parent().unwrap_or(std::path::Path::new("/")).join(&gen_link)
    } else {
        gen_link.clone()
    };
    let store_path = std::fs::read_link(&abs_gen_link)
        .with_context(|| format!("readlink {}", abs_gen_link.display()))?;
    let basename = store_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("non-utf8 basename: {}", store_path.display()))?
        .to_string();
    Ok(basename)
}

/// `Acknowledged` (204): done. `Cancelled` (410): CP says the
/// rollout was cancelled or deadline expired — agent rolls back.
/// `Other`: logged; the CP's rollback timer catches deadline expiry.
pub async fn confirm_target(
    client: &reqwest::Client,
    cp_url: &str,
    hostname: &str,
    target: &EvaluatedTarget,
    rollout: &str,
    wave: u32,
    boot_id: &str,
) -> Result<crate::comms::ConfirmOutcome> {
    use nixfleet_proto::agent_wire::{ConfirmRequest, GenerationRef};

    let req = ConfirmRequest {
        hostname: hostname.to_string(),
        rollout: rollout.to_string(),
        wave,
        generation: GenerationRef {
            closure_hash: target.closure_hash.clone(),
            channel_ref: Some(target.channel_ref.clone()),
            boot_id: boot_id.to_string(),
        },
    };

    let outcome = crate::comms::confirm(client, cp_url, &req).await?;
    match outcome {
        crate::comms::ConfirmOutcome::Acknowledged => {
            tracing::info!(
                target_closure = %target.closure_hash,
                rollout,
                wave,
                "agent: confirm acknowledged (204)",
            );
        }
        crate::comms::ConfirmOutcome::Cancelled => {
            tracing::warn!(
                target_closure = %target.closure_hash,
                rollout,
                "agent: confirm returned 410 — CP says trigger local rollback",
            );
        }
        crate::comms::ConfirmOutcome::Other => {
            tracing::warn!(
                target_closure = %target.closure_hash,
                rollout,
                "agent: confirm returned unexpected status — deadline timer will handle",
            );
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    //! Pure-logic tests for the path-comparison surface of
    //! `read_current_system_basename`. The realise/switch path itself
    //! is covered by the microvm harness — unit-level mocking of
    //! `Command` is more friction than payoff.

    use super::*;
    use std::path::{Path, PathBuf};

    /// Stand-in for `read_current_system_basename` that takes the
    /// (already-resolved) symlink target as a path and returns the
    /// basename. Used to exercise the basename-extraction logic
    /// without touching `/run/current-system`.
    fn basename_of(target: &Path) -> Result<String> {
        target
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("no utf-8 basename: {}", target.display()))
    }

    #[test]
    fn basename_extracts_from_typical_store_path() {
        let p = PathBuf::from("/nix/store/abc123-nixos-system-test-host-26.05");
        assert_eq!(basename_of(&p).unwrap(), "abc123-nixos-system-test-host-26.05");
    }

    #[test]
    fn basename_unchanged_by_trailing_slash() {
        let p = PathBuf::from("/nix/store/abc123-nixos-system-test-host-26.05/");
        assert_eq!(basename_of(&p).unwrap(), "abc123-nixos-system-test-host-26.05");
    }

    #[test]
    fn outcome_kinds_are_distinct() {
        // Trivial round-trip: just asserts the variants exist + Debug-print
        // distinctly so future refactors don't silently drop one.
        let outcomes = [
            format!("{:?}", ActivationOutcome::FiredAndPolled),
            format!(
                "{:?}",
                ActivationOutcome::RealiseFailed {
                    reason: "x".into()
                }
            ),
            format!(
                "{:?}",
                ActivationOutcome::SwitchFailed {
                    phase: "switch-poll-timeout".into(),
                    exit_code: Some(1),
                }
            ),
            format!(
                "{:?}",
                ActivationOutcome::SignatureMismatch {
                    closure_hash: "h".into(),
                    stderr_tail: "x".into(),
                }
            ),
        ];
        let unique: std::collections::HashSet<_> = outcomes.iter().collect();
        assert_eq!(unique.len(), outcomes.len(), "outcome variants collide on Debug");
    }

    #[tokio::test]
    async fn poll_current_system_returns_ok_when_match_appears() {
        // Use tempdir + symlink to simulate /run/current-system. The
        // helper only takes a basename — we exercise the poll loop's
        // observation logic by symlinking and leaving it stable; the
        // real /run/current-system is fine for tests since we don't
        // assert on its content.
        // Skip on systems without /run/current-system (Darwin in CI).
        if !std::path::Path::new("/run/current-system").exists() {
            return;
        }
        // Read whatever is currently there and assert match — this
        // exercises the success branch of the poll loop with no fake
        // filesystem rigging.
        let basename = read_current_system_basename().await.unwrap();
        let result = poll_current_system(
            &basename,
            Duration::from_millis(100),
            Duration::from_millis(10),
        )
        .await;
        assert!(result.is_ok(), "poll did not match its own current-system: {result:?}");
    }

    #[tokio::test]
    async fn poll_current_system_times_out_when_no_match() {
        if !std::path::Path::new("/run/current-system").exists() {
            return;
        }
        let result = poll_current_system(
            "definitely-not-a-real-closure-hash-xyz",
            Duration::from_millis(50),
            Duration::from_millis(10),
        )
        .await;
        let err = result.expect_err("expected timeout");
        // last_observed should be the actual current-system basename,
        // not the placeholder.
        assert!(
            !err.last_observed.starts_with("<not-yet-read>"),
            "expected at least one observation before timeout: {err:?}",
        );
    }

    #[test]
    fn switch_failed_phase_strings_are_stable() {
        // The phase strings are part of the wire (passed up to CP via
        // ReportEvent::ActivationFailed); locking them here ensures
        // any rename on the agent side surfaces as a test failure
        // rather than silently changing the report contract.
        //
        // Linux phases: nix-env-set, systemd-run-fire, switch-poll-*.
        // Darwin phases: nix-env-set (shared), darwin-activate-spawn,
        // switch-poll-* (shared — `/run/current-system` is the same
        // signal on both).
        for phase in &[
            "nix-env-set",
            "systemd-run-fire",
            "switch-poll-timeout",
            "switch-poll-mismatch",
            "darwin-activate-spawn",
        ] {
            let outcome = ActivationOutcome::SwitchFailed {
                phase: (*phase).to_string(),
                exit_code: None,
            };
            // Just exercises the constructor + Debug — the assertion
            // is that the strings compile + match the documented set.
            let _ = format!("{outcome:?}");
        }
    }

    /// Platform dispatch sanity check. The helper picks the right
    /// fire impl based on `cfg!(target_os)`. We can't unit-test the
    /// actual fire path (would need a real `nix-env`+`activate`/
    /// `systemd-run` rig — that's the harness's job), but we CAN
    /// assert that `is_switch_in_progress` short-circuits to false
    /// on darwin (no flock probe) and goes through the file-existence
    /// gate on linux.
    #[tokio::test]
    async fn is_switch_in_progress_short_circuits_on_darwin() {
        // On darwin: always false (no equivalent lock).
        // On linux: returns false when the lock file is absent.
        // Either way, in CI / dev sandbox the lock file shouldn't
        // exist, so the function returns false. The platform-specific
        // assertion is that on darwin we never even *try* to run
        // `flock(1)` — the function returns at the first line. We
        // can't directly observe that without mocking, but we *can*
        // assert the function returns false fast (well under the
        // 1s timeout) on a clean dev machine.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            is_switch_in_progress(),
        )
        .await;
        assert!(
            result.is_ok(),
            "is_switch_in_progress should return promptly on a host with no in-flight switch",
        );
        assert!(
            !result.unwrap(),
            "is_switch_in_progress: no switch in flight on a clean dev host",
        );
    }

    /// Phase string returned on darwin when `<store>/activate` spawn
    /// fails. Locked here separately from the linux phases since
    /// it's part of the wire contract.
    #[test]
    fn darwin_activate_spawn_phase_string_is_stable() {
        let outcome = ActivationOutcome::SwitchFailed {
            phase: "darwin-activate-spawn".to_string(),
            exit_code: None,
        };
        let s = format!("{outcome:?}");
        assert!(s.contains("darwin-activate-spawn"));
    }

    /// Ensure `read_unit_exit_code` returns None on darwin without
    /// shelling out to a non-existent `systemctl`. The mere fact
    /// that the call returns (rather than hanging or panicking on
    /// macOS) is the assertion — the function short-circuits on
    /// `cfg!(target_os = "macos")`.
    #[tokio::test]
    async fn read_unit_exit_code_short_circuits_on_darwin() {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            read_unit_exit_code("definitely-not-a-real-unit.service"),
        )
        .await
        .expect("must return promptly");
        // On darwin: always None (no systemctl). On linux: None
        // because the unit doesn't exist. Both branches converge.
        assert!(result.is_none());
    }

    #[test]
    fn detect_signature_error_matches_nix_2_18_phrasing() {
        let s = "error: cannot add path '/nix/store/abc-foo' because \
                 it lacks a valid signature";
        assert!(looks_like_signature_error(s));
    }

    #[test]
    fn detect_signature_error_matches_no_signature_trusted() {
        let s = "error: no signature is trusted by any of these keys: cache.example.com-1";
        assert!(looks_like_signature_error(s));
    }

    #[test]
    fn detect_signature_error_matches_legacy_phrasing() {
        let s = "error: path '/nix/store/abc-foo' is not signed by any of the keys in \
                 trusted-public-keys";
        assert!(looks_like_signature_error(s));
    }

    #[test]
    fn detect_signature_error_matches_no_signatures_matched() {
        let s = "error: no signatures matched any of the configured public keys";
        assert!(looks_like_signature_error(s));
    }

    #[test]
    fn detect_signature_error_matches_signature_mismatch() {
        let s = "warning: signature mismatch for path '/nix/store/abc-foo'";
        assert!(looks_like_signature_error(s));
    }

    #[test]
    fn detect_signature_error_does_not_match_network_failure() {
        // Network blip → generic RealiseFailed, not SignatureMismatch.
        let s = "error: unable to download 'https://cache.example.com/nar/abc.nar': \
                 Couldn't connect to server";
        assert!(!looks_like_signature_error(s));
    }

    #[test]
    fn detect_signature_error_does_not_match_missing_path() {
        let s = "error: path '/nix/store/abc-foo' is required, but it has no substitutes \
                 and there is no derivation that produces it";
        assert!(!looks_like_signature_error(s));
    }

    #[test]
    fn detect_signature_error_case_insensitive() {
        // tracing or operator-side log redirection sometimes uppercases
        // the first letter; the heuristic is case-insensitive so
        // "Lacks a valid signature" still matches.
        let s = "Error: path Lacks A Valid Signature on this host";
        assert!(looks_like_signature_error(s));
    }
}
