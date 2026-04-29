//! Runtime compliance gate (issue #57 / arcanesys/nixfleet#4).
//!
//! Post fire-and-forget activation, BEFORE the agent posts confirm,
//! it must verify that compliance probes have re-run against the
//! newly-active closure and produced fresh evidence. Without this
//! gate, the rollout engine can promote a non-compliant host on
//! yesterday's PASS data.
//!
//! Mirrors the freshness-verify-after-async-trigger pattern from
//! ADR-011 fire-and-forget activation:
//!
//! | ADR-011 fire-and-forget                          | Runtime gate (here)                                  |
//! |--------------------------------------------------|------------------------------------------------------|
//! | `systemd-run --unit=nixfleet-switch` (async)     | `systemctl start compliance-evidence-collector`      |
//! | Poll `/run/current-system` for expected basename | Read `evidence.json.timestamp >= activation_time`    |
//! | Poll budget timeout → SwitchFailed               | Collector timeout → RuntimeGateError                 |
//! | ±60s skew slack on freshness gate                | ±60s skew slack on timestamp comparison              |
//!
//! Same shape: don't trust the async trigger fired; verify the
//! observable post-condition.
//!
//! ## Graceful degradation
//!
//! Fleets without `nixfleet-compliance` deployed have no collector
//! unit. The gate detects this via `systemctl status` and skips
//! cleanly with a debug log — never errors out. This keeps the
//! agent compatible with non-compliance fleets without a feature
//! flag.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

/// Default path the `compliance-evidence-collector` writes to.
/// Matches `compliance.evidence.collector.outputDir` in
/// nixfleet-compliance (default `/var/lib/nixfleet-compliance`).
/// Configurable per-host via `--compliance-evidence-path` if the
/// operator overrode the collector's outputDir.
pub const DEFAULT_EVIDENCE_PATH: &str = "/var/lib/nixfleet-compliance/evidence.json";

/// Default systemd unit name. Matches the unit declared in
/// `nixfleet-compliance/evidence/collector.nix`.
pub const COLLECTOR_UNIT: &str = "compliance-evidence-collector.service";

/// How long the agent waits for `systemctl start --wait` to return
/// before declaring the collector hung. 120s is generous — on lab
/// the collector typically completes in <2s; the budget covers
/// hosts with many probes or slow disks. Tunable via
/// `--compliance-collector-timeout-secs` if needed.
pub const COLLECTOR_TIMEOUT: Duration = Duration::from_secs(120);

/// Symmetric slack applied to the timestamp freshness comparison.
/// Mirrors `freshness::CLOCK_SKEW_SLACK_SECS`. The agent's clock
/// and the collector's clock are the same kernel, so the slack is
/// really for "evidence collected within slack-secs OF
/// activation_completed_at" rather than clock skew per se.
pub const TIMESTAMP_SLACK_SECS: i64 = 60;

// Re-export the canonical `GateMode` from nixfleet-proto. Earlier
// revisions had three parallel definitions (Nix enum / agent enum /
// CP `&str` matching) — the proto crate is now the single source of
// truth for the policy vocabulary, and every layer parses + matches
// against the same variants.
pub use nixfleet_proto::compliance::GateMode;

/// Wire-shape of `evidence.json` written by the
/// nixfleet-compliance probe-runner. Public because callers may
/// want to inspect specific controls; the gate logic only uses a
/// subset of fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceEvidence {
    pub host: String,
    pub timestamp: DateTime<Utc>,
    #[serde(default)]
    pub controls: Vec<ControlEvidence>,
    #[serde(default)]
    pub overall: String,
}

/// One row of the `controls` array. `framework_articles` is an
/// attrset on the wire (`{nis2: ["21(b)"], iso27001: ["A.8.15"]}`);
/// callers flatten it into a `framework:article` string list when
/// building `ReportEvent::ComplianceFailure`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlEvidence {
    pub control: String,
    pub status: String,
    #[serde(default, rename = "framework_articles")]
    pub framework_articles: serde_json::Value,
    #[serde(default)]
    pub checks: serde_json::Value,
}

/// Outcome of `run_runtime_gate`. Mirrors `ActivationOutcome` shape
/// so the caller can pattern-match similarly.
#[derive(Debug, Clone)]
pub enum GateOutcome {
    /// Collector ran, evidence is fresh, all controls compliant or
    /// the gate is informational-only. Agent proceeds to confirm.
    Pass {
        evidence: ComplianceEvidence,
    },
    /// Collector ran, evidence is fresh, but at least one control
    /// is `non-compliant` or `error`. Agent posts one
    /// `ComplianceFailure` event per failing control. Whether to
    /// also block the confirm is the CP rollout engine's call —
    /// the agent always reports honestly.
    Failures {
        evidence: ComplianceEvidence,
        failures: Vec<ControlEvidence>,
    },
    /// Collector wasn't even installed on this host. Skip the gate
    /// silently; non-compliance fleets are valid configurations.
    Skipped {
        reason: String,
    },
    /// Collector failed, timed out, or evidence is stale relative
    /// to `activation_completed_at`. Agent posts
    /// `ReportEvent::RuntimeGateError` and refuses to confirm —
    /// magic-rollback fires on the deadline. Defense-in-depth
    /// match for ADR-011's poll-timeout class.
    GateError {
        reason: String,
        collector_exit_code: Option<i32>,
        evidence_collected_at: Option<DateTime<Utc>>,
    },
}

/// Resolve the effective gate mode from operator input + collector
/// presence. Public so the caller can know what mode the gate will
/// run in — needed to decide whether `RuntimeGateError` should
/// block confirm.
///
/// Resolution:
/// - explicit `Disabled` → `Disabled` (regardless of presence)
/// - explicit `Permissive` / `Enforce` + collector present → that mode
/// - explicit `Permissive` / `Enforce` + collector absent →
///   `Disabled`, with a warn-log (operator configured for
///   measurement but no collector deployed — likely an oversight
///   worth flagging, but NOT a measurement failure). Caller
///   posts no event for this case.
/// - `None` (auto) → `Permissive` if present, `Disabled` if absent.
pub async fn resolve_mode(input: Option<GateMode>) -> GateMode {
    let collector_present = collector_unit_present().await;
    match input {
        Some(GateMode::Disabled) => GateMode::Disabled,
        Some(m @ (GateMode::Permissive | GateMode::Enforce)) if collector_present => m,
        Some(explicit) => {
            tracing::warn!(
                ?explicit,
                "compliance gate configured to enforce/permissive but \
                 {} not present — skipping. Either deploy \
                 nixfleet-compliance or set complianceGate.mode = \"disabled\".",
                COLLECTOR_UNIT
            );
            GateMode::Disabled
        }
        None => {
            if collector_present {
                GateMode::Permissive
            } else {
                GateMode::Disabled
            }
        }
    }
}

/// Run the runtime compliance gate.
///
/// Caller is expected to have resolved the mode via `resolve_mode`
/// before invoking. `Disabled` short-circuits to `Skipped` — caller
/// can pass it through unchanged. The gate's main work runs only
/// for `Permissive` and `Enforce`; the difference between those
/// two is interpreted by the caller (whether `GateError` blocks
/// confirm), not by the gate body.
///
/// Sequence:
/// 1. If `Disabled`: return `Skipped` immediately. No state changes,
///    no events, no journal warnings.
/// 2. Trigger `systemctl start --wait <unit>` with bounded timeout.
///    The collector unit's presence was confirmed by `resolve_mode`
///    so this is expected to find the unit.
/// 3. Read evidence.json.
/// 4. Verify timestamp >= `activation_completed_at - SLACK`.
/// 5. Classify into Pass / Failures based on `controls[*].status`.
pub async fn run_runtime_gate(
    activation_completed_at: DateTime<Utc>,
    evidence_path: &Path,
    effective_mode: GateMode,
) -> GateOutcome {
    if matches!(effective_mode, GateMode::Disabled) {
        // Caller resolved to Disabled either explicitly or via
        // auto-detection (collector unit absent). Either way:
        // skip cleanly, no events, no log noise.
        return GateOutcome::Skipped {
            reason: "gate mode disabled (collector absent or operator-suppressed)"
                .to_string(),
        };
    }

    // Step 2: trigger + wait for the collector.
    let trigger_result = trigger_collector_with_timeout(COLLECTOR_TIMEOUT).await;
    let collector_exit: Option<i32> = match trigger_result {
        Ok(()) => None,
        Err(TriggerError::Timeout) => {
            return GateOutcome::GateError {
                reason: format!(
                    "{COLLECTOR_UNIT} did not complete within {}s",
                    COLLECTOR_TIMEOUT.as_secs()
                ),
                collector_exit_code: None,
                evidence_collected_at: None,
            };
        }
        Err(TriggerError::NonZero(code)) => {
            return GateOutcome::GateError {
                reason: format!(
                    "{COLLECTOR_UNIT} exited non-zero ({:?}); evidence may be stale",
                    code
                ),
                collector_exit_code: code,
                evidence_collected_at: None,
            };
        }
        Err(TriggerError::Spawn(err)) => {
            return GateOutcome::GateError {
                reason: format!("could not invoke systemctl: {err}"),
                collector_exit_code: None,
                evidence_collected_at: None,
            };
        }
    };
    // collector_exit currently always None on success — kept as a
    // typed binding so future systemctl wrappers that expose exec
    // status (via systemctl show) can plumb it without an API
    // change. Unused for now.
    let _: Option<i32> = collector_exit;
    // Effective_mode is consumed at the end (Pass/Failures
    // classification doesn't differ between Permissive and Enforce
    // — only the caller's response to the outcome differs).
    let _ = effective_mode;

    // Step 3: read evidence.json.
    let evidence = match read_evidence(evidence_path).await {
        Ok(e) => e,
        Err(err) => {
            return GateOutcome::GateError {
                reason: format!("read {}: {err}", evidence_path.display()),
                collector_exit_code: None,
                evidence_collected_at: None,
            };
        }
    };

    // Step 4: freshness check. Evidence must have been collected
    // at or after `activation_completed_at - SLACK` to be
    // considered post-activation. The slack absorbs sub-second
    // ordering between fire-and-forget poll completion and the
    // collector's timestamp generation.
    let min_acceptable =
        activation_completed_at - chrono::Duration::seconds(TIMESTAMP_SLACK_SECS);
    if evidence.timestamp < min_acceptable {
        return GateOutcome::GateError {
            reason: format!(
                "evidence stale: collected_at={} < activation_completed_at-{}s={}",
                evidence.timestamp, TIMESTAMP_SLACK_SECS, min_acceptable
            ),
            collector_exit_code: None,
            evidence_collected_at: Some(evidence.timestamp),
        };
    }

    // Step 5: classify.
    let failures: Vec<ControlEvidence> = evidence
        .controls
        .iter()
        .filter(|c| c.status == "non-compliant" || c.status == "error")
        .cloned()
        .collect();

    if failures.is_empty() {
        GateOutcome::Pass { evidence }
    } else {
        GateOutcome::Failures {
            evidence,
            failures,
        }
    }
}

/// Detect whether the collector unit exists on this host.
async fn collector_unit_present() -> bool {
    Command::new("systemctl")
        .arg("cat")
        .arg(COLLECTOR_UNIT)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

#[derive(Debug)]
enum TriggerError {
    Timeout,
    NonZero(Option<i32>),
    Spawn(anyhow::Error),
}

/// `systemctl start --wait <unit>` with a hard wall-clock timeout.
/// `--wait` blocks until the (oneshot) unit exits, so the agent
/// has a synchronous "collector done" signal. The wall-clock
/// timeout protects against a stuck unit (e.g. a probe in a busy
/// loop) — without it the agent could block forever.
async fn trigger_collector_with_timeout(
    timeout: Duration,
) -> std::result::Result<(), TriggerError> {
    let spawn_future = Command::new("systemctl")
        .arg("start")
        .arg("--wait")
        .arg(COLLECTOR_UNIT)
        .status();
    match tokio::time::timeout(timeout, spawn_future).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(status)) => Err(TriggerError::NonZero(status.code())),
        Ok(Err(err)) => {
            Err(TriggerError::Spawn(anyhow::Error::from(err).context(
                "spawn `systemctl start --wait`",
            )))
        }
        Err(_) => Err(TriggerError::Timeout),
    }
}

async fn read_evidence(path: &Path) -> Result<ComplianceEvidence> {
    let raw = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("read {}", path.display()))?;
    let parsed: ComplianceEvidence = serde_json::from_str(&raw)
        .with_context(|| format!("parse JSON at {}", path.display()))?;
    Ok(parsed)
}

/// Flatten `framework_articles` (an attrset on the wire) into a
/// `vec!["framework:article", ...]` for `ReportEvent::ComplianceFailure`.
/// Defensive against the field being null / non-attrset.
pub fn flatten_framework_articles(value: &serde_json::Value) -> Vec<String> {
    let Some(obj) = value.as_object() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut keys: Vec<&String> = obj.keys().collect();
    keys.sort();
    for fw in keys {
        if let Some(arts) = obj.get(fw).and_then(|v| v.as_array()) {
            for art in arts {
                if let Some(s) = art.as_str() {
                    out.push(format!("{fw}:{s}"));
                }
            }
        }
    }
    out
}

/// Truncate `evidence_snippet` to ~1KB to bound report size on the
/// wire. The CP records the full structured object, but keeping
/// per-event payloads small avoids unbounded growth in the agent's
/// in-memory ring buffer + the proto wire shape.
pub fn truncate_evidence_snippet(checks: &serde_json::Value) -> serde_json::Value {
    let serialized = serde_json::to_string(checks)
        .expect("serde_json::to_string on a serde_json::Value is infallible");
    if serialized.len() <= 1024 {
        return checks.clone();
    }
    // Truncate to a string field if the structured form is too
    // large. Operators always have the full `evidence.json` on the
    // host for triage; the wire copy is a triage hint, not the
    // source of truth.
    serde_json::json!({
        "_truncated_": true,
        "_original_size_bytes_": serialized.len(),
        "_preview_": serialized.chars().take(900).collect::<String>(),
    })
}

/// Default evidence path as a `PathBuf` for use in main.rs.
pub fn default_evidence_path() -> PathBuf {
    PathBuf::from(DEFAULT_EVIDENCE_PATH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatten_framework_articles_handles_attrset() {
        let v = serde_json::json!({
            "nis2": ["21(b)", "21(f)"],
            "iso27001": ["A.8.15"],
        });
        let out = flatten_framework_articles(&v);
        assert_eq!(
            out,
            vec![
                "iso27001:A.8.15".to_string(),
                "nis2:21(b)".to_string(),
                "nis2:21(f)".to_string(),
            ],
        );
    }

    #[test]
    fn flatten_framework_articles_handles_null() {
        assert!(flatten_framework_articles(&serde_json::Value::Null).is_empty());
    }

    #[test]
    fn flatten_framework_articles_handles_empty_obj() {
        assert!(flatten_framework_articles(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn truncate_evidence_snippet_returns_short_unchanged() {
        let v = serde_json::json!({"compliant": true, "x": 1});
        assert_eq!(truncate_evidence_snippet(&v), v);
    }

    #[test]
    fn truncate_evidence_snippet_truncates_large() {
        let big = "x".repeat(2000);
        let v = serde_json::json!({"compliant": false, "blob": big});
        let out = truncate_evidence_snippet(&v);
        assert_eq!(out["_truncated_"], serde_json::Value::Bool(true));
        assert!(out["_original_size_bytes_"].as_u64().unwrap() > 1024);
        assert!(out["_preview_"].as_str().unwrap().len() <= 900);
    }

    // GateMode parsing tests live in `nixfleet-proto::compliance::tests`
    // (single source of truth). The agent re-exports the type and inherits
    // the parsing behaviour by definition.

    #[tokio::test]
    async fn run_runtime_gate_disabled_short_circuits_without_io() {
        // Passing a non-existent path proves the function never tries
        // to read it when the mode is Disabled — the caller's
        // resolve_mode() decision is honoured even if a stale evidence
        // file exists on disk.
        let bogus = std::path::PathBuf::from("/nonexistent/evidence.json");
        let now = chrono::Utc::now();
        let outcome = run_runtime_gate(now, &bogus, GateMode::Disabled).await;
        match outcome {
            GateOutcome::Skipped { .. } => {}
            other => panic!("expected Skipped, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn evidence_parses_real_envelope() {
        // The shape probe-runner.sh writes (host, timestamp,
        // controls, overall). Static fixture so this test doesn't
        // depend on a running collector.
        let raw = r#"{
          "host": "lab",
          "timestamp": "2026-04-29T11:57:38Z",
          "controls": [
            {
              "control": "access-control",
              "status": "compliant",
              "framework_articles": {"nis2": ["21(i)"]},
              "checks": {"compliant": true}
            },
            {
              "control": "auditLogging",
              "status": "non-compliant",
              "framework_articles": {"nis2": ["21(b)"], "iso27001": ["A.8.15"]},
              "checks": {"compliant": false, "rules": {"AL-03": {"compliant": false}}}
            }
          ],
          "overall": "1/2 controls compliant"
        }"#;
        let evidence: ComplianceEvidence = serde_json::from_str(raw).unwrap();
        assert_eq!(evidence.host, "lab");
        assert_eq!(evidence.controls.len(), 2);
        let failures: Vec<_> = evidence
            .controls
            .iter()
            .filter(|c| c.status == "non-compliant")
            .collect();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].control, "auditLogging");
    }
}
