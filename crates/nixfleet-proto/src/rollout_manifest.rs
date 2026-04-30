//! `releases/rollouts/<rolloutId>.json` — signed per-channel rollout
//! manifest.
//!
//! Produced by CI alongside `fleet.resolved.json`: for every channel
//! `c`, CI projects `fleet.resolved` into a `RolloutManifest` (host
//! membership + wave layout + target closure for hosts on `c`),
//! canonicalizes via JCS, and signs with the same `ciReleaseKey` used
//! for `fleet.resolved.json` and `revocations.json`.
//!
//! Identifier semantics: the manifest's identity is the SHA-256 hex of
//! its canonical bytes (computed by
//! `nixfleet-reconciler::sidecar::compute_rollout_id`). The CP serves
//! this identifier as `EvaluatedTarget.rollout_id`; agents fetch
//! `GET /v1/rollouts/<rolloutId>`, verify the signature, recompute
//! the hash, and assert it matches before consuming any other field
//! of the dispatch target. The human-readable `<channel>@<short-ci-commit>`
//! label lives inside the manifest as `display_name` for trace and
//! CLI display only — it is not the primary key.
//!
//! Plan vs state: `fleet.resolved.json` is the desired-state snapshot
//! at CI time and rolls forward as new commits land. A
//! `RolloutManifest` is the frozen plan for one rollout's lifetime.
//! The `fleet_resolved_hash` field anchors the manifest to the exact
//! signed snapshot it was projected from — closes a mix-and-match
//! attack where two snapshots at the same channel ref could otherwise
//! be paired with each other's manifests.
//!
//! Trust class: same `ciReleaseKey` as `fleet.resolved.json` and
//! `revocations.json`. Same trust root, same rotation surface, same
//! verification path.

use serde::{Deserialize, Serialize};

use crate::fleet_resolved::{HealthGate, Meta};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RolloutManifest {
    pub schema_version: u32,

    /// Human-readable annotation of the form `<channel>@<short-ci-commit>`.
    /// NOT the manifest identifier — the rolloutId is a content hash
    /// over the canonical bytes of this struct (see crate docs).
    /// Exposed for trace / CLI / log readability; safe to break-glass
    /// edit without affecting integrity (any edit changes the content
    /// hash so any tampered manifest is rejected on hash recompute).
    pub display_name: String,

    pub channel: String,

    /// Git ref the channel resolved to at CI time. Bound to the
    /// channel's `target_closure` via the `fleet.resolved` projection.
    pub channel_ref: String,

    /// Target closure hash this rollout converges hosts onto.
    /// Identical to `fleet_resolved.hosts[hostname].closureHash` for
    /// every host in `host_set` — CI guarantees the invariant by
    /// construction (the manifest is a projection of `fleet.resolved`).
    pub target_closure: String,

    /// SHA-256 (hex, lowercase) of the canonical bytes of the
    /// `fleet.resolved.json` from which this manifest was projected.
    /// Cryptographic anchor: the manifest belongs to one specific
    /// signed snapshot, so an attacker can't mix-and-match a manifest
    /// from snapshot X with the resolved.json from snapshot Y.
    pub fleet_resolved_hash: String,

    /// Hosts in this rollout, paired with their wave assignment.
    /// MUST be sorted by `hostname` ascending for canonical-byte
    /// stability — JCS sorts object keys but not array elements,
    /// so the producer's emission order is the canonical order.
    /// Verifiers should re-assert the sort at parse time.
    pub host_set: Vec<HostWave>,

    pub health_gate: HealthGate,

    /// Mirrored from `fleet.resolved.channels[channel].compliance.frameworks`
    /// at projection time. Lets agents apply the same compliance posture
    /// they would have inferred from `fleet.resolved` directly, without
    /// fetching the full snapshot.
    pub compliance_frameworks: Vec<String>,

    pub meta: Meta,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct HostWave {
    pub hostname: String,
    /// 0-based index in `fleet.resolved.waves[channel]` at projection
    /// time. Frozen for the manifest's lifetime; new CI commits that
    /// reshape `waves[channel]` produce a different manifest with a
    /// different `rolloutId`.
    pub wave_index: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet_resolved::Meta;
    use nixfleet_canonicalize::canonicalize;

    fn meta_v1() -> Meta {
        Meta {
            schema_version: 1,
            signed_at: Some("2026-04-30T12:00:00Z".parse().unwrap()),
            ci_commit: Some("def45678".into()),
            signature_algorithm: Some("ed25519".into()),
        }
    }

    fn sample_manifest() -> RolloutManifest {
        RolloutManifest {
            schema_version: 1,
            display_name: "stable@def4567".into(),
            channel: "stable".into(),
            channel_ref: "def4567abc123def4567abc123def4567abc123d".into(),
            target_closure: "0000000000000000000000000000000000000000-test-system".into(),
            fleet_resolved_hash:
                "1111111111111111111111111111111111111111111111111111111111111111".into(),
            host_set: vec![
                HostWave {
                    hostname: "agent-01".into(),
                    wave_index: 0,
                },
                HostWave {
                    hostname: "agent-02".into(),
                    wave_index: 1,
                },
            ],
            health_gate: HealthGate::default(),
            compliance_frameworks: vec!["anssi-bp028".into()],
            meta: meta_v1(),
        }
    }

    #[test]
    fn manifest_round_trip() {
        let m = sample_manifest();
        let s = serde_json::to_string(&m).unwrap();
        let parsed: RolloutManifest = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, m);
    }

    #[test]
    fn manifest_canonical_bytes_stable_across_round_trip() {
        // Serialize → canonicalize → parse → re-serialize → re-canonicalize.
        // JCS canonical bytes must match byte-for-byte; this is the
        // load-bearing property for `rolloutId = sha256(canonical(m))`.
        let m = sample_manifest();
        let raw1 = serde_json::to_string(&m).unwrap();
        let canon1 = canonicalize(&raw1).unwrap();

        let parsed: RolloutManifest = serde_json::from_str(&canon1).unwrap();
        let raw2 = serde_json::to_string(&parsed).unwrap();
        let canon2 = canonicalize(&raw2).unwrap();

        assert_eq!(canon1, canon2);
    }

    #[test]
    fn manifest_host_set_order_changes_canonical_bytes() {
        // JCS sorts object keys but not array elements. Two manifests
        // with the same logical host_set but different element order
        // canonicalize differently — proves the producer must emit a
        // sorted host_set for the rolloutId to be stable.
        let mut m1 = sample_manifest();
        let mut m2 = sample_manifest();
        m2.host_set.reverse();

        let canon1 = canonicalize(&serde_json::to_string(&m1).unwrap()).unwrap();
        let canon2 = canonicalize(&serde_json::to_string(&m2).unwrap()).unwrap();

        assert_ne!(
            canon1, canon2,
            "host_set order must affect canonical bytes (CI must emit sorted)"
        );

        // And re-sorting m2's host_set restores byte-equality.
        m2.host_set.sort_by(|a, b| a.hostname.cmp(&b.hostname));
        let canon2_resorted = canonicalize(&serde_json::to_string(&m2).unwrap()).unwrap();
        assert_eq!(canon1, canon2_resorted);

        // Touch m1 to silence "doesn't need to be mut" — kept mut for
        // symmetry with m2 in the assertion above.
        let _ = &mut m1;
    }

    #[test]
    fn fleet_resolved_hash_change_changes_canonical_bytes() {
        // Sanity: the anchor field is part of the canonical surface,
        // so two manifests projected from different fleet.resolved
        // snapshots produce different rolloutIds.
        let m1 = sample_manifest();
        let mut m2 = sample_manifest();
        m2.fleet_resolved_hash =
            "2222222222222222222222222222222222222222222222222222222222222222".into();

        let canon1 = canonicalize(&serde_json::to_string(&m1).unwrap()).unwrap();
        let canon2 = canonicalize(&serde_json::to_string(&m2).unwrap()).unwrap();

        assert_ne!(canon1, canon2);
    }

    #[test]
    fn host_wave_round_trip() {
        let h = HostWave {
            hostname: "agent-03".into(),
            wave_index: 2,
        };
        let s = serde_json::to_string(&h).unwrap();
        let parsed: HostWave = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, h);
        // wire shape: camelCase
        assert!(s.contains("\"waveIndex\":2"));
    }
}
