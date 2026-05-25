//! Hardware-introspection adapter: surfaces the host's `facter.json`
//! (produced by `numtide/nixos-facter` running host-side as a
//! probe).
//!
//! GPL v3 hygiene: this adapter consumes the JSON output of facter as
//! bytes. The facter binary itself runs on the host as a subprocess
//! per the standard probe deployment, never linked or vendored into
//! nixfleet code.
//!
//! Best-effort: hosts without facter deployed produce
//! `available: false` here. The fleet-evidence record carries the
//! absence verbatim; the auditor can tell which hosts had hardware
//! introspection vs which did not.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::super::collector::{CollectorContext, CollectorEntry, EvidenceCollector};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct FacterData {
    /// True when a parseable `facter.json` was fetched. False when
    /// the host did not ship one, or when the JSON failed to parse.
    available: bool,
    /// Parsed facter JSON, verbatim. Renderers downstream slice
    /// what they need (hardware vendor, TPM presence, Secure Boot
    /// state, kernel info).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    raw: Option<serde_json::Value>,
    /// Populated when bytes were fetched but did not parse as JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parse_error: Option<String>,
}

pub struct FacterCollector;

impl EvidenceCollector for FacterCollector {
    fn id(&self) -> &'static str {
        "facter"
    }

    fn collect(&self, ctx: &CollectorContext<'_>) -> Result<CollectorEntry> {
        let bytes = ctx.fetched.and_then(|f| f.facter_json.as_ref());
        let data = match bytes {
            None => FacterData {
                available: false,
                raw: None,
                parse_error: None,
            },
            Some(b) => match serde_json::from_slice::<serde_json::Value>(b) {
                Ok(v) => FacterData {
                    available: true,
                    raw: Some(v),
                    parse_error: None,
                },
                Err(e) => FacterData {
                    available: false,
                    raw: None,
                    parse_error: Some(e.to_string()),
                },
            },
        };
        Ok(CollectorEntry {
            collector_id: self.id().to_string(),
            data: serde_json::to_value(data)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::fetch_ssh::FetchedHost;
    use super::*;
    use nixfleet_proto::fleet_resolved::Host;

    fn host_fixture() -> Host {
        Host {
            platform: "x86_64-linux".into(),
            tags: vec![],
            channel: "stable".into(),
            closure_hash: None,
            pubkey: None,
            pin: None,
        }
    }

    fn fetched_with_facter(facter_json: Option<&[u8]>) -> FetchedHost {
        FetchedHost {
            hostname: "h1".into(),
            source: "ssh",
            ok: true,
            error: None,
            evidence_json: Some(b"{}".to_vec()),
            signature: Some(b"sig".to_vec()),
            host_pubkey: Some(b"pub".to_vec()),
            facter_json: facter_json.map(|b| b.to_vec()),
        }
    }

    #[test]
    fn facter_records_unavailable_when_no_bytes_fetched() {
        let host = host_fixture();
        let fh = fetched_with_facter(None);
        let ctx = CollectorContext {
            hostname: "h1",
            host: &host,
            fetched: Some(&fh),
        };
        let entry = FacterCollector.collect(&ctx).unwrap();
        let parsed: FacterData = serde_json::from_value(entry.data).unwrap();
        assert!(!parsed.available);
        assert!(parsed.raw.is_none());
        assert!(parsed.parse_error.is_none());
    }

    #[test]
    fn facter_surfaces_valid_json_verbatim() {
        let host = host_fixture();
        let body = br#"{"system":"x86_64-linux","tpm":{"present":true}}"#;
        let fh = fetched_with_facter(Some(body));
        let ctx = CollectorContext {
            hostname: "h1",
            host: &host,
            fetched: Some(&fh),
        };
        let entry = FacterCollector.collect(&ctx).unwrap();
        let parsed: FacterData = serde_json::from_value(entry.data).unwrap();
        assert!(parsed.available);
        let raw = parsed.raw.expect("raw");
        assert_eq!(raw["system"], "x86_64-linux");
        assert_eq!(raw["tpm"]["present"], true);
    }

    #[test]
    fn facter_records_parse_error_on_malformed_json() {
        let host = host_fixture();
        let fh = fetched_with_facter(Some(b"not json"));
        let ctx = CollectorContext {
            hostname: "h1",
            host: &host,
            fetched: Some(&fh),
        };
        let entry = FacterCollector.collect(&ctx).unwrap();
        let parsed: FacterData = serde_json::from_value(entry.data).unwrap();
        assert!(!parsed.available);
        assert!(parsed.raw.is_none());
        assert!(parsed.parse_error.is_some());
    }
}
