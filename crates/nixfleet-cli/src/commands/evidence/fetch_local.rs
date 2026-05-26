//! Filesystem fetch source. Reads pre-staged per-host evidence
//! files from a local directory instead of pulling over SSH.
//!
//! Two use cases:
//!
//! - **CI / offline E2E**: the test harness synthesizes a fixture
//!   directory and the binary reads from it without any network.
//! - **Operator dogfooding / archival**: an operator captures a
//!   snapshot of every host's `/var/lib/nixfleet-compliance/` into
//!   a single directory tree, then re-runs the aggregator against
//!   it later (audit re-runs, decommission archival, on-call
//!   dry-runs).
//!
//! Expected layout (mirrors the on-host layout under one root):
//!
//! ```text
//! <source-dir>/
//!   <hostname-1>/
//!     evidence.json
//!     evidence.json.sig
//!     evidence.host.pub
//!     facter.json              (optional)
//!     osquery-evidence.json    (optional)
//!   <hostname-2>/
//!     ...
//! ```
//!
//! Missing host directory or missing required file ⇒
//! `FetchedHost { ok: false, error: Some("...") }`. Same semantics
//! as the SSH source: per-host failure is recorded; the run
//! continues.

use std::path::PathBuf;

use super::fetch_ssh::{
    EVIDENCE_FILENAME, FACTER_FILENAME, FetchedHost, HOST_PUBKEY_FILENAME,
    OSQUERY_EVIDENCE_FILENAME, SIGNATURE_FILENAME,
};

/// Read the three required files (plus best-effort facter.json) from
/// `<dir>/<hostname>/`. Synchronous internally; returns an async
/// future for symmetry with `fetch_ssh::fetch_all`.
pub async fn fetch_all_local(hosts: Vec<String>, dir: PathBuf) -> Vec<FetchedHost> {
    hosts
        .into_iter()
        .map(|hostname| fetch_one(&hostname, &dir))
        .collect()
}

fn fetch_one(hostname: &str, dir: &std::path::Path) -> FetchedHost {
    let host_dir = dir.join(hostname);
    let mut error: Option<String> = None;
    let mut evidence_json: Option<Vec<u8>> = None;
    let mut signature: Option<Vec<u8>> = None;
    let mut host_pubkey: Option<Vec<u8>> = None;

    for (filename, slot) in [
        (EVIDENCE_FILENAME, &mut evidence_json),
        (SIGNATURE_FILENAME, &mut signature),
        (HOST_PUBKEY_FILENAME, &mut host_pubkey),
    ] {
        match std::fs::read(host_dir.join(filename)) {
            Ok(bytes) => *slot = Some(bytes),
            Err(e) => {
                error = Some(format!("read {filename} from {}: {e}", host_dir.display()));
                break;
            }
        }
    }

    let ok = error.is_none();
    let (facter_json, osquery_evidence_json) = if ok {
        (
            std::fs::read(host_dir.join(FACTER_FILENAME)).ok(),
            std::fs::read(host_dir.join(OSQUERY_EVIDENCE_FILENAME)).ok(),
        )
    } else {
        (None, None)
    };

    FetchedHost {
        hostname: hostname.to_string(),
        source: "local",
        ok,
        error,
        evidence_json,
        signature,
        host_pubkey,
        facter_json,
        osquery_evidence_json,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fetch_local_missing_host_dir_records_fetch_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let result = fetch_all_local(vec!["nonexistent".into()], tmp.path().to_path_buf()).await;
        assert_eq!(result.len(), 1);
        assert!(!result[0].ok);
        assert!(result[0].error.is_some());
        assert_eq!(result[0].source, "local");
    }

    #[tokio::test]
    async fn fetch_local_reads_three_required_files_and_optional_facter() {
        let tmp = tempfile::tempdir().unwrap();
        let host_dir = tmp.path().join("h1");
        std::fs::create_dir(&host_dir).unwrap();
        std::fs::write(host_dir.join(EVIDENCE_FILENAME), b"{}").unwrap();
        std::fs::write(host_dir.join(SIGNATURE_FILENAME), b"sig\n").unwrap();
        std::fs::write(
            host_dir.join(HOST_PUBKEY_FILENAME),
            b"ssh-ed25519 AAAA test\n",
        )
        .unwrap();
        std::fs::write(host_dir.join(FACTER_FILENAME), b"{\"x\":1}").unwrap();

        let result = fetch_all_local(vec!["h1".into()], tmp.path().to_path_buf()).await;
        assert_eq!(result.len(), 1);
        assert!(result[0].ok);
        assert!(result[0].error.is_none());
        assert_eq!(result[0].evidence_json.as_deref(), Some(b"{}".as_slice()));
        assert!(result[0].facter_json.is_some());
        assert!(result[0].osquery_evidence_json.is_none());
    }

    #[tokio::test]
    async fn fetch_local_reads_optional_osquery_evidence_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        let host_dir = tmp.path().join("h1");
        std::fs::create_dir(&host_dir).unwrap();
        std::fs::write(host_dir.join(EVIDENCE_FILENAME), b"{}").unwrap();
        std::fs::write(host_dir.join(SIGNATURE_FILENAME), b"sig\n").unwrap();
        std::fs::write(
            host_dir.join(HOST_PUBKEY_FILENAME),
            b"ssh-ed25519 AAAA test\n",
        )
        .unwrap();
        std::fs::write(
            host_dir.join(OSQUERY_EVIDENCE_FILENAME),
            br#"[{"name":"os_version","hostIdentifier":"h1"}]"#,
        )
        .unwrap();

        let result = fetch_all_local(vec!["h1".into()], tmp.path().to_path_buf()).await;
        assert_eq!(result.len(), 1);
        assert!(result[0].ok);
        assert!(result[0].osquery_evidence_json.is_some());
    }

    #[tokio::test]
    async fn fetch_local_missing_required_file_marks_host_failed_but_succeeds_run() {
        let tmp = tempfile::tempdir().unwrap();
        let host_dir = tmp.path().join("h1");
        std::fs::create_dir(&host_dir).unwrap();
        std::fs::write(host_dir.join(EVIDENCE_FILENAME), b"{}").unwrap();
        // Missing signature + pubkey.
        let result = fetch_all_local(vec!["h1".into()], tmp.path().to_path_buf()).await;
        assert!(!result[0].ok);
        assert!(
            result[0]
                .error
                .as_deref()
                .unwrap()
                .contains("read evidence.json.sig")
        );
        // Optional facter + osquery not attempted because we never reached
        // the ok branch.
        assert!(result[0].facter_json.is_none());
        assert!(result[0].osquery_evidence_json.is_none());
    }
}
