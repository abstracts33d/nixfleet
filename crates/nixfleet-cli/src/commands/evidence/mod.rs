//! `nixfleet evidence` subcommand tree.
//!
//! Aggregates per-host signed compliance evidence into a canonical
//! fleet-wide evidence record. The aggregator runs operator-side and
//! signs nothing: it stitches signed inputs and adds nothing to the
//! cryptographic chain. The control plane is not in the loop; per-host
//! signatures remain the only trust anchor.
//!
//! Trust posture: the wrapper record is treated as untrusted stitching.
//! The auditor re-verifies each per-host signature independently via
//! `nixfleet-compliance-verify`. Tampering with the wrapper either
//! breaks an embedded per-host signature or is caught by recomputing
//! the deterministic summary block from the canonical `hosts` array.
//!
//! Hosts are sourced from `fleet.resolved.json` (the signed release
//! artefact). The signature on `fleet.resolved.json` is what makes
//! "what we tried to deploy" == "what we collected evidence from."
//!
//! See `staging/audit-report-generator/00-m1-plan.md` for the full
//! design including the M1.0 - M1.7 phase decomposition.

use anyhow::Result;
use clap::Subcommand;

pub mod assemble;
pub mod collect;
pub mod collector;
pub mod collectors;
pub mod enumerate;
pub mod fetch_local;
pub mod fetch_ssh;
pub mod schema;
pub mod verify;
pub mod verify_host;

#[derive(Subcommand, Debug)]
pub enum EvidenceCommands {
    /// Aggregate per-host signed evidence into a fleet-evidence record.
    Collect(collect::Args),
    /// Re-verify signatures on an existing fleet-evidence record.
    Verify(verify::Args),
}

pub async fn dispatch(cmd: EvidenceCommands) -> Result<()> {
    match cmd {
        EvidenceCommands::Collect(args) => collect::run(args).await,
        EvidenceCommands::Verify(args) => verify::run(args).await,
    }
}
