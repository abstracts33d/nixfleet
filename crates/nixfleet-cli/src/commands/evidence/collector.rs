//! `EvidenceCollector` trait.
//!
//! The architectural commitment that lets adapters for different
//! evidence sources (intent-side `nix-derivation`, hardware-side
//! `facter`, runtime-side `osquery` via FleetDM, boot-integrity
//! `tpm2-quote`, ANSSI-specific `securix-attest`) plug into the same
//! canonical per-host record without schema rework.
//!
//! Two adapters ship in M1 (`nix_derivation` here, `facter` in M1.3).
//! Two adapters validate the trait — one is underspec.

use anyhow::Result;
use nixfleet_proto::fleet_resolved::Host;
use serde::{Deserialize, Serialize};

/// What an adapter writes into the per-host evidence wrapper.
///
/// The fleet-evidence schema (M1.4) wraps a `Vec<CollectorEntry>` per
/// host. The `data` blob is adapter-specific JSON; downstream
/// rendering keys off `collector_id`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CollectorEntry {
    /// Stable identifier of the collector that produced this entry,
    /// e.g. `"nix-derivation"`, `"facter"`. Auditor sees this verbatim.
    pub collector_id: String,
    /// Adapter-specific payload.
    pub data: serde_json::Value,
}

/// Per-host context the collector reads. Borrowed; collectors return
/// owned `CollectorEntry` instances so the operator-side loop can
/// stitch them without lifetime juggling.
///
/// `fetched` carries the per-host SSH fetch outcome (M1.2) — bytes,
/// signature, host pubkey, optional facter blob. Adapters that work
/// from declarative state alone (e.g., `nix_derivation`) ignore it.
/// Adapters that consume host-emitted bytes (e.g., `facter`) read
/// from it. The trait survives both shapes by carrying the union as
/// an `Option`.
pub struct CollectorContext<'a> {
    pub hostname: &'a str,
    pub host: &'a Host,
    pub fetched: Option<&'a super::fetch_ssh::FetchedHost>,
}

/// A pluggable evidence source. Operator-side: synchronous,
/// failure-isolated (returns `Result`; one adapter failing does not
/// abort sibling adapters in the run loop).
pub trait EvidenceCollector {
    fn id(&self) -> &'static str;
    fn collect(&self, ctx: &CollectorContext<'_>) -> Result<CollectorEntry>;
}
