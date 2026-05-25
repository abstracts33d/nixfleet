//! M1.1: fleet host enumeration.
//!
//! Pure read-only helper. Returns hosts in canonical ASCII-ascending
//! order over the existing `nixfleet_proto::fleet_resolved::Host` type
//! (no bespoke struct — that would compound for no shared benefit; see
//! RFC-0004 §2 lift-to-general). Pubkey absence is preserved as `None`
//! on the host record; the gate (pubkey-required for fetch + verify)
//! belongs to M1.2 / M1.3.
//!
//! M1.2 swaps the caller's raw-JSON parse for
//! `nixfleet_reconciler::verify::verify_artifact` returning
//! `Verified<FleetResolved>`. This function takes `&FleetResolved`
//! directly so the wrap-or-not decision lives at the call site.

use nixfleet_proto::fleet_resolved::{FleetResolved, Host};

/// Canonical-order iteration of fleet hosts. Returns `(hostname, &Host)`
/// pairs sorted by ASCII-ascending hostname. Borrows from `fleet`;
/// callers needing owned data clone what they read.
pub fn enumerate(fleet: &FleetResolved) -> Vec<(&str, &Host)> {
    let mut out: Vec<(&str, &Host)> = fleet
        .hosts
        .iter()
        .map(|(name, host)| (name.as_str(), host))
        .collect();
    out.sort_by_key(|(name, _)| *name);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nixfleet_proto::fleet_resolved::Meta;
    use std::collections::HashMap;

    fn host(channel: &str, pubkey: Option<&str>) -> Host {
        Host {
            platform: "x86_64-linux".into(),
            tags: vec![],
            channel: channel.into(),
            closure_hash: None,
            pubkey: pubkey.map(str::to_owned),
            pin: None,
        }
    }

    fn fleet_with_hosts(pairs: Vec<(&str, Host)>) -> FleetResolved {
        FleetResolved {
            schema_version: 1,
            hosts: pairs.into_iter().map(|(n, h)| (n.to_string(), h)).collect(),
            channels: HashMap::new(),
            rollout_policies: HashMap::new(),
            waves: HashMap::new(),
            edges: Vec::new(),
            channel_edges: Vec::new(),
            disruption_budgets: Vec::new(),
            meta: Meta {
                schema_version: 1,
                signed_at: None,
                ci_commit: None,
                signature_algorithm: None,
            },
        }
    }

    #[test]
    fn enumerate_returns_hosts_in_ascii_ascending_order() {
        // HashMap iteration is randomized; the canonical sort must
        // produce the same answer regardless of insertion / iteration
        // order. Three hosts with a non-alphabetical insertion order
        // exercises that.
        let fleet = fleet_with_hosts(vec![
            ("zeta", host("stable", None)),
            ("alpha", host("stable", None)),
            ("mu", host("stable", None)),
        ]);
        let names: Vec<&str> = enumerate(&fleet).iter().map(|(n, _)| *n).collect();
        assert_eq!(names, vec!["alpha", "mu", "zeta"]);
    }

    #[test]
    fn enumerate_preserves_pubkey_presence_per_host() {
        let fleet = fleet_with_hosts(vec![
            ("a", host("stable", Some("AAAAC3NzaC1lZDI1NTE5_example"))),
            ("b", host("stable", None)),
        ]);
        let hosts = enumerate(&fleet);
        assert_eq!(
            hosts[0].1.pubkey.as_deref(),
            Some("AAAAC3NzaC1lZDI1NTE5_example")
        );
        assert!(hosts[1].1.pubkey.is_none());
    }

    #[test]
    fn enumerate_empty_hosts_yields_empty_vec() {
        let fleet = fleet_with_hosts(vec![]);
        assert!(enumerate(&fleet).is_empty());
    }
}
