//! Activation pipeline. Restored from main commit `7469b4bd^` (the parent of
//! Phase 8d-1's "delete legacy activation/ pipeline + dead comms surface"
//! sweep). The 8d-1 deletion was premature: Phase 7's runtime worker
//! replacement (`runtime/workers/activation.rs`) was a fire-and-forget
//! shim that did NOT preserve the operational logic the pipeline encoded.
//! Subsequent demo + lab validation (post-D-016 cascade) surfaced the
//! regression chain (OBSERVATION-018/019/020). D-026 closes it by
//! restoring the pipeline and wiring it into the existing v0.2 worker
//! entry point.
//!
//! The pipeline preserves seven LOADBEARING operational steps that the
//! shim dropped:
//!
//!   1. `realise.rs` — `nix-store --realise` forces fetch + signature
//!      verification of the target closure with trust-failure detection
//!      distinct from generic failure (SignatureMismatch outcome).
//!   2. `profile.rs::set` — `nix-env --set` flips
//!      `/nix/var/nix/profiles/system` to point at the target. Required
//!      before fire because `switch-to-configuration` reads that profile
//!      to derive the generation number it writes into bootloader entries.
//!   3. Pre-switch basename read — captured for `verify_poll`'s
//!      flip-to-unexpected detection (third basename = symlink got
//!      switched to something OTHER than expected OR previous; caller
//!      rolls back).
//!   4. `linux::fire_switch` / `darwin::fire_switch` — per-OS detached
//!      subprocess invocation. Linux uses `systemd-run --unit` (cgroup
//!      isolation so agent SIGTERM cannot kill mid-switch); Darwin uses
//!      `setsid()` to detach from agent's process group (launchd reload
//!      sends SIGTERM to the whole group).
//!   5. `verify_poll.rs` — polls `/run/current-system` until expected,
//!      with three outcomes: Settled, Timeout, FlippedToUnexpected.
//!      POLL_BUDGET (300s) locked to CP's DEFAULT_CONFIRM_DEADLINE_SECS
//!      (360s) minus slack — exceeding splits state.
//!   6. `profile::self_correct_profile` — recovers when the activation
//!      script re-pointed the profile (some closures do this). Non-fatal
//!      if self-correction itself fails (the live switch still happened).
//!   7. `linux::detect_switch_inhibitors` — detects critical-component
//!      swaps (dbus, systemd, kernel) that nixos-rebuild refuses to
//!      live-switch. DeferredPendingReboot outcome triggers
//!      `switch-to-configuration boot` (writes bootloader entries) so
//!      the next reboot lands on the new generation.
//!
//! The legacy v0.1-shaped `activate(target: &EvaluatedTarget)` entry
//! point + `comms::confirm()` post-activation report are NOT restored —
//! they belonged to the pre-v0.2 architecture. The v0.2 entry is
//! `runtime::workers::activation::handle_intent(ActivationIntent)`,
//! which constructs an `ActivationTarget` from the intent and calls
//! `activate_with(&DEFAULT_BACKEND, &target)`. Completion / failure
//! flows back through the existing `Event::LocalActivation*` reducer
//! events; CP reads from the outbound queue (no separate confirm HTTP
//! call needed in v0.2).

pub mod darwin;
pub mod linux;
pub mod pipeline;
pub mod profile;
pub mod realise;
pub mod rollback;
pub mod types;
pub mod verify_poll;

pub use pipeline::activate_with;
pub use rollback::rollback_with;
pub use types::{
    ActivationBackend, ActivationOutcome, ActivationTarget, DEFAULT_BACKEND, DefaultBackend,
    POLL_BUDGET, POLL_INTERVAL, RollbackOutcome,
};

/// High-level activate facade — production callers use `DEFAULT_BACKEND`.
/// Test paths that need a stub backend call `activate_with` directly.
pub async fn activate(target: &ActivationTarget) -> anyhow::Result<ActivationOutcome> {
    activate_with(&DEFAULT_BACKEND, target).await
}

/// High-level rollback facade.
pub async fn rollback() -> anyhow::Result<RollbackOutcome> {
    rollback_with(&DEFAULT_BACKEND).await
}
