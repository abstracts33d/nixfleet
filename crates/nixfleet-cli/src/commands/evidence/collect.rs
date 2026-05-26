//! `nixfleet evidence collect` — operator-side aggregator entry point.
//!
//! Reads `fleet.resolved.json`, enumerates declared hosts, fetches
//! per-host signed evidence (subprocess `ssh` or local fixture
//! directory), verifies signatures against the pubkeys declared in
//! `fleet.resolved.json`, runs the configured `EvidenceCollector`
//! adapters, then stitches the per-host outcomes into a canonical
//! JCS-canonical fleet-evidence record on disk.
//!
//! The `normalize_rollout_policies` call in `run` is the canary for
//! a deferred lift: every other consumer of `fleet.resolved.json`
//! routes through `nixfleet_reconciler::verify::verify_artifact`
//! returning `Verified<FleetResolved>` (which applies that step
//! internally). Wiring `verify_artifact` operator-side needs a new
//! trust-root config field (CLI flag + env + `config init` UX) —
//! see master plan row M1.8 for the deferred-with-rationale entry.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::Utc;
use clap::ValueEnum;
use nixfleet_proto::fleet_resolved::{FleetResolved, normalize_rollout_policies};

/// Where the aggregator pulls per-host evidence bytes from. `ssh`
/// is the default operator workflow; `local` is the offline /
/// fixture-replay path documented in the M1.6 reference.
#[derive(Clone, Debug, ValueEnum, Default)]
#[clap(rename_all = "lowercase")]
pub enum FetchSourceKind {
    #[default]
    Ssh,
    Local,
}

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Path to `fleet.resolved.json` (the signed release artefact that
    /// declares the host set + expected per-host pubkeys).
    #[arg(long)]
    pub fleet: PathBuf,

    /// Output path for the fleet-evidence record. Defaults to
    /// `./fleet-evidence-<UTC>.json` in the current working directory.
    #[arg(long)]
    pub out: Option<PathBuf>,

    /// Optional fleet identifier embedded in the record metadata.
    #[arg(long)]
    pub fleet_name: Option<String>,

    /// Optional operator identity (human label) embedded in the
    /// record metadata. Defaults to the operator's username when
    /// unset.
    #[arg(long)]
    pub operator_identity: Option<String>,

    /// Fetch source. `ssh` (default) pulls live from each host via
    /// subprocess to system `ssh`. `local` reads pre-staged files
    /// from `--source-dir`; useful for offline E2E + audit re-runs.
    #[arg(long, default_value_t = FetchSourceKind::Ssh, value_enum)]
    pub source: FetchSourceKind,

    /// Required when `--source local`. Root directory containing
    /// `<hostname>/{evidence.json, .sig, evidence.host.pub, facter.json?}`
    /// subtrees. See `docs/reference/evidence-collect.md`.
    #[arg(long)]
    pub source_dir: Option<PathBuf>,

    /// SSH user override (defaults to the operator's current user).
    /// Ignored when `--source local`.
    #[arg(long)]
    pub ssh_user: Option<String>,

    /// SSH port. Ignored when `--source local`.
    #[arg(long, default_value_t = 22)]
    pub ssh_port: u16,

    /// Continue collection when an individual host fetch fails.
    /// The fleet-evidence record records the failure; the run does
    /// not abort.
    #[arg(long, default_value_t = true)]
    pub continue_on_error: bool,
}

pub async fn run(args: Args) -> Result<()> {
    let bytes = fs::read(&args.fleet)
        .with_context(|| format!("reading fleet.resolved.json from {}", args.fleet.display()))?;
    let mut fleet: FleetResolved =
        serde_json::from_slice(&bytes).context("parsing fleet.resolved.json")?;
    // `verify_artifact` applies this internally; mirrored here so the
    // raw-load path produces the same in-memory shape every other
    // consumer sees. Removed when M1.8 swaps to `verify_artifact`.
    normalize_rollout_policies(&mut fleet);

    let hosts = super::enumerate::enumerate(&fleet);
    println!(
        "evidence collect: enumerated {} host(s) from {}",
        hosts.len(),
        args.fleet.display(),
    );

    let hostnames: Vec<String> = hosts.iter().map(|(n, _)| (*n).to_string()).collect();

    let fetched = match args.source {
        FetchSourceKind::Ssh => {
            let params = super::fetch_ssh::SshParams {
                user: args.ssh_user.clone(),
                port: args.ssh_port,
                ..Default::default()
            };
            super::fetch_ssh::fetch_all(hostnames, params, super::fetch_ssh::MAX_PARALLEL_FETCHES)
                .await
        }
        FetchSourceKind::Local => {
            let dir = args
                .source_dir
                .clone()
                .ok_or_else(|| anyhow::anyhow!("--source-dir is required when --source=local"))?;
            super::fetch_local::fetch_all_local(hostnames, dir).await
        }
    };

    let collectors: Vec<Box<dyn super::collector::EvidenceCollector>> = vec![
        Box::new(super::collectors::nix_derivation::NixDerivationCollector),
        Box::new(super::collectors::facter::FacterCollector),
        Box::new(super::collectors::osquery::OsqueryCollector),
    ];

    let mut inputs: Vec<super::assemble::PerHostInput> = Vec::with_capacity(fetched.len());
    let mut ok_count = 0usize;
    let mut sig_valid = 0usize;
    let mut sig_invalid = 0usize;
    let mut pubkey_mismatch = 0usize;

    for fh in fetched {
        if fh.ok {
            ok_count += 1;
        }
        let declared_pubkey = fleet
            .hosts
            .get(&fh.hostname)
            .and_then(|h| h.pubkey.as_deref());
        let verification = super::verify_host::verify_fetched(&fh, declared_pubkey);
        if verification.valid {
            sig_valid += 1;
        } else if verification.present {
            sig_invalid += 1;
        }
        if matches!(
            verification.pubkey_matches_declared,
            super::verify_host::PubkeyMatch::Mismatch
        ) {
            pubkey_mismatch += 1;
        }

        let mut collector_entries: Vec<super::collector::CollectorEntry> = Vec::new();
        if fh.ok
            && let Some(host_record) = fleet.hosts.get(&fh.hostname)
        {
            let ctx = super::collector::CollectorContext {
                hostname: &fh.hostname,
                host: host_record,
                fetched: Some(&fh),
            };
            for c in &collectors {
                match c.collect(&ctx) {
                    Ok(entry) => collector_entries.push(entry),
                    Err(e) => eprintln!("  collector {} on {}: {e}", c.id(), fh.hostname),
                }
            }
        }

        // Per-host status line.
        let sig_label = match (verification.present, verification.valid) {
            (true, true) => "sig=valid",
            (true, false) => "sig=INVALID",
            (false, _) => "sig=absent",
        };
        let match_label = match verification.pubkey_matches_declared {
            super::verify_host::PubkeyMatch::Match => "pub=match",
            super::verify_host::PubkeyMatch::Mismatch => "pub=MISMATCH",
            super::verify_host::PubkeyMatch::DeclaredAbsent => "pub=undeclared",
            super::verify_host::PubkeyMatch::FetchedAbsent => "pub=absent",
        };
        let fetch_label = if fh.ok { "fetch=ok" } else { "fetch=FAIL" };
        let trailer = fh
            .error
            .clone()
            .or_else(|| verification.error.clone())
            .unwrap_or_else(|| "-".to_string());
        println!(
            "  {host:30}  {fetch_label:10}  {sig_label:13}  {match_label:14}  {trailer}",
            host = fh.hostname,
        );
        let collector_label = collector_entries
            .iter()
            .map(|e| e.collector_id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if !collector_label.is_empty() {
            println!("      collectors: {collector_label}");
        }

        inputs.push(super::assemble::PerHostInput {
            fetched: fh,
            verification,
            collectors: collector_entries,
        });
    }

    let now = Utc::now();
    let operator_identity = args.operator_identity.or_else(|| Some(whoami::username()));
    let record = super::assemble::assemble(
        &fleet,
        args.fleet_name.clone(),
        operator_identity,
        inputs,
        now,
    );

    let out_path = args.out.clone().unwrap_or_else(|| {
        let stamp = now.format("%Y%m%dT%H%M%SZ");
        PathBuf::from(format!("fleet-evidence-{stamp}.json"))
    });
    // JCS-canonicalize the output. Re-running with the same inputs +
    // same `now` must produce byte-identical bytes; operator + auditor
    // diff outputs and trust the diff reflects state change, not
    // serializer state. Trailing newline is outside the JSON value
    // and is the operator-friendly cat convention.
    let mut bytes =
        serde_jcs::to_vec(&record).context("JCS-canonicalizing fleet-evidence record")?;
    bytes.push(b'\n');
    fs::write(&out_path, &bytes)
        .with_context(|| format!("writing {} ({} bytes)", out_path.display(), bytes.len()))?;

    println!(
        "evidence collect: {ok}/{total} fetched, {sv} signatures valid, \
         {si} invalid, {pm} pubkey mismatch(es); record written to {out} ({n} bytes)",
        ok = ok_count,
        total = record.summary.hosts_total,
        sv = sig_valid,
        si = sig_invalid,
        pm = pubkey_mismatch,
        out = out_path.display(),
        n = bytes.len(),
    );

    Ok(())
}
