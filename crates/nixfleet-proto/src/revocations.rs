//! `revocations.json` — signed agent-cert revocation list.
//!
//! Sidecar artifact alongside `fleet.resolved.json`. Closes
//! of `docs/roadmap/0002-v0.2-completeness-gaps.md`: without it,
//! `cert_revocations` is the only piece of CP-resident state where
//! loss-on-rebuild is a security regression rather than an
//! operational rough edge.
//!
//! Trust class: signed with the same `ciReleaseKey` that signs
//! `fleet.resolved.json`. One signing key surface to rotate, same
//! verification path on the CP. The threat model is identical
//! both artifacts are the kind of data that, if forged, lets an
//! attacker steer the fleet. They live in the same trust bucket.
//!
//! Recovery semantics: on every successful poll, the CP replays
//! the verified list into the in-memory + on-disk
//! `cert_revocations` table. `revoke_cert` is already upsert, so
//! replays are idempotent. If the artifact is unreachable or
//! verification fails, the CP retains its last-known-good
//! revocation set — same posture as `fleet.resolved` itself.
//!
//! Producer-side: `nixfleet-release` signs this artifact when the
//! consumer flake exposes a non-empty `revocations` attribute.
//! Operator workflow shifts from CLI-on-CP to git commit + CI
//! sign + push — strengthening the inversion-of-trust property
//! from ARCHITECTURE.md §1.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::fleet_resolved::Meta;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Revocations {
    pub schema_version: u32,
    /// Revocation entries. Each one applies to any agent cert
    /// for `hostname` whose `notBefore` is older than the entry's
    /// `notBefore`. Empty list is valid — represents "no
    /// revocations on file" and is the steady state.
    pub revocations: Vec<RevocationEntry>,
    pub meta: Meta,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RevocationEntry {
    pub hostname: String,
    /// Any cert for `hostname` with `notBefore` strictly older
    /// than this is rejected at mTLS handshake time. Stored as
    /// RFC3339; mirrored from the `cert_revocations.not_before`
    /// column.
    pub not_before: DateTime<Utc>,
    /// Free-form operator note (decommissioned, compromised,
    /// rotated, etc.). Surfaces in audit logs.
    #[serde(default)]
    pub reason: Option<String>,
    /// Operator who declared the revocation. Free-form;
    /// surfaces in audit logs.
    #[serde(default)]
    pub revoked_by: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta_v1() -> Meta {
        Meta {
            schema_version: 1,
            signed_at: Some("2026-04-28T10:00:00Z".parse().unwrap()),
            ci_commit: Some("abc12345".into()),
            signature_algorithm: None,
        }
    }

    #[test]
    fn empty_revocations_round_trip() {
        let r = Revocations {
            schema_version: 1,
            revocations: vec![],
            meta: meta_v1(),
        };
        let s = serde_json::to_string(&r).unwrap();
        let parsed: Revocations = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, r);
    }

    #[test]
    fn revocation_entry_round_trip() {
        let r = Revocations {
            schema_version: 1,
            revocations: vec![RevocationEntry {
                hostname: "old-laptop".into(),
                not_before: "2026-04-26T00:00:00Z".parse().unwrap(),
                reason: Some("decommissioned".into()),
                revoked_by: Some("operator".into()),
            }],
            meta: meta_v1(),
        };
        let s = serde_json::to_string(&r).unwrap();
        let parsed: Revocations = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, r);
    }

    #[test]
    fn revocation_entry_optional_fields_default_to_none() {
        // Fleet-flake-side input may omit reason/revokedBy. Must
        // round-trip cleanly without those fields.
        let json = r#"{
            "hostname": "old-laptop",
            "notBefore": "2026-04-26T00:00:00Z"
        }"#;
        let entry: RevocationEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.hostname, "old-laptop");
        assert!(entry.reason.is_none());
        assert!(entry.revoked_by.is_none());
    }
}
