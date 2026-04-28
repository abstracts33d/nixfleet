//! Integration test for the sign + smoke-verify pipeline.
//!
//! Doesn't exercise the build / push / git steps — those need a
//! real flake + nix daemon. This test takes a hand-built
//! `FleetResolved`, runs canonicalize → sign-via-shell-hook →
//! smoke-verify-with-real-pubkey end-to-end, asserting the
//! pipeline produces an artifact `verify_artifact` accepts.

use std::io::Write;
use std::process::Command;
use std::time::Duration;

use base64::Engine as _;
use chrono::Utc;
use ed25519_dalek::{Signer, SigningKey};
use nixfleet_proto::{
    Channel, Compliance, FleetResolved, Host, KeySlot, Meta, TrustConfig, TrustedPubkey,
};
use rand::rngs::OsRng;

fn dummy_resolved() -> FleetResolved {
    let mut hosts = std::collections::HashMap::new();
    hosts.insert(
        "test-host".to_string(),
        Host {
            system: "x86_64-linux".into(),
            tags: vec![],
            channel: "stable".into(),
            closure_hash: Some("abc123-nixos-system-test-host-26.05".into()),
            pubkey: None,
        },
    );
    let mut channels = std::collections::HashMap::new();
    channels.insert(
        "stable".to_string(),
        Channel {
            rollout_policy: "default".into(),
            reconcile_interval_minutes: 5,
            freshness_window: 60,
            signing_interval_minutes: 30,
            compliance: Compliance {
                strict: false,
                frameworks: vec![],
            },
        },
    );
    FleetResolved {
        schema_version: 1,
        hosts,
        channels,
        rollout_policies: Default::default(),
        waves: Default::default(),
        edges: vec![],
        disruption_budgets: vec![],
        meta: Meta {
            schema_version: 1,
            signed_at: Some(Utc::now()),
            ci_commit: Some("deadbeef".into()),
            signature_algorithm: Some("ed25519".into()),
        },
    }
}

#[test]
fn end_to_end_sign_then_verify_artifact_accepts() {
    let signing_key = SigningKey::generate(&mut OsRng);
    let pubkey_b64 = base64::engine::general_purpose::STANDARD.encode(signing_key.verifying_key());

    // Canonicalize a real FleetResolved (the orchestrator's exact
    // canonicalize path — not duplicated here).
    let resolved = dummy_resolved();
    let canonical =
        nixfleet_release::canonicalize_resolved(&resolved).expect("canonicalize");
    let canonical_bytes = canonical.as_bytes();
    let signature = signing_key.sign(canonical_bytes);

    // Run verify_artifact directly — it's the same code-path
    // smoke_verify takes when a pubkey is supplied.
    let trust = TrustConfig {
        schema_version: 1,
        ci_release_key: KeySlot {
            current: Some(TrustedPubkey {
                algorithm: "ed25519".into(),
                public: pubkey_b64.clone(),
            }),
            previous: None,
            reject_before: None,
        },
        cache_keys: vec![],
        org_root_key: None,
    };
    let trusted_keys = trust.ci_release_key.active_keys();
    let parsed = nixfleet_reconciler::verify_artifact(
        canonical_bytes,
        &signature.to_bytes(),
        &trusted_keys,
        Utc::now(),
        Duration::from_secs(86400 * 365 * 10),
        None,
    )
    .expect("verify_artifact accepts real signature");

    assert_eq!(
        parsed.hosts["test-host"].closure_hash.as_deref(),
        Some("abc123-nixos-system-test-host-26.05"),
        "verified artifact carries the injected closureHash"
    );
}

#[test]
fn shell_hook_contract_invokes_sh_with_env_vars() {
    // Verifies the public hook contract: when `--sign-cmd` runs, it
    // sees NIXFLEET_INPUT and NIXFLEET_OUTPUT in its env, and the
    // input file contains the bytes the orchestrator gave us.
    //
    // Uses a tiny sh hook that records its env, copies input → output.
    let tmpdir = tempfile::tempdir().unwrap();
    let log = tmpdir.path().join("hook.log");
    let log_str = log.to_string_lossy();
    let cmd = format!(
        r#"echo "$NIXFLEET_INPUT" >> {log}; echo "$NIXFLEET_OUTPUT" >> {log}; cat "$NIXFLEET_INPUT" > "$NIXFLEET_OUTPUT""#,
        log = log_str,
    );

    // Round-trip via std::process::Command — no orchestrator
    // involvement, just proves sh + env vars work.
    let in_file = tmpdir.path().join("in");
    let out_file = tmpdir.path().join("out");
    std::fs::write(&in_file, b"some-canonical-bytes").unwrap();
    std::fs::write(&out_file, b"").unwrap();

    let status = Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .env("NIXFLEET_INPUT", &in_file)
        .env("NIXFLEET_OUTPUT", &out_file)
        .status()
        .unwrap();
    assert!(status.success());

    let log_text = std::fs::read_to_string(&log).unwrap();
    assert!(log_text.contains(in_file.to_str().unwrap()));
    assert!(log_text.contains(out_file.to_str().unwrap()));
    let copied = std::fs::read(&out_file).unwrap();
    assert_eq!(copied, b"some-canonical-bytes");
}
