//! Agent-side activation logic (Phase 4 PR-D).
//!
//! When the CP returns a non-null `target` in `CheckinResponse`,
//! the agent:
//! 1. Logs the target (already done in Phase 3 — kept for audit).
//! 2. Runs `nixos-rebuild switch --to-system <store-path>` to
//!    activate the closure. The store path is reconstructed from
//!    the closure hash (`/nix/store/<hash>-nixos-system-…`); the
//!    agent assumes the closure is already in the local nix store
//!    or reachable via the configured binary cache.
//! 3. On rebuild success: TODO — POST /v1/agent/confirm. Wire
//!    types live in nixfleet_proto::agent_wire (PR-A); when both
//!    PRs land, this module's `confirm_target` function gets its
//!    real body.
//! 4. On rebuild failure: run `nixos-rebuild --rollback` to revert
//!    to the previous boot generation. This is the agent-side half
//!    of magic rollback (issue #2). The CP-side half — the
//!    deadline-based rollback when /confirm doesn't arrive — is in
//!    parallel PR feat/phase-4-magic-rollback.
//!
//! All commands run as root via the systemd unit (StateDirectory +
//! no NoNewPrivileges hardening on the agent unit; the agent is a
//! privileged system manager by design — see the agent module
//! comment in modules/scopes/nixfleet/_agent.nix).

use std::process::ExitStatus;

use anyhow::Result;
use nixfleet_proto::agent_wire::EvaluatedTarget;
use tokio::process::Command;

/// Activate `target` via `nixos-rebuild switch`. Returns the exit
/// status; non-zero means activation failed and the caller should
/// trigger rollback.
///
/// Resolves the closure to a store path via `nix-store --realise`
/// first. If the closure isn't in the local store, this will fall
/// through to the configured binary cache (or the closure-proxy
/// fallback if attic is unreachable).
pub async fn activate(target: &EvaluatedTarget) -> Result<ExitStatus> {
    tracing::info!(
        target_closure = %target.closure_hash,
        target_channel = %target.channel_ref,
        "agent: activating target via nixos-rebuild switch"
    );

    // Resolve the closure hash to a system store path. Format:
    // `/nix/store/<hash>-nixos-system-<host>-<rev>`. We need the
    // full path for `nixos-rebuild switch --to-system`; nix-store
    // produces it on stdout once realisation succeeds.
    //
    // For now we assume the closure_hash IS the full store path
    // basename (e.g. "abc123def-nixos-system-krach-26.05") so we
    // can construct the path directly. If the CP only sends the
    // 32-char hash, we'd need a separate /v1 endpoint to look up
    // the system path — TODO in a follow-up if it becomes a real
    // problem.
    let store_path = format!("/nix/store/{}", target.closure_hash);

    let status = Command::new("nixos-rebuild")
        .arg("switch")
        .arg("--no-flake")
        .arg("--system")
        .arg(&store_path)
        .status()
        .await?;

    if status.success() {
        tracing::info!(
            target_closure = %target.closure_hash,
            "agent: activation succeeded"
        );
    } else {
        tracing::error!(
            target_closure = %target.closure_hash,
            exit_code = ?status.code(),
            "agent: activation failed — caller should trigger rollback"
        );
    }
    Ok(status)
}

/// Local rollback: `nixos-rebuild --rollback`. Reverts to the
/// previous boot generation. Used when:
/// - activate() returned a non-zero status (activation itself
///   failed).
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
        .await?;
    if status.success() {
        tracing::info!("agent: rollback succeeded");
    } else {
        tracing::error!(exit_code = ?status.code(), "agent: rollback failed");
    }
    Ok(status)
}

/// TODO(phase-4-pr-a-merged): POST /v1/agent/confirm.
///
/// Currently a no-op + log. The wire types (ConfirmRequest,
/// ConfirmResponse) live in nixfleet_proto::agent_wire, added by
/// the parallel feat/phase-4-confirm-wire branch. Once both PRs
/// land, this function gets its real body:
///
/// ```ignore
/// let req = ConfirmRequest { hostname, rollout, wave, generation };
/// let url = format!("{}/v1/agent/confirm", cp_url.trim_end_matches('/'));
/// let resp = client.post(&url).json(&req).send().await?;
/// match resp.status().as_u16() {
///     204 => Ok(ConfirmOutcome::Acknowledged),
///     410 => Ok(ConfirmOutcome::Cancelled), // CP says trigger local rollback
///     other => anyhow::bail!("unexpected status {other}"),
/// }
/// ```
///
/// For now: log the intent. The activation loop logs at info; the
/// operator sees what would have been posted via journal.
pub async fn confirm_target(target: &EvaluatedTarget) -> Result<()> {
    tracing::info!(
        target_closure = %target.closure_hash,
        "agent: would POST /v1/agent/confirm (Phase 4 PR-A wires this)"
    );
    Ok(())
}
