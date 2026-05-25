//! Operator-side agent-cert revocation declarer. Symmetric to `mint-token`:
//! writes a `RevocationEntry` to the release-side source file, which the
//! release pipeline reads + signs into `releases/revocations.json`. No
//! auto-prune (unlike bootstrap nonces) - revocations are audit-permanent
//! by design.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use clap::Args as ClapArgs;
use nixfleet_proto::RevocationEntry;

#[derive(ClapArgs, Debug)]
#[command(about = "Declare an agent-cert revocation for the signed sidecar.")]
pub struct Args {
    /// Hostname whose certs are being revoked. Must match the CN the
    /// agent presents at mTLS handshake time.
    #[arg(long)]
    hostname: String,

    /// Reject any cert for `hostname` whose `notBefore` is strictly
    /// older than this RFC3339 timestamp. Defaults to "now" so the
    /// freshly-issued revocation invalidates every existing cert
    /// without affecting future enrolments.
    #[arg(long)]
    not_before: Option<String>,

    /// Free-form operator note (decommissioned, compromised, rotated).
    /// Surfaces in audit logs.
    #[arg(long)]
    reason: Option<String>,

    /// Who declared the revocation. Surfaces in audit logs. Defaults
    /// to `$USER`.
    #[arg(long)]
    revoked_by: Option<String>,

    /// Append the entry to the revocations source file consumed by
    /// `nixfleet-release --revocations-file`. Without this flag the
    /// command prints the entry on stdout for the operator to record
    /// manually (dev/test ergonomics).
    #[arg(long)]
    append: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<()> {
    let not_before = match args.not_before.as_deref() {
        Some(ts) => ts
            .parse()
            .with_context(|| format!("parse --not-before {ts:?} as RFC3339"))?,
        None => Utc::now(),
    };
    let revoked_by = args.revoked_by.or_else(|| std::env::var("USER").ok());
    let entry = RevocationEntry {
        hostname: args.hostname,
        not_before,
        reason: args.reason,
        revoked_by,
    };

    match args.append.as_deref() {
        Some(path) => {
            append_revocation_entry(path, &entry)?;
            eprintln!(
                "Appended to {}. Commit + push; CI re-signs the sidecar",
                path.display()
            );
            eprintln!("within the release pipeline's next run.");
        }
        None => {
            let entry_json =
                serde_json::to_string_pretty(&entry).context("serialise RevocationEntry")?;
            println!("{entry_json}");
            eprintln!();
            eprintln!("Append the entry above to the revocations source file");
            eprintln!("(consumed by `nixfleet-release --revocations-file`),");
            eprintln!("or rerun with `--append <path>` to write it automatically.");
        }
    }
    Ok(())
}

/// Read the source file (a JSON array of `RevocationEntry`), append the new
/// entry, and write back atomically. Missing file -> single-entry array.
/// No deduplication - operators can declare overlapping revocations and the
/// signed sidecar carries them all (the audit trail is the value).
fn append_revocation_entry(path: &Path, entry: &RevocationEntry) -> Result<()> {
    let mut entries: Vec<RevocationEntry> = match std::fs::read_to_string(path) {
        Ok(raw) => {
            serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => {
            return Err(anyhow::Error::from(err).context(format!("read {}", path.display())));
        }
    };
    entries.push(entry.clone());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create parent dir of {}", path.display()))?;
    }
    let mut body = serde_json::to_string_pretty(&entries)
        .context("serialise revocations source file")?;
    body.push('\n');
    let tmp = path.with_extension("in.json.tmp");
    std::fs::write(&tmp, &body).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}
