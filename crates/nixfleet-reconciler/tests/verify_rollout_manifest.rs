//! `verify_rollout_manifest` integration + RolloutId canonical-construction discrimination.

mod common;

use chrono::{Duration as ChronoDuration, Utc};
use common::signing::{fresh_signing_key, sign_artifact, trust_root_for};
use ed25519_dalek::Signer;
use nixfleet_canonicalize::canonicalize;
use nixfleet_proto::RolloutId;
use nixfleet_reconciler::{VerifyError, verify_rollout_manifest};
use std::time::Duration;

const FIXTURE_MANIFEST: &str = r#"{
  "schemaVersion": 1,
  "displayName": "stable@def4567",
  "channel": "stable",
  "channelRef": "def4567abc123def4567abc123def4567abc123d",
  "fleetResolvedHash": "1111111111111111111111111111111111111111111111111111111111111111",
  "hostSet": [
    {"hostname": "agent-01", "waveIndex": 0, "targetClosure": "0000000000000000000000000000000000000000-host-a"},
    {"hostname": "agent-02", "waveIndex": 1, "targetClosure": "1111111111111111111111111111111111111111-host-b"}
  ],
  "healthGate": {},
  "complianceFrameworks": ["anssi-bp028"],
  "meta": {
    "schemaVersion": 1,
    "signedAt": "2026-04-30T12:00:00Z",
    "ciCommit": "def45678",
    "signatureAlgorithm": "ed25519"
  }
}"#;

#[test]
fn verify_rollout_manifest_ok_returns_manifest() {
    let (bytes, sig, trust, signed_at) = sign_artifact(FIXTURE_MANIFEST);
    let now = signed_at + ChronoDuration::minutes(30);
    let window = Duration::from_secs(3 * 3600);

    let result = verify_rollout_manifest(
        &bytes,
        &sig,
        std::slice::from_ref(&trust),
        now,
        window,
        None,
    );
    let m = result.expect("verify_rollout_manifest_ok").into_inner();
    assert_eq!(m.schema_version, 1);
    assert_eq!(m.channel, "stable");
    assert_eq!(m.host_set.len(), 2);
    assert_eq!(m.host_set[0].hostname, "agent-01");
    assert_eq!(m.host_set[1].wave_index, 1);
    assert!(m.host_set[0].target_closure.starts_with("0000"));
    assert!(m.host_set[1].target_closure.starts_with("1111"));
}

#[test]
fn verify_rollout_manifest_rejects_tampered_signature() {
    let (bytes, mut sig, trust, signed_at) = sign_artifact(FIXTURE_MANIFEST);
    sig[0] ^= 0xFF;
    let now = signed_at + ChronoDuration::minutes(30);
    let window = Duration::from_secs(3 * 3600);

    let err = verify_rollout_manifest(
        &bytes,
        &sig,
        std::slice::from_ref(&trust),
        now,
        window,
        None,
    )
    .unwrap_err();
    assert!(matches!(err, VerifyError::BadSignature));
}

#[test]
fn verify_rollout_manifest_rejects_stale() {
    let (bytes, sig, trust, signed_at) = sign_artifact(FIXTURE_MANIFEST);
    let now = signed_at + ChronoDuration::hours(4);
    let window = Duration::from_secs(3 * 3600);

    let err = verify_rollout_manifest(
        &bytes,
        &sig,
        std::slice::from_ref(&trust),
        now,
        window,
        None,
    )
    .unwrap_err();
    assert!(matches!(err, VerifyError::Stale { .. }));
}

#[test]
fn rollout_id_for_verified_manifest_is_canonical_composite() {
    // RFC-0012 §6.3: rollout_id is `"{channel}@{channel_ref}"` derived
    // from the parsed manifest's typed fields. Deterministic from the
    // projection inputs (channel + channel_ref) alone; field changes
    // outside that pair do not perturb the id (architectural distinction
    // from a content-addressed-hash shape).
    let (bytes, sig, trust, signed_at) = sign_artifact(FIXTURE_MANIFEST);
    let now = signed_at + ChronoDuration::minutes(30);
    let window = Duration::from_secs(3 * 3600);

    let m = verify_rollout_manifest(
        &bytes,
        &sig,
        std::slice::from_ref(&trust),
        now,
        window,
        None,
    )
    .expect("verify ok")
    .into_inner();

    let id = RolloutId::new(&m.channel, &m.channel_ref);
    assert_eq!(
        id.as_str(),
        "stable@def4567abc123def4567abc123def4567abc123d",
        "rollout_id is the canonical channel@channel_ref composite",
    );

    // Perturbing a field outside (channel, channel_ref) leaves the
    // identifier intact. The id is a content-addressable name for the
    // rollout's identity, not a hash of its bytes.
    let mut m2 = m.clone();
    m2.host_set[0].target_closure =
        "9999999999999999999999999999999999999999-perturbed".to_string();
    let id2 = RolloutId::new(&m2.channel, &m2.channel_ref);
    assert_eq!(id, id2, "id depends only on (channel, channel_ref)");
}

#[test]
fn verify_rollout_manifest_rejects_unsigned() {
    let signing_key = fresh_signing_key();
    let trust = trust_root_for(&signing_key);
    let json = r#"{
      "schemaVersion": 1,
      "displayName": "stable@def4567",
      "channel": "stable",
      "channelRef": "def4567abc123def4567abc123def4567abc123d",
      "fleetResolvedHash": "1111111111111111111111111111111111111111111111111111111111111111",
      "hostSet": [],
      "healthGate": {},
      "complianceFrameworks": [],
      "meta": {
        "schemaVersion": 1,
        "signedAt": null,
        "ciCommit": "def45678",
        "signatureAlgorithm": "ed25519"
      }
    }"#;
    let reserialized =
        serde_json::to_string(&serde_json::from_str::<serde_json::Value>(json).unwrap()).unwrap();
    let canonical = canonicalize(&reserialized).expect("canonicalize");
    let sig = signing_key.sign(canonical.as_bytes()).to_bytes();
    let err = verify_rollout_manifest(
        canonical.as_bytes(),
        &sig,
        std::slice::from_ref(&trust),
        Utc::now(),
        Duration::from_secs(3600),
        None,
    )
    .unwrap_err();
    assert!(
        matches!(err, VerifyError::NotSigned),
        "unsigned manifest must be rejected; got {err:?}"
    );
}

#[test]
fn verify_rollout_manifest_rejects_malformed_json() {
    let signing_key = fresh_signing_key();
    let trust = trust_root_for(&signing_key);
    let canonical = canonicalize(r#"{"not":"a-manifest"}"#).expect("canonicalize");
    let sig = signing_key.sign(canonical.as_bytes()).to_bytes();
    let err = verify_rollout_manifest(
        canonical.as_bytes(),
        &sig,
        std::slice::from_ref(&trust),
        Utc::now(),
        Duration::from_secs(3600),
        None,
    )
    .unwrap_err();
    assert!(
        matches!(err, VerifyError::Parse(_)),
        "expected ParseError, got {err:?}"
    );
}

#[test]
fn verify_rollout_manifest_rejects_when_trust_roots_empty() {
    let (bytes, sig, _trust, signed_at) = sign_artifact(FIXTURE_MANIFEST);
    let now = signed_at + ChronoDuration::minutes(30);
    let err = verify_rollout_manifest(&bytes, &sig, &[], now, Duration::from_secs(3600), None)
        .unwrap_err();
    assert!(
        matches!(err, VerifyError::NoTrustRoots),
        "empty trust roots -> NoTrustRoots; got {err:?}"
    );
}

#[test]
fn verify_rollout_manifest_reject_before_rejects_pre_compromise() {
    let (bytes, sig, trust, signed_at) = sign_artifact(FIXTURE_MANIFEST);
    let now = signed_at + ChronoDuration::minutes(30);
    let reject_before = signed_at + ChronoDuration::seconds(1);
    let err = verify_rollout_manifest(
        &bytes,
        &sig,
        std::slice::from_ref(&trust),
        now,
        Duration::from_secs(3600),
        Some(reject_before),
    )
    .unwrap_err();
    assert!(
        matches!(err, VerifyError::RejectedBeforeTimestamp { .. }),
        "reject_before must apply to rollout manifest; got {err:?}"
    );
}
