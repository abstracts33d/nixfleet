//! Per-host primitives the checkin pipeline needs from the OS.
//!
//! Two cfg-gated submodules expose the same public function set
//! (`boot_id`, `pending_generation`); the parent re-exports whichever
//! matches the build target. Call sites use `host_facts::boot_id()`
//! without ever seeing `cfg(target_os)`.
//!
//! Adding a new platform = one new sibling module + one re-export arm.

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
