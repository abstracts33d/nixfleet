//! Agent-side activation logic.
//!
//! The CP issues a closure hash via `CheckinResponse.target`; the
//! agent's job is to install + boot that closure. Per ARCHITECTURE.md
//! the agent is the *last line of defense* against a misbehaving
//! substituter or a tampered CP, so activation runs three checks
//! around `nixos-rebuild switch`:
//!
//! 1. **Pre-realise**: `nix-store --realise <path>` forces nix to
//!    fetch from the configured substituter (any nix-cache-protocol
//!    backend the fleet wires — harmonia, attic, cachix, etc.) and
//!    validate its signature *before* we commit to switching. If the
//!    closure isn't locally available and substituter trust is
//!    misconfigured, this fails closed — we never call
//!    `nixos-rebuild` against an unverifiable path. Also catches
//!    "closure-proxy returned a valid-looking narinfo for a path
//!    that doesn't actually exist upstream" (the proxy-fallback
//!    path is fundamentally less audited than direct substituter
//!    fetch).
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
    /// `nix-store --realise` failed specifically because the closure's
    /// narinfo signature did not match any key in
    /// `nixfleet.trust.cacheKeys` (issue #12 root #2). Distinct from
    /// the generic RealiseFailed so the operator dashboard can route
    /// "trust violation" alerts separately from "transient fetch
    /// failure". The system was never switched. Cache-substituter-
    /// agnostic — fires for harmonia, attic, cachix, or any other
    /// nix-cache-protocol backend; nix's substituter trust check is
    /// the gate, this just classifies its failure.
    SignatureMismatch {
        closure_hash: String,
        stderr_tail: String,
    },
    /// One of the activation steps exited non-zero. Caller runs
    /// local rollback. `phase` distinguishes which step failed:
    /// - `"nix-env-set"`: setting the system profile (system was
    ///   never activated; rollback re-points the profile).
    /// - `"switch-to-configuration"`: activation script (the system
    ///   may be in a half-applied state until rollback re-runs the
    ///   prior configuration's switch script).
    SwitchFailed {
        phase: String,
        exit_status: ExitStatus,
    },
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

    // Step 2: switch.
    //
    // Direct `nix-env --profile … --set` + `<path>/bin/switch-to-
    // configuration switch`. Earlier shape was `nixos-rebuild switch
    // --system <path>`, but `nixos-rebuild-ng` (the Python rewrite
    // shipped with NixOS 26.05) renamed `--system` to `--store-path`
    // and adds a self-evaluation step that needs `<nixpkgs/nixos>`
    // on NIX_PATH (the agent doesn't have it). Calling
    // switch-to-configuration directly skips the wrapper entirely:
    // no flag-rename surface, no eval of nixpkgs, no implicit
    // bootloader fallback path. This is what nixos-rebuild itself
    // calls under the hood — the agent's job is just to point the
    // system profile at the new closure and run its activation
    // script. Caught on lab when the agent's first real activation
    // attempt errored with `unrecognized arguments: --system …`.
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
            exit_status: set_status,
        });
    }

    let switch_status = Command::new(format!("{store_path}/bin/switch-to-configuration"))
        .arg("switch")
        .status()
        .await
        .with_context(|| format!("spawn {store_path}/bin/switch-to-configuration switch"))?;

    if !switch_status.success() {
        tracing::error!(
            target_closure = %target.closure_hash,
            exit_code = ?switch_status.code(),
            "agent: switch-to-configuration failed",
        );
        return Ok(ActivationOutcome::SwitchFailed {
            phase: "switch-to-configuration".to_string(),
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

/// Structured failure mode for `realise()`. Distinguishing
/// signature-mismatch from generic failure lets the agent map each
/// to a different `ReportEvent` variant (issue #12 root #2: explicit
/// surfacing of cache-trust violations vs transient fetch failures).
pub enum RealiseError {
    /// nix's substituter trust check refused the narinfo because no
    /// signature in it matched any key in `trusted-public-keys`
    /// (sourced from `nixfleet.trust.cacheKeys`). Stderr tail kept
    /// for triage; trimmed to the last few hundred bytes so the
    /// `ReportEvent` payload doesn't bloat.
    SignatureMismatch { stderr_tail: String },
    /// Anything else: spawn failure, network error, missing path,
    /// non-utf8 stdout, etc. Generic — caller maps to RealiseFailed.
    Other(anyhow::Error),
}

impl From<anyhow::Error> for RealiseError {
    fn from(err: anyhow::Error) -> Self {
        RealiseError::Other(err)
    }
}

/// Heuristic: is this stderr from `nix-store --realise` the
/// signature-trust failure mode? nix has shipped several phrasings
/// over the years; this matches the stable-as-of-2.18+ wording plus
/// a few legacy patterns. Cache-substituter-agnostic — the error
/// originates in nix's own narinfo verifier, not the cache backend.
///
/// Tested in `tests::detect_signature_error_*` — when nix changes
/// the wording, the test breaks rather than the production behavior
/// silently degrading to generic RealiseFailed.
pub fn looks_like_signature_error(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    [
        // nix 2.18+: "cannot add path '…' because it lacks a valid signature"
        "lacks a valid signature",
        // nix 2.18+: "no signature is trusted by"
        "no signature is trusted",
        // legacy 2.x phrasing
        "is not signed by any of the keys",
        "no signatures matched",
        // narinfo-level failure on substituter side
        "signature mismatch",
        "untrusted signature",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// `nix-store --realise <path>` — fetch + verify, return the realised
/// path from stdout. nix-store prints one path per line; we expect
/// exactly one (we passed exactly one input).
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
            // Trim to last ~500 bytes — enough for the matching line
            // plus context, capped so a chatty trace doesn't bloat
            // the ReportEvent body.
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

/// Local rollback: revert the system profile one generation back and
/// run the previous closure's `switch-to-configuration switch`.
/// Used when:
/// - `activate()` returned a non-success outcome that requires
///   rollback (`SwitchFailed`, `VerifyMismatch`).
/// - The agent's confirm window expired before the CP acknowledged
///   the activation (magic rollback, RFC-0003 §4.2).
///
/// `nix-env --rollback` flips the system profile symlink to the
/// previous generation; `/run/current-system/bin/switch-to-
/// configuration switch` then re-runs the activation script of the
/// (now previous) closure. Bypasses `nixos-rebuild` entirely — the
/// new `nixos-rebuild-ng` (Python rewrite shipped in 26.05) tries
/// to evaluate `<nixpkgs/nixos>` even on `--rollback`, which fails
/// in the agent's NIX_PATH-less sandbox.
///
/// Idempotent — running rollback twice in a row reverts twice. The
/// caller is expected to invoke this exactly once per failed
/// activation.
pub async fn rollback() -> Result<ExitStatus> {
    tracing::warn!("agent: triggering local rollback (nix-env --rollback + switch-to-configuration)");
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
            "agent: nix-env --rollback failed; not running switch-to-configuration",
        );
        return Ok(env_status);
    }

    // /run/current-system now points at the rolled-back generation
    // (nix-env --rollback updated the system profile symlink, and
    // /run/current-system tracks the profile target).
    let status = Command::new("/run/current-system/bin/switch-to-configuration")
        .arg("switch")
        .status()
        .await
        .with_context(|| "spawn /run/current-system/bin/switch-to-configuration switch")?;
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
