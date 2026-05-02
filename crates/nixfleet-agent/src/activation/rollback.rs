//! Rollback pipeline: nix-env --rollback → discover target → fire
//! (backend) → poll. Bypasses `nixos-rebuild` because
//! `nixos-rebuild-ng --rollback` tries to evaluate
//! `<nixpkgs/nixos>` even on rollback, which fails in the agent's
//! NIX_PATH-less sandbox.

use anyhow::{Context, Result};
use tokio::process::Command;

use super::backend::ActivationBackend;
use super::outcome::RollbackOutcome;
use super::profile::resolve_profile_target;
use super::verify_poll::{PollOutcome, VerifyPoll};

/// Generic-over-backend form. Production calls `rollback()` (the
/// façade in `mod.rs`); tests inject a fake.
pub async fn rollback_with<B: ActivationBackend>(backend: &B) -> Result<RollbackOutcome> {
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

    // Step 3: fire rollback (backend-dispatched).
    if let Some(failure) = backend.fire_rollback(&target_basename).await? {
        return Ok(failure);
    }

    // Step 4: poll for the rolled-back target. `previous_basename`
    // stays None — rollback has no meaningful "expected pre-state"
    // reference (the pre-rollback basename is the failed generation
    // we're abandoning), so flip-to-unexpected detection is disabled
    // and any non-match collapses into the timeout branch.
    match VerifyPoll::new(&target_basename).until_settled().await {
        PollOutcome::Settled => {
            tracing::info!(
                target = %target_basename,
                "agent: rollback fire-and-forget complete",
            );
            Ok(RollbackOutcome::FiredAndPolled)
        }
        PollOutcome::Timeout { last_observed } => {
            let exit_code = backend.read_unit_exit_code("nixfleet-rollback.service").await;
            tracing::error!(
                target = %target_basename,
                last_observed = %last_observed,
                exit_code = ?exit_code,
                "agent: rollback poll timed out",
            );
            Ok(RollbackOutcome::Failed {
                phase: "rollback-poll-timeout".to_string(),
                exit_code,
            })
        }
        PollOutcome::FlippedToUnexpected { .. } => {
            // Unreachable: only emitted when previous_basename is Some.
            unreachable!(
                "FlippedToUnexpected requires Some(previous_basename); rollback leaves it None"
            )
        }
    }
}
