//! M1.6: offline E2E for `nixfleet evidence collect`.
//!
//! Synthesizes a fleet + per-host ed25519 keypairs + signed evidence
//! files in a tempdir, runs the CLI as a subprocess with
//! `--source local`, parses the output record and asserts the
//! summary shape end-to-end.
//!
//! Live SSH against `nixfleet-demo` is documented in
//! `docs/reference/evidence-collect.md` (the `nix run .#fleet-up`
//! smoke). Operator-only per repo build economy.

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use rand::TryRngCore;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One host fixture: declarative shape plus the signed-evidence
/// files written to disk under `<source-dir>/<hostname>/`.
struct HostFixture {
    hostname: String,
    pubkey_openssh: String,
    /// Whether at least one of the host's controls is `passed: true`.
    /// Used to populate the asserted `summary.controlsByStatus.passed`.
    passed_controls: u32,
}

fn write_host_fixture(source_dir: &Path, hostname: &str) -> HostFixture {
    let host_dir = source_dir.join(hostname);
    std::fs::create_dir(&host_dir).unwrap();

    // Generate per-host ed25519 keypair.
    let mut seed = [0u8; 32];
    rand::rngs::OsRng.try_fill_bytes(&mut seed).unwrap();
    let sk = SigningKey::from_bytes(&seed);

    // Per-host evidence.json (two controls, one NIS2, one ANSSI BP-028).
    let evidence = serde_json::json!({
        "schemaVersion": 1,
        "hostname": hostname,
        "collectedAt": "2026-05-22T10:00:00Z",
        "controls": [
            { "controlId": "BH-01", "passed": true,  "frameworkArticles": ["NIS2-21(d)"] },
            { "controlId": "AL-02", "passed": false, "frameworkArticles": ["ANSSI-BP-028-R8"] }
        ]
    });
    let evidence_str = serde_json::to_string(&evidence).unwrap();
    let canonical = nixfleet_canonicalize::canonicalize(&evidence_str).unwrap();
    let sig = sk.sign(canonical.as_bytes());
    let sig_b64 = base64::engine::general_purpose::STANDARD.encode(sig.to_bytes());

    // OpenSSH-format pubkey, single-line + trailing newline.
    let pk_data = ssh_key::public::KeyData::Ed25519(ssh_key::public::Ed25519PublicKey(
        sk.verifying_key().to_bytes(),
    ));
    let pk = ssh_key::PublicKey::new(pk_data, format!("{hostname}-test"));
    let pub_str = pk.to_openssh().unwrap();

    // Write the three required files.
    std::fs::write(host_dir.join("evidence.json"), evidence_str.as_bytes()).unwrap();
    std::fs::write(
        host_dir.join("evidence.json.sig"),
        format!("{sig_b64}\n").as_bytes(),
    )
    .unwrap();
    std::fs::write(
        host_dir.join("evidence.host.pub"),
        format!("{pub_str}\n").as_bytes(),
    )
    .unwrap();

    HostFixture {
        hostname: hostname.to_string(),
        pubkey_openssh: pub_str,
        passed_controls: 1,
    }
}

fn write_fleet_resolved(dir: &Path, hosts: &[HostFixture]) -> PathBuf {
    let mut host_map = serde_json::Map::new();
    for h in hosts {
        host_map.insert(
            h.hostname.clone(),
            serde_json::json!({
                "platform": "x86_64-linux",
                "tags": [],
                "channel": "stable",
                "pubkey": h.pubkey_openssh,
            }),
        );
    }
    let fleet = serde_json::json!({
        "schemaVersion": 1,
        "hosts": serde_json::Value::Object(host_map),
        "channels": {},
        "waves": {},
        "edges": [],
        "channelEdges": [],
        "disruptionBudgets": [],
        "meta": {
            "schemaVersion": 1,
            "signedAt": null,
            "ciCommit": null,
            "signatureAlgorithm": null,
        }
    });
    let path = dir.join("fleet.resolved.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&fleet).unwrap()).unwrap();
    path
}

fn run_collect(fleet: &Path, source_dir: &Path, out: &Path) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_nixfleet");
    Command::new(bin)
        .args([
            "evidence",
            "collect",
            "--fleet",
            fleet.to_str().unwrap(),
            "--source",
            "local",
            "--source-dir",
            source_dir.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn nixfleet evidence collect")
}

#[test]
fn end_to_end_local_source_produces_valid_record() {
    let tmp = tempfile::tempdir().unwrap();
    let source_dir = tmp.path().join("fixture");
    std::fs::create_dir(&source_dir).unwrap();

    let hosts = vec![
        write_host_fixture(&source_dir, "h1"),
        write_host_fixture(&source_dir, "h2"),
    ];
    let fleet_path = write_fleet_resolved(tmp.path(), &hosts);
    let out_path = tmp.path().join("out.json");

    let output = run_collect(&fleet_path, &source_dir, &out_path);
    assert!(
        output.status.success(),
        "exit code: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let record_bytes = std::fs::read(&out_path).expect("read output");
    let record: serde_json::Value =
        serde_json::from_slice(&record_bytes).expect("output is valid JSON");

    assert_eq!(record["schemaVersion"], 1);
    assert_eq!(
        record["hosts"].as_array().map(Vec::len),
        Some(2),
        "two hosts expected"
    );

    let summary = &record["summary"];
    assert_eq!(summary["hostsTotal"], 2);
    assert_eq!(summary["hostsBySignatureStatus"]["valid"], 2);
    assert_eq!(summary["hostsBySignatureStatus"]["invalid"], 0);
    assert_eq!(summary["hostsByPubkeyMatch"]["match"], 2);
    let total_passed: u32 = hosts.iter().map(|h| h.passed_controls).sum();
    assert_eq!(summary["controlsByStatus"]["passed"], total_passed);
    // Each host has one NIS2 control and one ANSSI-BP-028 control.
    assert_eq!(summary["frameworkCoverage"]["NIS2"]["controlsTracked"], 2);
    assert_eq!(summary["frameworkCoverage"]["NIS2"]["controlsPassed"], 2);
    assert_eq!(
        summary["frameworkCoverage"]["ANSSI-BP-028"]["controlsTracked"],
        2
    );
    assert_eq!(
        summary["frameworkCoverage"]["ANSSI-BP-028"]["controlsPassed"],
        0
    );

    // Hosts in ASCII-ascending order.
    let host_names: Vec<&str> = record["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h.get("hostname").unwrap().as_str().unwrap())
        .collect();
    assert_eq!(host_names, vec!["h1", "h2"]);
}

#[test]
fn end_to_end_local_source_byte_reproducible_modulo_timestamps() {
    let tmp = tempfile::tempdir().unwrap();
    let source_dir = tmp.path().join("fixture");
    std::fs::create_dir(&source_dir).unwrap();
    let _ = write_host_fixture(&source_dir, "h1");
    let _ = write_host_fixture(&source_dir, "h2");
    let fleet_path = write_fleet_resolved(
        tmp.path(),
        &[HostFixture {
            hostname: "h1".into(),
            pubkey_openssh: "ssh-ed25519 AAAA test".into(),
            passed_controls: 0,
        }],
    );

    // Re-declare with the real fixtures' pubkeys this time.
    let mut hosts = serde_json::Map::new();
    for hostname in ["h1", "h2"] {
        let host_dir = source_dir.join(hostname);
        let pubkey = std::fs::read_to_string(host_dir.join("evidence.host.pub")).unwrap();
        hosts.insert(
            hostname.into(),
            serde_json::json!({
                "platform": "x86_64-linux",
                "tags": [],
                "channel": "stable",
                "pubkey": pubkey.trim(),
            }),
        );
    }
    let fleet = serde_json::json!({
        "schemaVersion": 1,
        "hosts": hosts,
        "channels": {},
        "waves": {},
        "edges": [],
        "channelEdges": [],
        "disruptionBudgets": [],
        "meta": {
            "schemaVersion": 1,
            "signedAt": null,
            "ciCommit": null,
            "signatureAlgorithm": null,
        }
    });
    std::fs::write(&fleet_path, serde_json::to_vec_pretty(&fleet).unwrap()).unwrap();

    let out_a = tmp.path().join("a.json");
    let out_b = tmp.path().join("b.json");

    let r1 = run_collect(&fleet_path, &source_dir, &out_a);
    assert!(
        r1.status.success(),
        "{}",
        String::from_utf8_lossy(&r1.stderr)
    );
    let r2 = run_collect(&fleet_path, &source_dir, &out_b);
    assert!(
        r2.status.success(),
        "{}",
        String::from_utf8_lossy(&r2.stderr)
    );

    let mut a: serde_json::Value = serde_json::from_slice(&std::fs::read(&out_a).unwrap()).unwrap();
    let mut b: serde_json::Value = serde_json::from_slice(&std::fs::read(&out_b).unwrap()).unwrap();

    // Zero out wall-clock fields that move per run.
    fn zero_timestamps(v: &mut serde_json::Value) {
        if let serde_json::Value::Object(map) = v {
            for (key, child) in map.iter_mut() {
                if matches!(key.as_str(), "collectedAt" | "fetchedAt" | "verifiedAt") {
                    *child = serde_json::Value::String("ZERO".into());
                } else {
                    zero_timestamps(child);
                }
            }
        } else if let serde_json::Value::Array(arr) = v {
            for child in arr.iter_mut() {
                zero_timestamps(child);
            }
        }
    }
    zero_timestamps(&mut a);
    zero_timestamps(&mut b);

    // After zeroing timestamps, JCS bytes must be identical.
    let ab = serde_jcs::to_vec(&a).unwrap();
    let bb = serde_jcs::to_vec(&b).unwrap();
    assert_eq!(
        ab,
        bb,
        "outputs differ modulo timestamps:\n{}\n!=\n{}",
        serde_json::to_string_pretty(&a).unwrap(),
        serde_json::to_string_pretty(&b).unwrap(),
    );
}
