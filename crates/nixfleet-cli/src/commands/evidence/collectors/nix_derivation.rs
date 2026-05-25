//! Intent-side adapter: surfaces the host's declared closure hash from
//! `fleet.resolved.json`. Pure data extraction; no subprocess. The
//! auditor compares this against any other adapter that reports the
//! running store path (e.g., a future `nix-store` runtime adapter).

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::super::collector::{CollectorContext, CollectorEntry, EvidenceCollector};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct NixDerivationData {
    /// Operator-declared expected closure hash for this host. `None`
    /// when `fleet.resolved.json` did not pin one at signing time
    /// (e.g., pre-build evaluation).
    closure_hash: Option<String>,
    /// Channel the host was rolled out on. Auditor cross-reference
    /// for channel-derived assertions in `evidence.json.controls`.
    channel: String,
    /// System triple (e.g., `x86_64-linux`). Closures pin per-platform
    /// store paths; the auditor needs this when reconciling.
    system: String,
}

pub struct NixDerivationCollector;

impl EvidenceCollector for NixDerivationCollector {
    fn id(&self) -> &'static str {
        "nix-derivation"
    }

    fn collect(&self, ctx: &CollectorContext<'_>) -> Result<CollectorEntry> {
        let data = NixDerivationData {
            closure_hash: ctx.host.closure_hash.clone(),
            channel: ctx.host.channel.clone(),
            system: ctx.host.platform.clone(),
        };
        Ok(CollectorEntry {
            collector_id: self.id().to_string(),
            data: serde_json::to_value(data)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nixfleet_proto::fleet_resolved::Host;

    fn host_fixture(closure_hash: Option<&str>, channel: &str) -> Host {
        Host {
            platform: "x86_64-linux".into(),
            tags: vec![],
            channel: channel.into(),
            closure_hash: closure_hash.map(str::to_owned),
            pubkey: None,
            pin: None,
        }
    }

    #[test]
    fn nix_derivation_surfaces_closure_hash_verbatim() {
        let host = host_fixture(Some("sha256-abc123"), "stable");
        let ctx = CollectorContext {
            hostname: "h1",
            host: &host,
            fetched: None,
        };
        let entry = NixDerivationCollector.collect(&ctx).unwrap();
        assert_eq!(entry.collector_id, "nix-derivation");
        let parsed: NixDerivationData = serde_json::from_value(entry.data).unwrap();
        assert_eq!(parsed.closure_hash.as_deref(), Some("sha256-abc123"));
        assert_eq!(parsed.channel, "stable");
        assert_eq!(parsed.system, "x86_64-linux");
    }

    #[test]
    fn nix_derivation_handles_absent_closure_hash() {
        let host = host_fixture(None, "edge");
        let ctx = CollectorContext {
            hostname: "h2",
            host: &host,
            fetched: None,
        };
        let entry = NixDerivationCollector.collect(&ctx).unwrap();
        let parsed: NixDerivationData = serde_json::from_value(entry.data).unwrap();
        assert!(parsed.closure_hash.is_none());
        assert_eq!(parsed.channel, "edge");
    }
}
