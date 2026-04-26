//! Agent-side activation logic.
//!
//! The CP issues a closure hash via `CheckinResponse.target`; the
//! agent's job is to install + boot that closure. Per ARCHITECTURE.md
//! the agent is the *last line of defense* against a misbehaving
//! substituter or a tampered CP, so activation runs three checks
//! around `nixos-rebuild switch`:
//!
//! 1. **Pre-realise**: `nix-store --realise <path>` forces nix to
//!    fetch from the configured substituter (attic) and validate its
//!    signature *before* we commit to switching. If the closure isn't
//!    locally available and substituter trust is misconfigured, this
//!    fails closed — we never call `nixos-rebuild` against an
//!    unverifiable path. Also catches "closure-proxy returned a
//!    valid-looking narinfo for a path that doesn't actually exist
//!    upstream" (the proxy-fallback path is fundamentally less
//!    audited than direct attic).
//! 2. **Switch**: `nixos-rebuild switch --system <verified-path>`.
//!    nix's own substituter signature checks fire here too; the
//!    pre-realise is belt-and-suspenders.
//! 3. **Post-verify**: read `/run/current-system` (resolve symlink),
//!    compare basename against the expected closure_hash. If they
//!    differ — switched to the wrong path, or `--system` got rewritten
//!    somewhere — refuse to confirm and trigger local rollback.
//!
//! Pre-realise + post-verify together close the property "the agent
//! either confirms the *exact* closure the CP told it about, or rolls
//! back" — without trusting the substituter or the CP to be honest
//! about which path was activated.
//!
//! On rebuild failure or post-verify mismatch the caller runs
//! `nixos-rebuild --rollback` to revert to the previous boot
//! generation. CP-side magic rollback (deadline expiry → 410 on
//! `/confirm`) is independent and additive.
//!
//! All commands run as root via the systemd unit (StateDirectory +
//! no NoNewPrivileges hardening on the agent unit; the agent is a
//! privileged system manager by design — see the agent module
//! comment in modules/scopes/nixfleet/_agent.nix).

use std::process::ExitStatus;

use anyhow::{anyhow, Context, Result};
use nixfleet_proto::agent_wire::EvaluatedTarget;
use tokio::process::Command;

/// Outcome of an activation attempt. The agent's main loop maps each
/// variant to a follow-up action: confirm on `Success`, rollback on
/// either `SwitchFailed` or `VerifyMismatch`, retry-on-next-tick on
/// `RealiseFailed` (nothing was switched, nothing to roll back).
#[derive(Debug)]
pub enum ActivationOutcome {
    /// Realised, switched, and `/run/current-system` matches the
    /// expected closure. Caller should POST `/v1/agent/confirm`.
    Success,
    /// `nix-store --realise` exited non-zero or returned a path that
    /// doesn't match the input. The system was never switched —
    /// caller skips rollback, retries next tick.
    RealiseFailed { reason: String },
    /// `nixos-rebuild switch` exited non-zero. Caller runs local
    /// rollback.
    SwitchFailed { exit_status: ExitStatus },
    /// Switch succeeded but `/run/current-system`'s basename does not
    /// match the expected closure_hash. The system is now booting an
    /// unexpected closure — caller runs local rollback.
    VerifyMismatch {
        expected: String,
        actual: String,
    },
}

/// Activate `target` via realise → switch → verify.
///
/// `tracing` events at every step give operators a grep-friendly
/// breadcrumb trail without parsing the systemd journal in JSON. The
/// `target_closure` field is consistent across all three log lines so
/// `journalctl | grep target_closure=<hash>` follows one activation
/// end to end.
pub async fn activate(target: &EvaluatedTarget) -> Result<ActivationOutcome> {
    tracing::info!(
        target_closure = %target.closure_hash,
        target_channel = %target.channel_ref,
        "agent: activating target",
    );

    // Step 1: realise.
    //
    // Pre-`nixos-rebuild` so that closure fetch + signature verification
    // happens explicitly here. nix-store prints the realised path to
    // stdout when it succeeds — we capture and assert it matches the
    // path we asked for, in case some future nix changes resolve
    // through symlinks or substitution-redirects.
    let store_path = format!("/nix/store/{}", target.closure_hash);
    let realised = match realise(&store_path).await {
        Ok(p) => p,
        Err(err) => {
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

    // Step 2: switch.
    let switch_status = Command::new("nixos-rebuild")
        .arg("switch")
        .arg("--no-flake")
        .arg("--system")
        .arg(&store_path)
        .status()
        .await
        .with_context(|| "spawn nixos-rebuild switch")?;

    if !switch_status.success() {
        tracing::error!(
            target_closure = %target.closure_hash,
            exit_code = ?switch_status.code(),
            "agent: nixos-rebuild switch failed",
        );
        return Ok(ActivationOutcome::SwitchFailed {
            exit_status: switch_status,
        });
    }

    // Step 3: post-switch verify.
    //
    // Read /run/current-system and compare. If --system was rewritten
    // by some shim, or if the system got switched to a different path
    // than we asked for, this is the gate that catches it.
    let actual_basename = match read_current_system_basename().await {
        Ok(b) => b,
        Err(err) => {
            // Couldn't read /run/current-system at all — this is weird
            // (the switch just succeeded). Treat as mismatch so the
            // caller rolls back rather than confirming blind.
            tracing::error!(
                target_closure = %target.closure_hash,
                error = %err,
                "agent: cannot read /run/current-system after switch — treating as mismatch",
            );
            return Ok(ActivationOutcome::VerifyMismatch {
                expected: target.closure_hash.clone(),
                actual: format!("<read failed: {err}>"),
            });
        }
    };

    if actual_basename != target.closure_hash {
        tracing::error!(
            target_closure = %target.closure_hash,
            actual = %actual_basename,
            "agent: post-switch verify mismatch — /run/current-system does not match expected closure",
        );
        return Ok(ActivationOutcome::VerifyMismatch {
            expected: target.closure_hash.clone(),
            actual: actual_basename,
        });
    }

    tracing::info!(
        target_closure = %target.closure_hash,
        "agent: activation succeeded (realised + switched + verified)",
    );
    Ok(ActivationOutcome::Success)
}

/// `nix-store --realise <path>` — fetch + verify, return the realised
/// path from stdout. nix-store prints one path per line; we expect
/// exactly one (we passed exactly one input).
async fn realise(store_path: &str) -> Result<String> {
    let output = Command::new("nix-store")
        .arg("--realise")
        .arg(store_path)
        .output()
        .await
        .with_context(|| format!("spawn nix-store --realise {store_path}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "nix-store --realise {store_path} exited {:?}: {stderr}",
            output.status.code()
        ));
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|e| anyhow!("nix-store --realise stdout not utf-8: {e}"))?;
    let line = stdout
        .lines()
        .next()
        .ok_or_else(|| anyhow!("nix-store --realise produced no output"))?;
    Ok(line.trim().to_string())
}

/// Read `/run/current-system` as a symlink and return the basename of
/// its target. The basename is the closure-hash form the wire and the
/// CP both speak.
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

/// Local rollback: `nixos-rebuild --rollback`. Reverts to the
/// previous boot generation. Used when:
/// - `activate()` returned a non-success outcome that requires
///   rollback (`SwitchFailed`, `VerifyMismatch`).
/// - The agent's confirm window expired before the CP acknowledged
///   the activation (magic rollback, RFC-0003 §4.2).
///
/// Idempotent — running rollback twice in a row reverts twice. The
/// caller is expected to invoke this exactly once per failed
/// activation.
pub async fn rollback() -> Result<ExitStatus> {
    tracing::warn!("agent: triggering local rollback (nixos-rebuild --rollback)");
    let status = Command::new("nixos-rebuild")
        .arg("--rollback")
        .arg("switch")
        .status()
        .await
        .with_context(|| "spawn nixos-rebuild --rollback switch")?;
    if status.success() {
        tracing::info!("agent: rollback succeeded");
    } else {
        tracing::error!(exit_code = ?status.code(), "agent: rollback failed");
    }
    Ok(status)
}

/// POST `/v1/agent/confirm` to acknowledge a successful activation.
///
/// Per RFC-0003 §4.2 the agent confirms exactly once after a
/// successful activation. Returns `ConfirmOutcome` so the activation
/// loop can react:
/// - `Acknowledged` (204): nothing else to do.
/// - `Cancelled` (410): CP says the rollout was cancelled or the
///   deadline passed — agent runs `nixos-rebuild --rollback`.
/// - `Other`: logged; the CP-side rollback timer will catch deadline
///   expiry independently.
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
    //! is covered by the microvm harness (Phase 5) — unit-level
    //! mocking of `Command` is more friction than payoff.

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
        let p = PathBuf::from("/nix/store/abc123-nixos-system-krach-26.05");
        assert_eq!(basename_of(&p).unwrap(), "abc123-nixos-system-krach-26.05");
    }

    #[test]
    fn basename_unchanged_by_trailing_slash() {
        let p = PathBuf::from("/nix/store/abc123-nixos-system-krach-26.05/");
        assert_eq!(basename_of(&p).unwrap(), "abc123-nixos-system-krach-26.05");
    }

    #[test]
    fn outcome_kinds_are_distinct() {
        // Trivial round-trip: just asserts the variants exist + Debug-print
        // distinctly so future refactors don't silently drop one.
        let outcomes = [
            format!("{:?}", ActivationOutcome::Success),
            format!(
                "{:?}",
                ActivationOutcome::RealiseFailed {
                    reason: "x".into()
                }
            ),
            format!(
                "{:?}",
                ActivationOutcome::VerifyMismatch {
                    expected: "a".into(),
                    actual: "b".into()
                }
            ),
        ];
        let unique: std::collections::HashSet<_> = outcomes.iter().collect();
        assert_eq!(unique.len(), outcomes.len(), "outcome variants collide on Debug");
    }
}
