//! Per-kind probe runners (RFC-0010 §3.1). Each runner consumes a
//! `ProbeDecl` + returns a `RunnerOutcome`. Uniform strict-mode
//! semantics: any runtime error → `ProbeStatus::Fail` with a
//! `failure_reason` string. Per RFC-0010 §6 there is no `Unknown` or
//! "swallowed error" class.
//!
//! Runners are pure (modulo I/O and the system clock) — they don't
//! emit events; the probe worker handles event emission + state
//! tracking. Each runner is `Send + 'static` so it can be `tokio::spawn`'d.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use nixfleet_state_machine::{ProbeMode, ProbeStatus, ProbeSubResult};
use serde::{Deserialize, Serialize};

pub mod evidence;
pub mod exec;
pub mod http;
pub mod tcp;

/// LOADBEARING: floor on probe interval guards against a misconfigured
/// 0/1-second probe DOSing the host. Operator-declared
/// `intervalSeconds` values below this are rounded up at the worker
/// layer (`crate::runtime::workers::probe::spawn` clamps via
/// `interval_seconds.max(MIN_INTERVAL_SECS)`). A weaker `.max(1)`
/// floor would still let a 1-second HTTP probe issue 60 reqs/min
/// against an operator-unintended backend.
pub const MIN_INTERVAL_SECS: u64 = 5;

/// LOADBEARING: per-failure cap on `failure_reason` string length keeps
/// the wire body bounded. Without truncation, runners can emit
/// arbitrarily long stderr / response bodies that inflate the outbound
/// queue's JSON payloads and event-log row sizes. Runners pass their
/// failure-reason strings through [`truncate_reason`] before
/// constructing a `RunnerOutcome::Fail`.
pub const FAILURE_REASON_MAX_LEN: usize = 512;

/// Truncate to `FAILURE_REASON_MAX_LEN` chars; appends `"...[truncated]"`
/// when truncation fires. UTF-8 safe: bumps `end` back to the prior
/// char boundary if a multibyte sequence would be split.
pub fn truncate_reason(s: String) -> String {
    if s.len() > FAILURE_REASON_MAX_LEN {
        let mut end = FAILURE_REASON_MAX_LEN;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...[truncated]", &s[..end])
    } else {
        s
    }
}

/// On-disk probe declaration. Loaded from
/// `/etc/nixfleet/agent/health-checks.json` (rendered from
/// `lib/mk-fleet.nix:effectiveHealthChecks` by `_agent.nix`).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeDecl {
    pub kind: String, // "http" | "tcp" | "exec" | "evidence"
    pub mode: String, // "enforce" | "observe" | "disabled"
    #[serde(default = "default_interval_seconds")]
    pub interval_seconds: u64,
    #[serde(default)]
    pub run_once: bool,
    // kind-specific (all optional; runner validates what it needs)
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default = "default_expect_status")]
    pub expect_status: u16,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default = "default_connect_timeout_secs")]
    pub connect_timeout_secs: u64,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub framework: Option<String>,
    #[serde(default = "default_evidence_path")]
    pub evidence_path: String,
    /// Per-control mode overrides on top of `mode`, scoped to the
    /// framework declared in `framework`. Resolved per-control at
    /// runtime: override > probe-level mode.
    #[serde(default)]
    pub control_overrides: HashMap<String, ControlOverrideDecl>,
    /// Explicit per-control selection (custom-framework declaration).
    /// Mutually exclusive with `framework`; eval-time validation in
    /// `lib/mk-fleet.nix` rejects probes that set both.
    #[serde(default)]
    pub controls: HashMap<String, ControlOverrideDecl>,
}

/// Single entry in `controlOverrides` / `controls` (RFC-0010 §3.4
/// per-control granularity). `mode` is the effective mode for the
/// control; `reason` is operator-facing audit rationale, surfaced in
/// event_log + dashboards.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlOverrideDecl {
    pub mode: String,
    #[serde(default)]
    pub reason: String,
}

impl ControlOverrideDecl {
    pub fn resolved_mode(&self) -> ProbeMode {
        match self.mode.as_str() {
            "observe" => ProbeMode::Observe,
            "disabled" => ProbeMode::Disabled,
            _ => ProbeMode::Enforce,
        }
    }
}

fn default_interval_seconds() -> u64 {
    30
}
fn default_expect_status() -> u16 {
    200
}
fn default_connect_timeout_secs() -> u64 {
    5
}
fn default_timeout_secs() -> u64 {
    10
}
fn default_evidence_path() -> String {
    "/var/lib/nixfleet-compliance/evidence.json".to_string()
}

/// Output of one runner invocation.
#[derive(Debug, Clone)]
pub struct RunnerOutcome {
    pub status: ProbeStatus,
    pub observed_at: DateTime<Utc>,
    pub failure_reason: Option<String>,
    /// `None` for non-evidence kinds; `Some(vec)` for evidence runner.
    pub sub_results: Option<Vec<ProbeSubResult>>,
}

impl RunnerOutcome {
    pub fn pass(observed_at: DateTime<Utc>) -> Self {
        Self {
            status: ProbeStatus::Pass,
            observed_at,
            failure_reason: None,
            sub_results: None,
        }
    }

    pub fn fail(observed_at: DateTime<Utc>, reason: impl Into<String>) -> Self {
        Self {
            status: ProbeStatus::Fail,
            observed_at,
            // Truncate at construction time so every runner gets the
            // FAILURE_REASON_MAX_LEN cap via the type funnel
            // (defense-in-depth — runners can't accidentally bypass).
            failure_reason: Some(truncate_reason(reason.into())),
            sub_results: None,
        }
    }
}

/// Dispatch on `decl.kind`. Unknown kinds fail closed.
pub async fn run(decl: &ProbeDecl, now: DateTime<Utc>) -> RunnerOutcome {
    match decl.kind.as_str() {
        "http" => http::run(decl, now).await,
        "tcp" => tcp::run(decl, now).await,
        "exec" => exec::run(decl, now).await,
        "evidence" => evidence::run(decl, now).await,
        other => RunnerOutcome::fail(now, format!("unknown probe kind '{other}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap()
    }

    #[test]
    fn truncate_reason_passes_through_short_strings() {
        let short = "503 Service Unavailable".to_string();
        assert_eq!(truncate_reason(short.clone()), short);
    }

    #[test]
    fn truncate_reason_caps_at_max_len() {
        let long = "x".repeat(FAILURE_REASON_MAX_LEN + 100);
        let truncated = truncate_reason(long);
        assert!(truncated.len() <= FAILURE_REASON_MAX_LEN + "...[truncated]".len());
        assert!(truncated.ends_with("...[truncated]"));
    }

    #[test]
    fn truncate_reason_exact_max_len_is_passthrough() {
        let exact = "x".repeat(FAILURE_REASON_MAX_LEN);
        assert_eq!(truncate_reason(exact.clone()).len(), FAILURE_REASON_MAX_LEN);
    }

    #[test]
    fn truncate_reason_handles_utf8_boundary() {
        // Build a string where byte `FAILURE_REASON_MAX_LEN` lands inside
        // a multibyte UTF-8 sequence. `truncate_reason` must back up to
        // a char boundary instead of slicing mid-codepoint.
        let prefix_len = FAILURE_REASON_MAX_LEN - 1;
        let mut s = "a".repeat(prefix_len);
        s.push('é'); // 2 bytes; first byte at index prefix_len, second at prefix_len+1
        s.push_str("trailing");
        assert!(s.len() > FAILURE_REASON_MAX_LEN);
        let truncated = truncate_reason(s);
        // Must not panic on multibyte slice, must end with "...[truncated]"
        assert!(truncated.ends_with("...[truncated]"));
    }

    #[test]
    fn fail_outcome_applies_truncation_at_construction() {
        // Regression guard: every RunnerOutcome::fail call routes
        // through truncate_reason so runners can produce arbitrary
        // failure-reason content (stderr tails, response bodies,
        // exception traces) without wire-size amplification.
        let huge = "fail reason ".repeat(200); // ~2400 chars
        let outcome = RunnerOutcome::fail(t0(), huge);
        let reason = outcome.failure_reason.expect("failure_reason set");
        assert!(reason.len() <= FAILURE_REASON_MAX_LEN + "...[truncated]".len());
        assert!(reason.ends_with("...[truncated]"));
    }

    #[test]
    fn min_interval_secs_is_5() {
        // Pin the constant against drift. The 5-second floor is the
        // documented operator-DOS-protection baseline.
        assert_eq!(MIN_INTERVAL_SECS, 5);
    }
}
