//! Runtime-state adapter: surfaces the host's `osquery-evidence.json`
//! (produced by the `compliance.evidence.osquery` module in
//! nixfleet-compliance, which runs `osqueryi -A schedule` against a
//! curated query pack at activation).
//!
//! License hygiene: this adapter consumes the JSON output of osquery
//! as bytes. The osquery binary itself runs on the host as a
//! subprocess per the standard producer deployment, never linked or
//! vendored into nixfleet code. osquery is Apache 2.0 / GPL v2
//! dual-licensed; subprocess invocation keeps the choice with the
//! operator.
//!
//! Best-effort: hosts without the producer enabled emit
//! `available: false` here. The fleet-evidence record carries the
//! absence verbatim; the auditor can tell which hosts have a local
//! runtime-state snapshot vs which rely solely on the fleet-dm
//! server-side ledger.
//!
//! Trust posture: `osquery-evidence.json` is best-effort enrichment,
//! NOT part of the per-host signed chain (`evidence.json` +
//! `evidence.json.sig` covers the typed-control output only). Auditors
//! who care about runtime-telemetry integrity should additionally
//! consume the fleet-dm server-side ledger (osquery's own enrollment +
//! TLS chain) — that's the system of record for runtime telemetry on
//! hosts enrolled there.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::super::collector::{CollectorContext, CollectorEntry, EvidenceCollector};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct OsqueryData {
    /// True when a parseable `osquery-evidence.json` was fetched.
    /// False when the host did not ship one, or when the JSON failed
    /// to parse.
    available: bool,
    /// Parsed osquery JSON, verbatim. The producer writes the output
    /// of `osqueryi --json -A schedule`, which is a JSON array of
    /// result rows (each annotated with `name`, `hostIdentifier`, and
    /// the per-query columns). Renderers downstream slice what they
    /// need (active services, kernel modules, interactive users, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    raw: Option<serde_json::Value>,
    /// Populated when bytes were fetched but did not parse as JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parse_error: Option<String>,
}

pub struct OsqueryCollector;

impl EvidenceCollector for OsqueryCollector {
    fn id(&self) -> &'static str {
        "osquery"
    }

    fn collect(&self, ctx: &CollectorContext<'_>) -> Result<CollectorEntry> {
        let bytes = ctx.fetched.and_then(|f| f.osquery_evidence_json.as_ref());
        let data = match bytes {
            None => OsqueryData {
                available: false,
                raw: None,
                parse_error: None,
            },
            Some(b) => match serde_json::from_slice::<serde_json::Value>(b) {
                Ok(v) => OsqueryData {
                    available: true,
                    raw: Some(v),
                    parse_error: None,
                },
                Err(e) => OsqueryData {
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

    fn fetched_with_osquery(osquery_evidence_json: Option<&[u8]>) -> FetchedHost {
        FetchedHost {
            hostname: "h1".into(),
            source: "ssh",
            ok: true,
            error: None,
            evidence_json: Some(b"{}".to_vec()),
            signature: Some(b"sig".to_vec()),
            host_pubkey: Some(b"pub".to_vec()),
            facter_json: None,
            osquery_evidence_json: osquery_evidence_json.map(|b| b.to_vec()),
        }
    }

    #[test]
    fn osquery_records_unavailable_when_no_bytes_fetched() {
        let host = host_fixture();
        let fh = fetched_with_osquery(None);
        let ctx = CollectorContext {
            hostname: "h1",
            host: &host,
            fetched: Some(&fh),
        };
        let entry = OsqueryCollector.collect(&ctx).unwrap();
        assert_eq!(entry.collector_id, "osquery");
        let parsed: OsqueryData = serde_json::from_value(entry.data).unwrap();
        assert!(!parsed.available);
        assert!(parsed.raw.is_none());
        assert!(parsed.parse_error.is_none());
    }

    #[test]
    fn osquery_surfaces_valid_json_verbatim() {
        let host = host_fixture();
        // Shape mirrors the on-host producer output: array of result
        // rows, each annotated by osqueryi -A schedule.
        let body = br#"[
            {"name":"os_version","hostIdentifier":"h1","columns":{"name":"NixOS","version":"26.05"}},
            {"name":"kernel_info","hostIdentifier":"h1","columns":{"version":"6.18.19"}}
        ]"#;
        let fh = fetched_with_osquery(Some(body));
        let ctx = CollectorContext {
            hostname: "h1",
            host: &host,
            fetched: Some(&fh),
        };
        let entry = OsqueryCollector.collect(&ctx).unwrap();
        let parsed: OsqueryData = serde_json::from_value(entry.data).unwrap();
        assert!(parsed.available);
        let raw = parsed.raw.expect("raw");
        assert_eq!(raw[0]["name"], "os_version");
        assert_eq!(raw[0]["columns"]["version"], "26.05");
        assert_eq!(raw[1]["columns"]["version"], "6.18.19");
    }

    #[test]
    fn osquery_records_parse_error_on_malformed_json() {
        let host = host_fixture();
        let fh = fetched_with_osquery(Some(b"not json"));
        let ctx = CollectorContext {
            hostname: "h1",
            host: &host,
            fetched: Some(&fh),
        };
        let entry = OsqueryCollector.collect(&ctx).unwrap();
        let parsed: OsqueryData = serde_json::from_value(entry.data).unwrap();
        assert!(!parsed.available);
        assert!(parsed.raw.is_none());
        assert!(parsed.parse_error.is_some());
    }
}
