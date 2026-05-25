//! `EvidenceCollector` adapter registry.
//!
//! M1.2 ships `nix-derivation` (intent side). M1.3 ships `facter`
//! (hardware side, subprocess-only for GPL v3 hygiene). Post-M1
//! adapters land here: `osquery` via FleetDM, `tpm2-quote`,
//! `securix-attest`.

pub mod facter;
pub mod nix_derivation;
