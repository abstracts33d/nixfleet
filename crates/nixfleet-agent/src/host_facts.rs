//! Per-host primitives the checkin pipeline needs from the OS.
//!
//! Two cfg-gated submodules expose the same public function set
//! (`boot_id`, `pending_generation`); the parent re-exports whichever
//! matches the build target. Call sites use `host_facts::boot_id()`
//! without ever seeing `cfg(target_os)`.
//!
//! Adding a new platform = one new sibling module + one re-export arm.
//!
//! ## Why no `HostFacts` trait?
//!
//! The sibling-`activation` module exposes a similar shape via the
//! `ActivationBackend` trait + cfg-selected `DEFAULT_BACKEND`. That
//! pattern earns its weight there because:
//! 1. dispatch handlers want to substitute a fake backend in unit
//!    tests (the audit-driven Reporter+ActivationBackend extraction
//!    landed for #67), and
//! 2. future SystemManager / MicroVM impls plug in by writing a new
//!    trait impl, no caller change.
//!
//! Neither driver applies here. `boot_id` and `pending_generation`
//! are two thin OS-primitive wrappers; no test wants to fake them
//! (their per-platform impls are themselves the unit tests), and a
//! third platform's `boot_id` is a one-liner sysctl call no different
//! in shape from the existing ones. Promoting this to a trait would
//! be premature abstraction — the simple cfg-gated re-export stays
//! intentionally simpler than `activation`'s shape.

use anyhow::Result;
use nixfleet_proto::agent_wire::GenerationRef;

use crate::checkin_state::current_closure_hash;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod darwin;

#[cfg(target_os = "linux")]
pub use linux::{boot_id, pending_generation};
#[cfg(target_os = "macos")]
pub use darwin::{boot_id, pending_generation};

/// Build the `currentGeneration` GenerationRef. `channel_ref` is
/// `None` until the agent's channel is correlated by the projection.
pub fn current_generation_ref() -> Result<GenerationRef> {
    Ok(GenerationRef {
        closure_hash: current_closure_hash()?,
        channel_ref: None,
        boot_id: boot_id()?,
    })
}
