//! Per-host primitives the checkin pipeline needs from the OS.
//!
//! The trait draws the seam between the agent's platform-agnostic
//! checkin body assembly and the OS-specific calls that build it
//! (per-boot identifiers, "is a generation pending a reboot"). Two
//! impls live in sibling modules, each gated at the module level so
//! the file body is free of `#[cfg]` sprinkles. Call sites use the
//! `Host` alias and never see the OS.
//!
//! Adding a new platform = one new sibling module + one new alias
//! arm. Adding a new primitive = one new trait method + one impl
//! per platform. The control plane and proto crates never see
//! `cfg(target_os)` — only this boundary does.

use anyhow::Result;
use nixfleet_proto::agent_wire::{GenerationRef, PendingGeneration};

use crate::checkin_state::current_closure_hash;

/// What the agent reads from the host to fill the checkin body.
///
/// Operations here are factual ("what is true about this host right
/// now") and string-equality-comparable on the wire. Semantic
/// differences between platforms (e.g. nix-darwin has no
/// booted-vs-activated split) are encoded in the contract — they
/// surface as `Ok(None)` rather than as `cfg` gates at call sites.
pub trait HostFacts {
    /// Per-boot identifier: stable for the lifetime of the running
    /// kernel, rotates on reboot. The CP correlates it with
    /// `pendingGeneration` clearing on the next checkin to detect
    /// that a host actually rebooted. Any stable per-boot string
    /// suffices — the only operation against the value is string
    /// equality.
    fn boot_id(&self) -> Result<String>;

    /// Generation queued for next reboot, when the platform has a
    /// "booted vs activated" split (Linux/NixOS via
    /// `/run/booted-system`). Returns `Ok(None)` on platforms where
    /// activation is inline and there is no kernel reboot in the
    /// activation path (nix-darwin via `darwin-rebuild switch`).
    fn pending_generation(&self) -> Result<Option<PendingGeneration>>;

    /// Build the `currentGeneration` GenerationRef. Composes the
    /// shared `current_closure_hash` reader with `boot_id`.
    /// `channel_ref` is `None` until the agent's channel is
    /// correlated by the projection.
    fn current_generation_ref(&self) -> Result<GenerationRef> {
        Ok(GenerationRef {
            closure_hash: current_closure_hash()?,
            channel_ref: None,
            boot_id: self.boot_id()?,
        })
    }
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod darwin;

#[cfg(target_os = "linux")]
pub use linux::LinuxHost as Host;
#[cfg(target_os = "macos")]
pub use darwin::DarwinHost as Host;
