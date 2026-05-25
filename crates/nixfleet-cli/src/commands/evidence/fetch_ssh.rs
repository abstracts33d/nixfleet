//! Per-host SSH fetch via subprocess to system `ssh`.
//!
//! Trust posture: this module fetches bytes; it does not verify them.
//! M1.3 verifies the per-host ed25519 signature against the pubkey
//! declared in `fleet.resolved.json`. The bytes captured here are
//! recorded verbatim so verification is reproducible offline.
//!
//! Why subprocess to system `ssh` instead of a Rust SSH client:
//! the operator is already SSH-authenticated to the fleet for day-2
//! ops. Their `~/.ssh/config`, ssh-agent, identity files, jump hosts,
//! ProxyCommand and ControlMaster all transfer for free. A pure-Rust
//! client would have to re-parse all of that. See the master plan's
//! SSH transport decision section.

use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// Files we pull from each host. Live at `/var/lib/nixfleet-compliance/`
/// per the nixfleet-compliance evidence layer.
pub const REMOTE_EVIDENCE_DIR: &str = "/var/lib/nixfleet-compliance";
pub const EVIDENCE_FILENAME: &str = "evidence.json";
pub const SIGNATURE_FILENAME: &str = "evidence.json.sig";
pub const HOST_PUBKEY_FILENAME: &str = "evidence.host.pub";
/// Optional hardware inventory blob produced by a host-side
/// `nixos-facter` probe. Best-effort: missing is OK and the
/// `facter` collector records `available: false` in that case.
pub const FACTER_FILENAME: &str = "facter.json";

/// Default cap on in-flight host fetches. SSH multiplexing handles
/// a dozen-ish concurrent in practice; 16 leaves headroom on larger
/// fleets without pushing the operator's ssh-agent.
pub const MAX_PARALLEL_FETCHES: usize = 16;

/// Per-host fetch outcome. Recorded verbatim in the fleet-evidence
/// record (M1.4 wraps a `Vec<FetchedHost>` into the canonical schema).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchedHost {
    pub hostname: String,
    pub source: &'static str,
    pub ok: bool,
    pub error: Option<String>,
    /// Verbatim file bytes. `None` when `ok = false`. M1.3 reads
    /// these for ed25519 verification.
    pub evidence_json: Option<Vec<u8>>,
    pub signature: Option<Vec<u8>>,
    pub host_pubkey: Option<Vec<u8>>,
    /// Best-effort: populated only when the host produces a
    /// `facter.json` alongside the evidence files. Absent = facter
    /// not deployed on this host, which the `facter` collector
    /// records as `available: false`. Fetch failure for this file
    /// never flips `ok` to false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facter_json: Option<Vec<u8>>,
}

/// Operator-side connection parameters. `user = None` means
/// "inherit from operator's ssh config" (no `-l` flag passed).
#[derive(Debug, Clone)]
pub struct SshParams {
    pub user: Option<String>,
    pub port: u16,
    pub connect_timeout: Duration,
}

impl Default for SshParams {
    fn default() -> Self {
        Self {
            user: None,
            port: 22,
            connect_timeout: Duration::from_secs(10),
        }
    }
}

/// Fetch the three required files from a single host (one `ssh` per
/// file so each failure is isolated and `FetchedHost.error` carries
/// the specific stderr tail). On full success, attempt one extra
/// best-effort fetch for `facter.json`. The facter fetch never flips
/// `ok` to false.
async fn fetch_one_host(hostname: String, params: SshParams) -> FetchedHost {
    let mut error: Option<String> = None;
    let mut evidence_json: Option<Vec<u8>> = None;
    let mut signature: Option<Vec<u8>> = None;
    let mut host_pubkey: Option<Vec<u8>> = None;

    for (filename, slot) in [
        (EVIDENCE_FILENAME, &mut evidence_json),
        (SIGNATURE_FILENAME, &mut signature),
        (HOST_PUBKEY_FILENAME, &mut host_pubkey),
    ] {
        match ssh_cat(&hostname, &params, filename).await {
            Ok(bytes) => *slot = Some(bytes),
            Err(e) => {
                error = Some(format!("fetch {filename}: {e}"));
                break;
            }
        }
    }

    let ok = error.is_none();
    let facter_json = if ok {
        ssh_cat(&hostname, &params, FACTER_FILENAME).await.ok()
    } else {
        None
    };

    FetchedHost {
        hostname,
        source: "ssh",
        ok,
        error,
        evidence_json,
        signature,
        host_pubkey,
        facter_json,
    }
}

/// `ssh <host> cat <remote-path>`. Subprocess captures stdout
/// verbatim; non-zero exit + stderr surface as `Err`. `BatchMode=yes`
/// refuses password prompts so the operator's ssh-agent / identity
/// file is the only credential path.
async fn ssh_cat(hostname: &str, params: &SshParams, filename: &str) -> Result<Vec<u8>> {
    let mut cmd = Command::new("ssh");
    cmd.arg("-o").arg("BatchMode=yes");
    cmd.arg("-o").arg(format!(
        "ConnectTimeout={}",
        params.connect_timeout.as_secs()
    ));
    if params.port != 22 {
        cmd.arg("-p").arg(params.port.to_string());
    }
    let target = match &params.user {
        Some(u) => format!("{u}@{hostname}"),
        None => hostname.to_string(),
    };
    cmd.arg(target);
    cmd.arg("cat");
    cmd.arg(format!("{REMOTE_EVIDENCE_DIR}/{filename}"));
    let out = cmd.output().await?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail = stderr
            .lines()
            .rev()
            .take(3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" | ");
        anyhow::bail!(
            "ssh {hostname} cat {filename} exited {:?}: {tail}",
            out.status.code()
        );
    }
    Ok(out.stdout)
}

/// Fan-out fetch over a host list. Bounded parallelism via semaphore.
/// Preserves input ordering in the returned `Vec` by carrying the
/// task's index alongside its result.
pub async fn fetch_all(
    hosts: Vec<String>,
    params: SshParams,
    max_parallel: usize,
) -> Vec<FetchedHost> {
    let sem = std::sync::Arc::new(Semaphore::new(max_parallel.max(1)));
    let task_count = hosts.len();
    let mut tasks = JoinSet::new();
    for (idx, hostname) in hosts.into_iter().enumerate() {
        let permit = sem.clone();
        let p = params.clone();
        tasks.spawn(async move {
            let _permit = permit.acquire_owned().await.expect("semaphore not closed");
            (idx, fetch_one_host(hostname, p).await)
        });
    }
    let mut results: Vec<Option<FetchedHost>> = (0..task_count).map(|_| None).collect();
    while let Some(joined) = tasks.join_next().await {
        let (idx, outcome) = joined.expect("task panicked");
        results[idx] = Some(outcome);
    }
    results
        .into_iter()
        .map(|o| o.expect("all filled"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetched_host_serializes_with_explicit_source() {
        let fh = FetchedHost {
            hostname: "h1".into(),
            source: "ssh",
            ok: true,
            error: None,
            evidence_json: Some(b"{}".to_vec()),
            signature: Some(b"sig".to_vec()),
            host_pubkey: Some(b"pub".to_vec()),
            facter_json: None,
        };
        let v = serde_json::to_value(&fh).unwrap();
        assert_eq!(v["source"], "ssh");
        assert_eq!(v["ok"], true);
    }

    #[test]
    fn ssh_params_default_port_22_no_user() {
        let p = SshParams::default();
        assert_eq!(p.port, 22);
        assert!(p.user.is_none());
        assert_eq!(p.connect_timeout, Duration::from_secs(10));
    }

    #[tokio::test]
    async fn fetch_all_preserves_input_order_on_empty_list() {
        let out = fetch_all(vec![], SshParams::default(), 4).await;
        assert!(out.is_empty());
    }
}
