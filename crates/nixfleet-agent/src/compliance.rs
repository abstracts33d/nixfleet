//! Runtime compliance gate.
//!
//! After fire-and-forget activation, before posting confirm: trigger
//! the evidence collector and verify it produced fresh evidence
//! against the new closure. Without this gate the rollout engine
//! could promote a non-compliant host on yesterday's PASS data.
//!
//! Same freshness-verify-after-async-trigger pattern as activation:
//! don't trust the async trigger fired, verify the post-condition.
//!
//! Fleets without `nixfleet-compliance` deployed have no collector
//! unit; the gate detects this and skips cleanly (debug log).

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

/// Matches `compliance.evidence.collector.outputDir` in
/// nixfleet-compliance.
pub const DEFAULT_EVIDENCE_PATH: &str = "/var/lib/nixfleet-compliance/evidence.json";

pub const COLLECTOR_UNIT: &str = "compliance-evidence-collector.service";

/// Generous: the collector typically completes in <2s on lab; the
/// budget covers hosts with many probes or slow disks.
pub const COLLECTOR_TIMEOUT: Duration = Duration::from_secs(120);

/// "Evidence collected within N seconds of activation_completed_at"
/// — agent and collector share a kernel, so this isn't strictly
/// clock skew, but the slack absorbs runtime noise.
pub const TIMESTAMP_SLACK_SECS: i64 = 60;

pub use nixfleet_proto::compliance::GateMode;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceEvidence {
    pub host: String,
    pub timestamp: DateTime<Utc>,
    #[serde(default)]
    pub controls: Vec<ControlEvidence>,
    #[serde(default)]
    pub overall: String,
}

/// `framework_articles` is `{nis2: ["21(b)"], iso27001: ["A.8.15"]}`
/// on the wire; callers flatten to `framework:article` strings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlEvidence {
    pub control: String,
    pub status: String,
    #[serde(default, rename = "framework_articles")]
    pub framework_articles: serde_json::Value,
    #[serde(default)]
    pub checks: serde_json::Value,
}

#[derive(Debug, Clone)]
pub enum GateOutcome {
    /// All controls compliant on fresh evidence. Confirm.
    Pass {
        evidence: ComplianceEvidence,
    },
    /// At least one control non-compliant on fresh evidence. Agent
    /// posts one `ComplianceFailure` per failing control; CP decides
    /// whether to block confirm.
    Failures {
        evidence: ComplianceEvidence,
        failures: Vec<ControlEvidence>,
    },
    /// Collector not installed; non-compliance fleets are valid.
    Skipped {
        reason: String,
    },
    /// Collector failed/timed out, or evidence stale relative to
    /// `activation_completed_at`. Agent posts `RuntimeGateError`
    /// and refuses confirm; magic-rollback fires on the deadline.
    GateError {
        reason: String,
        collector_exit_code: Option<i32>,
        evidence_collected_at: Option<DateTime<Utc>>,
    },
}

/// Resolution:
/// - explicit `Disabled` → `Disabled`
/// - `Permissive`/`Enforce` + collector present → that mode
/// - `Permissive`/`Enforce` + collector absent → `Disabled` + warn
///   (operator misconfigured: measurement requested but no collector)
/// - `None` (auto) → `Permissive` if present, `Disabled` if absent
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
///   no events, no journal warnings.
/// 2. Trigger `systemctl start --wait <unit>` with bounded timeout.
///   The collector unit's presence was confirmed by `resolve_mode`
///   so this is expected to find the unit.
/// 3. Read evidence.json.
/// 4. Verify timestamp >= `activation_completed_at - SLACK`.
/// 5. Classify into Pass / Failures based on `controls[*].status`.
pub async fn run_runtime_gate(
    activation_completed_at: DateTime<Utc>,
    evidence_path: &Path,
    effective_mode: GateMode,
) -> GateOutcome {
    if matches!(effective_mode, GateMode::Disabled) {
        return GateOutcome::Skipped {
            reason: "gate mode disabled (collector absent or operator-suppressed)"
                .to_string(),
        };
    }

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
    let _: Option<i32> = collector_exit;
    let _ = effective_mode;

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

    // Freshness: evidence must be collected ≥ activation - slack.
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

/// `--wait` blocks until the oneshot unit exits. Wall-clock timeout
/// protects against a stuck probe.
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

/// Bounds wire payload size. Operators have the full `evidence.json`
/// on-host for triage; the wire copy is a hint, not source of truth.
pub fn truncate_evidence_snippet(checks: &serde_json::Value) -> serde_json::Value {
    let serialized = serde_json::to_string(checks)
        .expect("serde_json::to_string on a serde_json::Value is infallible");
    if serialized.len() <= 1024 {
        return checks.clone();
    }
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
        // resolve_mode decision is honoured even if a stale evidence
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
