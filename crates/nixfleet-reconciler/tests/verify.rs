//! Step 0 — signature verification + freshness window.

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use ed25519_dalek::{Signer, SigningKey};
use nixfleet_canonicalize::canonicalize;
use nixfleet_proto::TrustedPubkey;
use nixfleet_reconciler::{
    compute_rollout_id, verify_artifact, verify_revocations, verify_rollout_manifest, VerifyError,
};
use rand::rngs::OsRng;
use rand::TryRngCore;
use std::time::Duration;

/// Generate a fresh ed25519 signing key using the OS CSPRNG.
///
/// We go through `rand::rngs::OsRng` (rand 0.9) and feed raw bytes to
/// `SigningKey::from_bytes`, bypassing `SigningKey::generate` — the latter
/// wants a `rand_core` 0.6 `CryptoRngCore`, which rand 0.9's `OsRng` does
/// not implement.
fn fresh_signing_key() -> SigningKey {
    let mut seed = [0u8; 32];
    OsRng.try_fill_bytes(&mut seed).expect("OS CSPRNG");
    SigningKey::from_bytes(&seed)
}

fn trust_root_for(signing_key: &SigningKey) -> TrustedPubkey {
    TrustedPubkey {
        algorithm: "ed25519".to_string(),
        public: BASE64_STANDARD.encode(signing_key.verifying_key().as_bytes()),
    }
}

/// Build a signed fleet.resolved artifact from JSON source.
///
/// Returns (signed_bytes, signature, trust_root, signed_at).
fn sign_artifact(json: &str) -> (Vec<u8>, [u8; 64], TrustedPubkey, DateTime<Utc>) {
    let signing_key = fresh_signing_key();
    let trust = trust_root_for(&signing_key);

    let value: serde_json::Value = serde_json::from_str(json).expect("parse");
    let signed_at: DateTime<Utc> = value["meta"]["signedAt"]
        .as_str()
        .expect("fixture must have meta.signedAt set")
        .parse()
        .expect("parse RFC 3339");

    let reserialized = serde_json::to_string(&value).unwrap();
    let canonical = canonicalize(&reserialized).expect("canonicalize");
    let sig = signing_key.sign(canonical.as_bytes()).to_bytes();

    (canonical.into_bytes(), sig, trust, signed_at)
}

const FIXTURE_SIGNED: &str =
    include_str!("../../nixfleet-proto/tests/fixtures/signed-artifact.json");

#[test]
fn verify_ok_returns_fleet() {
    let (bytes, sig, trust, signed_at) = sign_artifact(FIXTURE_SIGNED);
    let now = signed_at + ChronoDuration::minutes(30);
    let window = Duration::from_secs(3 * 3600);

    let result = verify_artifact(
        &bytes,
        &sig,
        std::slice::from_ref(&trust),
        now,
        window,
        None,
    );

    let fleet = result.expect("verify_ok");
    assert_eq!(fleet.schema_version, 1);
    assert!(fleet.hosts.contains_key("h1"));
}

#[test]
fn verify_bad_signature() {
    let (bytes, mut sig, trust, signed_at) = sign_artifact(FIXTURE_SIGNED);
    sig[0] ^= 0xFF;
    let now = signed_at + ChronoDuration::minutes(30);
    let window = Duration::from_secs(3 * 3600);

    let err = verify_artifact(
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
fn verify_stale() {
    let (bytes, sig, trust, signed_at) = sign_artifact(FIXTURE_SIGNED);
    let now = signed_at + ChronoDuration::hours(4);
    let window = Duration::from_secs(3 * 3600);

    let err = verify_artifact(
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
fn verify_at_exact_window_boundary_is_fresh() {
    let (bytes, sig, trust, signed_at) = sign_artifact(FIXTURE_SIGNED);
    let window_secs: u64 = 3 * 3600;
    let now = signed_at + ChronoDuration::seconds(window_secs as i64);
    let window = Duration::from_secs(window_secs);

    let result = verify_artifact(
        &bytes,
        &sig,
        std::slice::from_ref(&trust),
        now,
        window,
        None,
    );
    assert!(
        result.is_ok(),
        "age == window must be treated as fresh: {result:?}"
    );
}

#[test]
fn verify_within_clock_skew_slack_is_fresh() {
    // RFC-0003 §8: verify_artifact tolerates ≥60s clock
    // skew so a benignly-drifted host doesn't reject a freshly-signed
    // artifact. age = window + 30s must still be fresh.
    let (bytes, sig, trust, signed_at) = sign_artifact(FIXTURE_SIGNED);
    let window_secs: u64 = 3 * 3600;
    let now = signed_at + ChronoDuration::seconds(window_secs as i64 + 30);
    let window = Duration::from_secs(window_secs);

    let result = verify_artifact(
        &bytes,
        &sig,
        std::slice::from_ref(&trust),
        now,
        window,
        None,
    );
    assert!(
        result.is_ok(),
        "age within slack must be treated as fresh: {result:?}"
    );
}

#[test]
fn verify_just_past_slack_is_stale() {
    // age = window + 61s — one second past the 60s slack → stale.
    let (bytes, sig, trust, signed_at) = sign_artifact(FIXTURE_SIGNED);
    let window_secs: u64 = 3 * 3600;
    let now = signed_at + ChronoDuration::seconds(window_secs as i64 + 61);
    let window = Duration::from_secs(window_secs);

    let err = verify_artifact(
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
fn verify_unsigned() {
    let json = include_str!("../../nixfleet-proto/tests/fixtures/every-nullable.json");

    let signing_key = fresh_signing_key();
    let trust = trust_root_for(&signing_key);
    let canonical = canonicalize(json).expect("canonicalize");
    let sig = signing_key.sign(canonical.as_bytes()).to_bytes();

    let now = Utc::now();
    let window = Duration::from_secs(3 * 3600);

    let err = verify_artifact(
        canonical.as_bytes(),
        &sig,
        std::slice::from_ref(&trust),
        now,
        window,
        None,
    )
    .unwrap_err();
    assert!(matches!(err, VerifyError::NotSigned));
}

#[test]
fn verify_rejects_malleable_signature() {
    // Canonical ed25519 signatures have s < L where L is the curve order.
    // verify_strict rejects any s >= L. We construct a malleable sig by
    // adding L to the scalar component — ed25519-dalek 2's verify_strict
    // catches this; the weaker verify would accept it.
    let (bytes, sig, trust, signed_at) = sign_artifact(FIXTURE_SIGNED);

    // L (little-endian 32 bytes) = 2^252 + 27742317777372353535851937790883648493
    const L_LE: [u8; 32] = [
        0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde,
        0x14, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x10,
    ];

    let mut malleable = sig;
    let mut carry: u16 = 0;
    for i in 0..32 {
        let v = malleable[32 + i] as u16 + L_LE[i] as u16 + carry;
        malleable[32 + i] = v as u8;
        carry = v >> 8;
    }

    let now = signed_at + ChronoDuration::minutes(30);
    let window = Duration::from_secs(3 * 3600);

    let result = verify_artifact(
        &bytes,
        &malleable,
        std::slice::from_ref(&trust),
        now,
        window,
        None,
    );
    assert!(
        matches!(result, Err(VerifyError::BadSignature)),
        "verify_strict must reject malleable s >= L: got {result:?}"
    );
}

#[test]
fn verify_unsupported_schema() {
    let mut value: serde_json::Value = serde_json::from_str(FIXTURE_SIGNED).unwrap();
    value["schemaVersion"] = serde_json::json!(2);
    let json = value.to_string();

    let signing_key = fresh_signing_key();
    let trust = trust_root_for(&signing_key);
    let canonical = canonicalize(&json).expect("canonicalize");
    let sig = signing_key.sign(canonical.as_bytes()).to_bytes();

    let signed_at: DateTime<Utc> = value["meta"]["signedAt"].as_str().unwrap().parse().unwrap();
    let now = signed_at + ChronoDuration::minutes(30);
    let window = Duration::from_secs(3 * 3600);

    let err = verify_artifact(
        canonical.as_bytes(),
        &sig,
        std::slice::from_ref(&trust),
        now,
        window,
        None,
    )
    .unwrap_err();
    assert!(matches!(err, VerifyError::SchemaVersionUnsupported(2)));
}

#[test]
fn verify_malformed_json() {
    let signing_key = fresh_signing_key();
    let trust = trust_root_for(&signing_key);
    let bytes = b"{not json";
    let sig = [0u8; 64];

    let err = verify_artifact(
        bytes,
        &sig,
        std::slice::from_ref(&trust),
        Utc::now(),
        Duration::from_secs(60),
        None,
    )
    .unwrap_err();
    assert!(matches!(err, VerifyError::Parse(_)));
}

#[test]
fn verify_tampered_payload() {
    let (bytes, sig, trust, signed_at) = sign_artifact(FIXTURE_SIGNED);
    let mut tampered = bytes.clone();
    if let Some(byte) = tampered.iter_mut().find(|b| **b == b'"') {
        *byte = b'_';
    }
    let now = signed_at + ChronoDuration::minutes(30);
    let window = Duration::from_secs(3 * 3600);

    let err = verify_artifact(
        &tampered,
        &sig,
        std::slice::from_ref(&trust),
        now,
        window,
        None,
    )
    .unwrap_err();
    assert!(
        matches!(err, VerifyError::Parse(_) | VerifyError::BadSignature),
        "got {err:?}"
    );
}

// ---- New tests exercising the trust-root architecture -----------------

#[test]
fn verify_with_empty_trust_roots_errors() {
    let (bytes, sig, _trust, signed_at) = sign_artifact(FIXTURE_SIGNED);
    let now = signed_at + ChronoDuration::minutes(30);
    let window = Duration::from_secs(3 * 3600);

    let err = verify_artifact(&bytes, &sig, &[], now, window, None).unwrap_err();
    assert!(matches!(err, VerifyError::NoTrustRoots));
}

#[test]
fn verify_rotation_with_two_keys_tries_each_in_order() {
    // Simulate a rotation grace window: old key is declared first, new
    // key is declared second. The signature was produced by the new key.
    // Verifier tries the old key (fails) then the new key (succeeds).
    let old_key = fresh_signing_key();
    let new_key = fresh_signing_key();
    let trust_roots = vec![trust_root_for(&old_key), trust_root_for(&new_key)];

    let value: serde_json::Value = serde_json::from_str(FIXTURE_SIGNED).unwrap();
    let signed_at: DateTime<Utc> = value["meta"]["signedAt"].as_str().unwrap().parse().unwrap();
    let canonical = canonicalize(&value.to_string()).unwrap();
    let sig = new_key.sign(canonical.as_bytes()).to_bytes();

    let now = signed_at + ChronoDuration::minutes(30);
    let window = Duration::from_secs(3 * 3600);

    let result = verify_artifact(canonical.as_bytes(), &sig, &trust_roots, now, window, None);
    assert!(
        result.is_ok(),
        "rotation-order list must accept the second key: {result:?}"
    );
}

#[test]
fn verify_rejects_when_only_unknown_algorithm_declared() {
    // Operator declares a trust root with a future algorithm this binary
    // doesn't know about. Verifier rejects with UnsupportedAlgorithm —
    // NOT BadSignature — so ops logs are actionable.
    let (bytes, sig, _trust, signed_at) = sign_artifact(FIXTURE_SIGNED);
    let future_only = vec![TrustedPubkey {
        algorithm: "dilithium3".to_string(),
        public: "somebase64value==".to_string(),
    }];
    let now = signed_at + ChronoDuration::minutes(30);
    let window = Duration::from_secs(3 * 3600);

    let err = verify_artifact(&bytes, &sig, &future_only, now, window, None).unwrap_err();
    match err {
        VerifyError::UnsupportedAlgorithm { algorithm } => {
            assert_eq!(algorithm, "dilithium3");
        }
        other => panic!("expected UnsupportedAlgorithm, got {other:?}"),
    }
}

#[test]
fn verify_skips_unknown_algorithm_when_known_also_present() {
    // Mixed declaration: an unknown-to-this-binary algorithm is listed
    // alongside the ed25519 key that actually signed. Verifier skips the
    // unknown entry, matches the known one, returns Ok. This is the
    // forward-compat path for a rolling upgrade where some operators
    // have a newer Nix declaration but an older verifier binary.
    let (bytes, sig, ed_trust, signed_at) = sign_artifact(FIXTURE_SIGNED);
    let mixed = vec![
        TrustedPubkey {
            algorithm: "p256".to_string(),
            public: "somebase64value==".to_string(),
        },
        ed_trust,
    ];
    let now = signed_at + ChronoDuration::minutes(30);
    let window = Duration::from_secs(3 * 3600);

    let result = verify_artifact(&bytes, &sig, &mixed, now, window, None);
    assert!(
        result.is_ok(),
        "mixed-algorithm list with one known key must verify: {result:?}"
    );
}

// ---- ECDSA P-256 (signature-algorithm agility) -------------------

/// P-256 curve order `n`, big-endian. Used to construct high-s twin
/// signatures for malleability rejection tests.
const P256_N_BE: [u8; 32] = [
    0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xBC, 0xE6, 0xFA, 0xAD, 0xA7, 0x17, 0x9E, 0x84, 0xF3, 0xB9, 0xCA, 0xC2, 0xFC, 0x63, 0x25, 0x51,
];

/// Compute `minuend - subtrahend` on 32-byte big-endian scalars
/// (used when minuend > subtrahend; no modular reduction).
fn be_sub_32(minuend: &[u8; 32], subtrahend: &[u8; 32]) -> [u8; 32] {
    let mut result = [0u8; 32];
    let mut borrow: i32 = 0;
    for i in (0..32).rev() {
        let v = minuend[i] as i32 - subtrahend[i] as i32 - borrow;
        if v < 0 {
            result[i] = (v + 256) as u8;
            borrow = 1;
        } else {
            result[i] = v as u8;
            borrow = 0;
        }
    }
    result
}

/// Sign `canonical_bytes` with a freshly-generated p256 key. Returns
/// (signature 64-byte R||S, trust root carrying 64-byte X||Y public).
fn sign_p256(canonical_bytes: &[u8]) -> ([u8; 64], TrustedPubkey) {
    use p256::ecdsa::signature::Signer;
    use p256::ecdsa::{Signature, SigningKey};

    let mut seed = [0u8; 32];
    OsRng.try_fill_bytes(&mut seed).expect("OS CSPRNG");
    let signing_key = SigningKey::from_slice(&seed).expect("derive p256 key from 32 bytes");
    let verifying_key = signing_key.verifying_key();

    let sig: Signature = signing_key.sign(canonical_bytes);
    // The p256 crate's signer does not guarantee low-s output. Normalize
    // here so the helper always returns a canonical (low-s) signature —
    // matches what any well-behaved production signer would do before
    // writing the detached `.sig` file.
    let sig = sig.normalize_s().unwrap_or(sig);
    let sig_bytes: [u8; 64] = sig.to_bytes().into();

    // Encode public key as 64-byte X||Y (no 0x04 tag) per CONTRACTS.md §II #1.
    let tagged = verifying_key.to_encoded_point(false);
    let tagged_bytes = tagged.as_bytes();
    assert_eq!(
        tagged_bytes.len(),
        65,
        "uncompressed SEC1 point is 65 bytes"
    );
    assert_eq!(tagged_bytes[0], 0x04, "SEC1 uncompressed tag");
    let public_bytes: &[u8] = &tagged_bytes[1..];
    let public_b64 = BASE64_STANDARD.encode(public_bytes);

    let trust = TrustedPubkey {
        algorithm: "ecdsa-p256".to_string(),
        public: public_b64,
    };
    (sig_bytes, trust)
}

#[test]
fn verify_p256_ok() {
    let value: serde_json::Value = serde_json::from_str(FIXTURE_SIGNED).unwrap();
    let signed_at: DateTime<Utc> = value["meta"]["signedAt"].as_str().unwrap().parse().unwrap();
    let canonical = canonicalize(&value.to_string()).unwrap();

    let (sig, trust) = sign_p256(canonical.as_bytes());
    let now = signed_at + ChronoDuration::minutes(30);
    let window = Duration::from_secs(3 * 3600);

    let result = verify_artifact(canonical.as_bytes(), &sig, &[trust], now, window, None);
    assert!(result.is_ok(), "verify_p256_ok: {result:?}");
}

#[test]
fn verify_p256_accepts_high_s() {
    // ECDSA signatures are malleable: both `(r, s)` and `(r, n-s)`
    // are valid for the same message. Earlier strict-rejection
    // posture was Bitcoin-style defence-in-depth, but TPM2_Sign does
    // not normalise s on its own (~50% of TPM-emitted sigs are
    // high-s). The verifier now normalises both forms to the
    // canonical low-s representation before ECDSA-verifying. These
    // artifacts are signed by a single TPM, fetched once, verified
    // once, never re-emitted — the malleability protection isn't
    // load-bearing for our wire. Caught on lab when a CI run
    // produced a high-s sig after the previous ones happened to be
    // low-s by chance.
    let value: serde_json::Value = serde_json::from_str(FIXTURE_SIGNED).unwrap();
    let signed_at: DateTime<Utc> = value["meta"]["signedAt"].as_str().unwrap().parse().unwrap();
    let canonical = canonicalize(&value.to_string()).unwrap();

    let (sig, trust) = sign_p256(canonical.as_bytes());

    let mut malleable = sig;
    let s_be: [u8; 32] = sig[32..64].try_into().unwrap();
    let s_high = be_sub_32(&P256_N_BE, &s_be);
    malleable[32..64].copy_from_slice(&s_high);

    let now = signed_at + ChronoDuration::minutes(30);
    let window = Duration::from_secs(3 * 3600);

    let result = verify_artifact(
        canonical.as_bytes(),
        &malleable,
        &[trust],
        now,
        window,
        None,
    );
    assert!(
        result.is_ok(),
        "high-s must verify (normalised internally): got {result:?}"
    );
}

#[test]
fn verify_rotation_cross_algorithm() {
    // Cross-algorithm rotation grace: current = p256, previous = ed25519
    // (or vice versa). p256-signed artifact verifies via the first
    // matching entry in the list; ed25519 doesn't interfere.
    let value: serde_json::Value = serde_json::from_str(FIXTURE_SIGNED).unwrap();
    let signed_at: DateTime<Utc> = value["meta"]["signedAt"].as_str().unwrap().parse().unwrap();
    let canonical = canonicalize(&value.to_string()).unwrap();

    let (p256_sig, p256_trust) = sign_p256(canonical.as_bytes());

    // An unrelated ed25519 "previous" trust root.
    let previous_ed25519_key = fresh_signing_key();
    let ed_trust = trust_root_for(&previous_ed25519_key);

    let trusted = vec![p256_trust, ed_trust];
    let now = signed_at + ChronoDuration::minutes(30);
    let window = Duration::from_secs(3 * 3600);

    let result = verify_artifact(canonical.as_bytes(), &p256_sig, &trusted, now, window, None);
    assert!(
        result.is_ok(),
        "p256 current + ed25519 previous — p256 sig must verify via first entry: {result:?}"
    );
}

#[test]
fn verify_rejects_malformed_pubkey_encoding() {
    let (bytes, sig, _trust, signed_at) = sign_artifact(FIXTURE_SIGNED);
    let bad_key = vec![TrustedPubkey {
        algorithm: "ed25519".to_string(),
        public: "!!! not base64 !!!".to_string(),
    }];
    let now = signed_at + ChronoDuration::minutes(30);
    let window = Duration::from_secs(3 * 3600);

    // Malformed key doesn't verify → fall through to BadSignature. Operators
    // see "no key verified" rather than a per-key decode error. If the
    // opposite behavior is desired (surface decode errors loudly), a future
    // PR can change verify_ed25519 to propagate BadPubkeyEncoding; this test
    // pins the current "skip on decode failure" behavior so the change is
    // deliberate.
    let err = verify_artifact(&bytes, &sig, &bad_key, now, window, None).unwrap_err();
    assert!(matches!(err, VerifyError::BadSignature));
}

// ---- reject_before compromise switch (CONTRACTS.md §II #1, trust-root §7.2) -----

#[test]
fn rejects_artifact_older_than_reject_before() {
    let (bytes, sig, trust, signed_at) = sign_artifact(FIXTURE_SIGNED);
    let freshness = Duration::from_secs(86_400);
    let reject_before = signed_at + ChronoDuration::seconds(60);
    let now = signed_at + ChronoDuration::seconds(10);

    let err = verify_artifact(
        &bytes,
        &sig,
        std::slice::from_ref(&trust),
        now,
        freshness,
        Some(reject_before),
    )
    .unwrap_err();

    match err {
        VerifyError::RejectedBeforeTimestamp {
            signed_at: got_signed_at,
            reject_before: got_rb,
        } => {
            assert_eq!(got_signed_at, signed_at);
            assert_eq!(got_rb, reject_before);
        }
        other => panic!("expected RejectedBeforeTimestamp, got: {other:?}"),
    }
}

#[test]
fn accepts_artifact_signed_at_after_reject_before() {
    let (bytes, sig, trust, signed_at) = sign_artifact(FIXTURE_SIGNED);
    let freshness = Duration::from_secs(86_400);
    // reject_before older than the artifact — the artifact stays valid.
    let reject_before = signed_at - ChronoDuration::seconds(60);
    let now = signed_at + ChronoDuration::seconds(10);

    let fleet = verify_artifact(
        &bytes,
        &sig,
        std::slice::from_ref(&trust),
        now,
        freshness,
        Some(reject_before),
    )
    .expect("accepts artifact signed after rejectBefore");
    assert_eq!(fleet.schema_version, 1);
}

#[test]
fn reject_before_none_disables_the_gate() {
    let (bytes, sig, trust, signed_at) = sign_artifact(FIXTURE_SIGNED);
    let freshness = Duration::from_secs(86_400);
    let now = signed_at + ChronoDuration::minutes(30);

    let _fleet = verify_artifact(
        &bytes,
        &sig,
        std::slice::from_ref(&trust),
        now,
        freshness,
        None,
    )
    .expect("None means gate disabled");
}

/// Strict `<` comparison: an artifact signed exactly at `reject_before`
/// is accepted. Mirrors the precedent set by `verify_at_exact_window_boundary_is_fresh`
/// on the freshness window. Locks the semantic so any future flip to
/// non-strict `<=` surfaces as a test failure.
#[test]
fn reject_before_exact_equal_is_accepted() {
    let (bytes, sig, trust, signed_at) = sign_artifact(FIXTURE_SIGNED);
    let freshness = Duration::from_secs(86_400);
    let reject_before = signed_at;
    let now = signed_at + ChronoDuration::seconds(10);

    let _fleet = verify_artifact(
        &bytes,
        &sig,
        std::slice::from_ref(&trust),
        now,
        freshness,
        Some(reject_before),
    )
    .expect("signed_at == reject_before must be accepted under strict < semantic");
}

// =================================================================
// verify_revocations — signed `revocations.json` sidecar artifact
// =================================================================

const FIXTURE_REVOCATIONS: &str = r#"{
  "meta": {
    "schemaVersion": 1,
    "signedAt": "2026-04-28T10:00:00Z",
    "ciCommit": "abc12345"
  },
  "revocations": [
    {
      "hostname": "old-laptop",
      "notBefore": "2026-04-26T00:00:00Z",
      "reason": "decommissioned",
      "revokedBy": "operator"
    }
  ],
  "schemaVersion": 1
}"#;

fn sign_revocations(json: &str) -> (Vec<u8>, [u8; 64], TrustedPubkey, DateTime<Utc>) {
    // Same shape as sign_artifact but reused here for clarity. Both
    // artifacts share the same trust class + canonicalisation; the
    // helper is identical except for caller-side type expectations.
    sign_artifact(json)
}

#[test]
fn verify_revocations_ok_returns_revocations() {
    let (bytes, sig, trust, signed_at) = sign_revocations(FIXTURE_REVOCATIONS);
    let now = signed_at + ChronoDuration::minutes(30);
    let window = Duration::from_secs(3 * 3600);

    let result = verify_revocations(
        &bytes,
        &sig,
        std::slice::from_ref(&trust),
        now,
        window,
        None,
    );
    let revs = result.expect("verify_revocations_ok");
    assert_eq!(revs.schema_version, 1);
    assert_eq!(revs.revocations.len(), 1);
    assert_eq!(revs.revocations[0].hostname, "old-laptop");
}

#[test]
fn verify_revocations_rejects_tampered_signature() {
    let (bytes, mut sig, trust, signed_at) = sign_revocations(FIXTURE_REVOCATIONS);
    sig[0] ^= 0xFF;
    let now = signed_at + ChronoDuration::minutes(30);
    let window = Duration::from_secs(3 * 3600);

    let err = verify_revocations(
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
fn verify_revocations_rejects_stale() {
    let (bytes, sig, trust, signed_at) = sign_revocations(FIXTURE_REVOCATIONS);
    let now = signed_at + ChronoDuration::hours(4);
    let window = Duration::from_secs(3 * 3600);

    let err = verify_revocations(
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
fn verify_revocations_rejects_unsigned() {
    // Body has meta.signedAt = null. The signature verifies, but
    // finish_revocations_verification rejects with NotSigned.
    let signing_key = fresh_signing_key();
    let trust = trust_root_for(&signing_key);
    let json = r#"{
      "meta": { "schemaVersion": 1, "signedAt": null, "ciCommit": "abc12345" },
      "revocations": [],
      "schemaVersion": 1
    }"#;
    let reserialized = serde_json::to_string(&serde_json::from_str::<serde_json::Value>(json).unwrap()).unwrap();
    let canonical = canonicalize(&reserialized).expect("canonicalize");
    let sig = signing_key.sign(canonical.as_bytes()).to_bytes();
    let err = verify_revocations(
        canonical.as_bytes(),
        &sig,
        std::slice::from_ref(&trust),
        Utc::now(),
        Duration::from_secs(3600),
        None,
    )
    .unwrap_err();
    assert!(matches!(err, VerifyError::NotSigned), "got {err:?}");
}

#[test]
fn verify_revocations_empty_list_is_valid() {
    // Steady state: no revocations on file. The artifact must still
    // verify so the CP can replay-into-empty without thinking there's
    // a problem.
    let json = r#"{
      "meta": {
        "schemaVersion": 1,
        "signedAt": "2026-04-28T10:00:00Z",
        "ciCommit": "abc12345"
      },
      "revocations": [],
      "schemaVersion": 1
    }"#;
    let (bytes, sig, trust, signed_at) = sign_revocations(json);
    let now = signed_at + ChronoDuration::minutes(5);
    let revs = verify_revocations(
        &bytes,
        &sig,
        std::slice::from_ref(&trust),
        now,
        Duration::from_secs(3600),
        None,
    )
    .expect("empty list verifies");
    assert!(revs.revocations.is_empty());
}

/// `RejectedBeforeTimestamp` wins over `Stale` when both conditions
/// hold. Makes the alert-class invariant explicit: operators seeing a
/// compromise-rejected artifact must not be misled into thinking the
/// artifact is merely expired.
#[test]
fn reject_before_takes_precedence_over_stale() {
    let (bytes, sig, trust, signed_at) = sign_artifact(FIXTURE_SIGNED);
    // Freshness = 60s but artifact is 600s old → stale.
    // reject_before is 300s after signing → also triggers.
    let window = Duration::from_secs(60);
    let reject_before = signed_at + ChronoDuration::seconds(300);
    let now = signed_at + ChronoDuration::seconds(600);

    let err = verify_artifact(
        &bytes,
        &sig,
        std::slice::from_ref(&trust),
        now,
        window,
        Some(reject_before),
    )
    .unwrap_err();

    assert!(
        matches!(err, VerifyError::RejectedBeforeTimestamp { .. }),
        "compromise switch must win over routine staleness, got {err:?}"
    );
}

// =================================================================
// verify_rollout_manifest + compute_rollout_id — RFC-0002 §4.4
// =================================================================

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

fn sign_manifest(json: &str) -> (Vec<u8>, [u8; 64], TrustedPubkey, DateTime<Utc>) {
    sign_artifact(json)
}

#[test]
fn verify_rollout_manifest_ok_returns_manifest() {
    let (bytes, sig, trust, signed_at) = sign_manifest(FIXTURE_MANIFEST);
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
    let m = result.expect("verify_rollout_manifest_ok");
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
    let (bytes, mut sig, trust, signed_at) = sign_manifest(FIXTURE_MANIFEST);
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
    let (bytes, sig, trust, signed_at) = sign_manifest(FIXTURE_MANIFEST);
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
fn compute_rollout_id_is_64_hex_chars() {
    let (bytes, sig, trust, signed_at) = sign_manifest(FIXTURE_MANIFEST);
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
    .expect("verify ok");

    let id = compute_rollout_id(&m).expect("compute_rollout_id");
    assert_eq!(id.len(), 64, "sha256 hex must be 64 chars: {id}");
    assert!(
        id.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "id must be hex lowercase only: {id}"
    );
}

#[test]
fn compute_rollout_id_stable_across_round_trip() {
    // Compute id from the verified manifest, then serialize → parse →
    // compute again. Bytes round-trip identically through JCS, so the
    // recomputed id must match.
    let (bytes, sig, trust, signed_at) = sign_manifest(FIXTURE_MANIFEST);
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
    .expect("verify ok");

    let id1 = compute_rollout_id(&m).unwrap();
    let raw = serde_json::to_string(&m).unwrap();
    let m2: nixfleet_proto::RolloutManifest = serde_json::from_str(&raw).unwrap();
    let id2 = compute_rollout_id(&m2).unwrap();

    assert_eq!(id1, id2, "id must survive serialize/parse round-trip");
}

#[test]
fn compute_rollout_id_changes_with_field_change() {
    // Sanity: any field change perturbs the id. Validates that the
    // hash actually covers the canonical surface.
    let (bytes, sig, trust, signed_at) = sign_manifest(FIXTURE_MANIFEST);
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
    .expect("verify ok");
    let id1 = compute_rollout_id(&m).unwrap();

    let mut m2 = m.clone();
    m2.host_set[0].target_closure =
        "9999999999999999999999999999999999999999-perturbed".to_string();
    let id2 = compute_rollout_id(&m2).unwrap();

    assert_ne!(id1, id2);
}

// =================================================================
// Sidecar coverage matrix — assert the gates `verify_artifact`
// already exercises are actually wired through the shared
// `verify_signed_sidecar<T>` generic for the two newer types
// (revocations + rollout manifest). The underlying pipeline is
// shared, but a per-sidecar test guards against a future variant
// adding a bypass that only its own wrapper would miss.
// =================================================================

#[test]
fn verify_revocations_rejects_malformed_json() {
    // Bytes that signature-verify but don't parse as Revocations.
    // Payload is well-formed JSON but missing the schema fields.
    let signing_key = fresh_signing_key();
    let trust = trust_root_for(&signing_key);
    let canonical = canonicalize(r#"{"not":"a-revocations"}"#).expect("canonicalize");
    let sig = signing_key.sign(canonical.as_bytes()).to_bytes();
    let err = verify_revocations(
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
fn verify_revocations_rejects_when_trust_roots_empty() {
    let (bytes, sig, _trust, signed_at) = sign_artifact(FIXTURE_REVOCATIONS);
    let now = signed_at + ChronoDuration::minutes(30);
    let err = verify_revocations(
        &bytes,
        &sig,
        &[],
        now,
        Duration::from_secs(3600),
        None,
    )
    .unwrap_err();
    assert!(
        matches!(err, VerifyError::NoTrustRoots),
        "empty trust roots → NoTrustRoots; got {err:?}"
    );
}

#[test]
fn verify_revocations_reject_before_rejects_pre_compromise() {
    // Compromise-kill-switch must apply to every signed sidecar, not
    // just `verify_artifact`. Same `<` semantic.
    let (bytes, sig, trust, signed_at) = sign_revocations(FIXTURE_REVOCATIONS);
    let now = signed_at + ChronoDuration::minutes(30);
    let reject_before = signed_at + ChronoDuration::seconds(1);
    let err = verify_revocations(
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
        "reject_before must apply to revocations; got {err:?}"
    );
}

#[test]
fn verify_revocations_reject_before_none_disables_gate() {
    let (bytes, sig, trust, signed_at) = sign_revocations(FIXTURE_REVOCATIONS);
    let now = signed_at + ChronoDuration::minutes(30);
    verify_revocations(
        &bytes,
        &sig,
        std::slice::from_ref(&trust),
        now,
        Duration::from_secs(3600),
        None,
    )
    .expect("None disables the gate, same as verify_artifact");
}

#[test]
fn verify_rollout_manifest_rejects_unsigned() {
    // signedAt = null → NotSigned. The rollout-manifest verifier
    // must enforce this — an unsigned manifest is the same trust
    // class as the rollout-rename attack RFC-0002 §4.4 closes.
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
    let (bytes, sig, _trust, signed_at) = sign_manifest(FIXTURE_MANIFEST);
    let now = signed_at + ChronoDuration::minutes(30);
    let err = verify_rollout_manifest(
        &bytes,
        &sig,
        &[],
        now,
        Duration::from_secs(3600),
        None,
    )
    .unwrap_err();
    assert!(
        matches!(err, VerifyError::NoTrustRoots),
        "empty trust roots → NoTrustRoots; got {err:?}"
    );
}

#[test]
fn verify_rollout_manifest_reject_before_rejects_pre_compromise() {
    let (bytes, sig, trust, signed_at) = sign_manifest(FIXTURE_MANIFEST);
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
