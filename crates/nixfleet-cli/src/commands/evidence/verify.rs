//! M1.0 scaffold: `nixfleet evidence verify`.
//!
//! Re-verifies signatures on an existing fleet-evidence record. Defensive
//! double-check tool: an auditor can run this against a record we shipped
//! to confirm every per-host signature still verifies against the included
//! pubkey, without trusting the operator who produced the aggregate.
//!
//! Implementation lands alongside the fleet-evidence schema in M1.4 /
//! M1.5 (the same types are needed for parse + verify as for collect's
//! output assembly).

use std::path::PathBuf;

use anyhow::{Result, bail};

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Path to the fleet-evidence record to verify.
    #[arg(long)]
    pub record: PathBuf,
}

pub async fn run(_args: Args) -> Result<()> {
    bail!(
        "nixfleet evidence verify: not implemented yet (M1.0 scaffold). \
         Implementation lands with M1.4-M1.5; see staging/audit-report-generator/00-m1-plan.md."
    );
}
