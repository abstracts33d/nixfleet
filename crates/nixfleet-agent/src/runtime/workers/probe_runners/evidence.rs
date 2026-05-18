//! Evidence probe runner (RFC-0010 §3.1 + §7). READ-ONLY consumer of
//! the local collector unit's signed evidence file.
//!
//! The collector (compliance-evidence-collector.service) owns its own
//! systemd timer + cadence; this runner does NOT trigger it. On each
//! tick the runner:
//! 1. Reads `evidence_path` (default `/var/lib/nixfleet-compliance/evidence.json`).
//! 2. Verifies the ed25519 signature against the host's SSH ed25519
//!    public key half (RFC-0004 §5). Signature is read from
//!    `<path>.sig` (base64 64-byte sig of the JCS canonical bytes of
//!    the payload's `controls` array).
//! 3. Filters `controls` to `framework == decl.framework` and produces
//!    per-control sub_results. Aggregate Pass iff every framework-
//!    matching control is Pass.
//!
//! Any error (file missing, parse, signature mismatch, framework
//! missing) → `Fail` (RFC-0010 §6 uniform strict mode).

use base64::Engine as _;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use nixfleet_proto::evidence::{EvidenceFile, SCHEMA_VERSION};
use nixfleet_state_machine::{ProbeMode, ProbeStatus, ProbeSubResult};
use std::path::Path;

use super::{ProbeDecl, RunnerOutcome};

/// Probe-level fallback mode parsed from `ProbeDecl.mode`. Used by the
/// per-control effective-mode resolver when neither
/// `controls`/`controlOverrides` declares a control-specific value.
fn probe_level_mode(decl: &ProbeDecl) -> ProbeMode {
    match decl.mode.as_str() {
        "observe" => ProbeMode::Observe,
        "disabled" => ProbeMode::Disabled,
        _ => ProbeMode::Enforce,
    }
}

pub async fn run(decl: &ProbeDecl, now: DateTime<Utc>) -> RunnerOutcome {
    // Selection mode:
    //   - `framework` set → traditional whole-framework probe (with
    //     optional per-control overrides from `controlOverrides`).
    //   - `controls` non-empty → custom-framework probe (explicit
    //     control list with per-control modes). Native framework
    //     stays on the wire via ProbeSubResult.framework for
    //     auditor visibility.
    // Validation at fleet-eval time (lib/mk-fleet.nix) enforces XOR;
    // runtime check is defence-in-depth.
    let framework_filter = decl.framework.as_deref();
    let explicit_controls_present = !decl.controls.is_empty();
    if framework_filter.is_none() && !explicit_controls_present {
        return RunnerOutcome::fail(
            now,
            "evidence probe: neither framework nor controls declared",
        );
    }
    if framework_filter.is_some() && explicit_controls_present {
        return RunnerOutcome::fail(
            now,
            "evidence probe: framework and controls both set (XOR violation)",
        );
    }
    let evidence_path = Path::new(&decl.evidence_path);
    let sig_path = evidence_path.with_extension("json.sig");

    let payload_bytes = match tokio::fs::read(evidence_path).await {
        Ok(b) => b,
        Err(err) => {
            return RunnerOutcome::fail(
                now,
                format!("evidence probe: read {}: {err}", evidence_path.display()),
            );
        }
    };
    let sig_b64 = match tokio::fs::read_to_string(&sig_path).await {
        Ok(s) => s.trim().to_string(),
        Err(err) => {
            return RunnerOutcome::fail(
                now,
                format!("evidence probe: read {}: {err}", sig_path.display()),
            );
        }
    };

    // Host SSH host pubkey (RFC-0004 §5). Read from a conventional
    // path; the agent's main.rs --ssh-host-key-file points at the
    // PRIVATE half, the public half is alongside as `.pub` per
    // OpenSSH convention. The agent verifies the signature against
    // the public half here.
    let pubkey_bytes = match resolve_host_pubkey().await {
        Ok(b) => b,
        Err(reason) => return RunnerOutcome::fail(now, format!("evidence probe: {reason}")),
    };
    let vk = match VerifyingKey::from_bytes(&pubkey_bytes) {
        Ok(v) => v,
        Err(err) => {
            return RunnerOutcome::fail(now, format!("evidence probe: pubkey parse: {err}"));
        }
    };
    let sig_bytes = match base64::engine::general_purpose::STANDARD.decode(&sig_b64) {
        Ok(b) => b,
        Err(err) => return RunnerOutcome::fail(now, format!("evidence probe: sig base64: {err}")),
    };
    let Ok(sig_arr) = <[u8; 64]>::try_from(sig_bytes.as_slice()) else {
        return RunnerOutcome::fail(
            now,
            format!("evidence probe: sig length {} != 64", sig_bytes.len()),
        );
    };
    let sig = Signature::from_bytes(&sig_arr);
    if vk.verify(&payload_bytes, &sig).is_err() {
        return RunnerOutcome::fail(now, "evidence probe: signature verify failed");
    }

    // Parse the canonical EvidenceFile shape (RFC-0010 §3.1 +
    // nixfleet_proto::evidence). The producer-side contract is the
    // proto's serde struct; if deserialise fails the agent emits a
    // probe Fail with the parse error so operators see exactly which
    // field is wrong (most common: nixfleet-compliance running an
    // older emit shape without schemaVersion).
    let parsed: EvidenceFile = match serde_json::from_slice(&payload_bytes) {
        Ok(p) => p,
        Err(err) => return RunnerOutcome::fail(now, format!("evidence probe: parse: {err}")),
    };
    if parsed.schema_version != SCHEMA_VERSION {
        return RunnerOutcome::fail(
            now,
            format!(
                "evidence probe: schemaVersion {} unsupported (agent expects {SCHEMA_VERSION}); \
                 upgrade nixfleet-compliance",
                parsed.schema_version,
            ),
        );
    }

    let probe_mode = probe_level_mode(decl);

    // Expand the one-entry-per-control wire shape into one
    // ProbeSubResult per (control, framework, article) tuple. Each
    // sub-result carries the resolved effective_mode so the CP-side
    // probe_failures applier (RFC-0010 §7.2) can gate by control
    // rather than by whole probe. Controls with effective_mode =
    // Disabled are dropped entirely (no event_log noise for opted-
    // out controls).
    let mut sub_results: Vec<ProbeSubResult> = Vec::new();
    for entry in &parsed.controls {
        let effective_mode =
            resolve_effective_mode(decl, &entry.control_id, probe_mode, framework_filter);
        if matches!(effective_mode, ProbeMode::Disabled) {
            continue;
        }
        let status = if entry.passed {
            ProbeStatus::Pass
        } else {
            ProbeStatus::Fail
        };
        push_entry_sub_results(
            &mut sub_results,
            entry,
            framework_filter,
            status,
            effective_mode,
        );
    }
    if sub_results.is_empty() {
        let context = match framework_filter {
            Some(f) => format!("evidence probe: no controls match framework '{f}'"),
            None => "evidence probe: no controls matched the explicit selection".to_string(),
        };
        return RunnerOutcome::fail(now, context);
    }

    // Aggregate Pass only over enforce-mode sub-results. Observe-mode
    // failures stay on the wire for visibility but do not fail the
    // probe overall; the wave gate consults the per-row effective_mode
    // on the CP side.
    let enforce_subs: Vec<&ProbeSubResult> = sub_results
        .iter()
        .filter(|s| matches!(s.effective_mode, ProbeMode::Enforce))
        .collect();
    let all_enforce_pass = enforce_subs
        .iter()
        .all(|s| matches!(s.status, ProbeStatus::Pass));
    let aggregate_status = if all_enforce_pass {
        ProbeStatus::Pass
    } else {
        ProbeStatus::Fail
    };
    RunnerOutcome {
        status: aggregate_status,
        observed_at: now,
        failure_reason: if all_enforce_pass {
            None
        } else {
            let descriptor = framework_filter.unwrap_or("custom-controls");
            Some(format!(
                "evidence probe: {}: at least one enforce-mode control failed",
                descriptor
            ))
        },
        sub_results: Some(sub_results),
    }
}

/// Resolve effective mode for a control by consulting the probe's
/// `controls` map (custom-framework declaration) first, then
/// `controlOverrides` (per-framework override), then falling back to
/// the probe-level mode. For framework probes, controls whose
/// frameworkArticles don't cover the probe's framework are skipped at
/// the caller — this fn assumes the control is in scope.
fn resolve_effective_mode(
    decl: &ProbeDecl,
    control_id: &str,
    probe_mode: ProbeMode,
    framework_filter: Option<&str>,
) -> ProbeMode {
    if framework_filter.is_some() {
        if let Some(o) = decl.control_overrides.get(control_id) {
            return o.resolved_mode();
        }
        return probe_mode;
    }
    // Custom-framework (controls map) declaration. Only listed controls
    // contribute; the listed entry's mode is the effective mode (no
    // fallback to probe-level mode — operators declare each one).
    if let Some(c) = decl.controls.get(control_id) {
        return c.resolved_mode();
    }
    // Control not in the explicit list → drop it (mark Disabled so
    // the caller's filter excludes it from sub_results).
    ProbeMode::Disabled
}

/// Expand one EvidenceControlEntry into ProbeSubResults respecting the
/// probe's selection mode. Pushes one sub-result per (framework,
/// article) tuple in scope.
fn push_entry_sub_results(
    sub_results: &mut Vec<ProbeSubResult>,
    entry: &nixfleet_proto::evidence::EvidenceControlEntry,
    framework_filter: Option<&str>,
    status: ProbeStatus,
    effective_mode: ProbeMode,
) {
    if let Some(framework) = framework_filter {
        // Whole-framework probe: emit one sub-result per article of
        // this framework. Controls not covering the framework were
        // filtered out at the caller via resolve_effective_mode +
        // the framework_articles lookup below.
        let Some(articles) = entry.framework_articles.get(framework) else {
            return;
        };
        if articles.is_empty() {
            sub_results.push(ProbeSubResult {
                control_id: entry.control_id.clone(),
                status,
                framework: framework.to_string(),
                article: None,
                effective_mode,
            });
        } else {
            for article in articles {
                sub_results.push(ProbeSubResult {
                    control_id: entry.control_id.clone(),
                    status,
                    framework: framework.to_string(),
                    article: Some(article.clone()),
                    effective_mode,
                });
            }
        }
    } else {
        // Custom-framework declaration. Emit one sub-result per
        // (framework, article) tuple from the control's native
        // frameworkArticles map. If the control has no native
        // framework (synthetic / smoke), emit a single sub-result
        // with framework="custom" and article=None so the CP gate
        // still sees the control.
        if entry.framework_articles.is_empty() {
            sub_results.push(ProbeSubResult {
                control_id: entry.control_id.clone(),
                status,
                framework: "custom".to_string(),
                article: None,
                effective_mode,
            });
        } else {
            for (framework, articles) in &entry.framework_articles {
                if articles.is_empty() {
                    sub_results.push(ProbeSubResult {
                        control_id: entry.control_id.clone(),
                        status,
                        framework: framework.clone(),
                        article: None,
                        effective_mode,
                    });
                } else {
                    for article in articles {
                        sub_results.push(ProbeSubResult {
                            control_id: entry.control_id.clone(),
                            status,
                            framework: framework.clone(),
                            article: Some(article.clone()),
                            effective_mode,
                        });
                    }
                }
            }
        }
    }
}

async fn resolve_host_pubkey() -> Result<[u8; 32], String> {
    // The agent's CLI accepts `--ssh-host-key-file` (defaulting to
    // `/etc/ssh/ssh_host_ed25519_key`). The matching public half lives
    // alongside as `<path>.pub`. We read the public file here so the
    // probe runner doesn't need the private key (which it has no
    // business with).
    let priv_path = std::env::var("NIXFLEET_AGENT_SSH_HOST_KEY_FILE")
        .unwrap_or_else(|_| "/etc/ssh/ssh_host_ed25519_key".to_string());
    let pub_path = format!("{priv_path}.pub");
    let raw = tokio::fs::read_to_string(&pub_path)
        .await
        .map_err(|err| format!("read host pubkey {pub_path}: {err}"))?;
    // OpenSSH public key format: "ssh-ed25519 <base64-blob> <comment>".
    let blob_b64 = raw
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("malformed host pubkey at {pub_path}"))?;
    let blob = base64::engine::general_purpose::STANDARD
        .decode(blob_b64)
        .map_err(|err| format!("host pubkey base64 decode: {err}"))?;
    // The blob is a length-prefixed wire format; the last 32 bytes are
    // the raw ed25519 public key (RFC-0004 §5 / RFC 4253).
    if blob.len() < 32 {
        return Err(format!("host pubkey blob len {} < 32", blob.len()));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&blob[blob.len() - 32..]);
    Ok(out)
}

// Wire shape lives in nixfleet_proto::evidence::EvidenceFile so the
// auditor verifier (nixfleet-compliance-verify) and compliance-check
// CLI consume the same canonical schema. Drift between producer +
// consumer is a compile error rather than a runtime parse failure.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::workers::probe_runners::ControlOverrideDecl;
    use nixfleet_proto::evidence::EvidenceControlEntry;
    use std::collections::HashMap;

    fn base_decl(framework: Option<&str>) -> ProbeDecl {
        ProbeDecl {
            kind: "evidence".into(),
            mode: "enforce".into(),
            interval_seconds: 30,
            run_once: false,
            url: None,
            expect_status: 200,
            host: None,
            port: None,
            connect_timeout_secs: 5,
            command: Vec::new(),
            timeout_secs: 10,
            framework: framework.map(|s| s.to_string()),
            evidence_path: "/var/lib/nixfleet-compliance/evidence.json".into(),
            control_overrides: HashMap::new(),
            controls: HashMap::new(),
        }
    }

    fn entry(
        control_id: &str,
        passed: bool,
        framework: &str,
        articles: &[&str],
    ) -> EvidenceControlEntry {
        let mut fa = HashMap::new();
        fa.insert(
            framework.to_string(),
            articles.iter().map(|s| s.to_string()).collect(),
        );
        EvidenceControlEntry {
            control_id: control_id.into(),
            passed,
            framework_articles: fa,
            details: None,
            schema: None,
        }
    }

    #[test]
    fn resolve_effective_mode_framework_probe_no_overrides_uses_probe_mode() {
        let decl = base_decl(Some("nis2"));
        let m = resolve_effective_mode(&decl, "access-control", ProbeMode::Enforce, Some("nis2"));
        assert_eq!(m, ProbeMode::Enforce);
    }

    #[test]
    fn resolve_effective_mode_framework_probe_override_wins_over_probe_mode() {
        let mut decl = base_decl(Some("nis2"));
        decl.control_overrides.insert(
            "access-control".into(),
            ControlOverrideDecl {
                mode: "observe".into(),
                reason: "Phase-out".into(),
            },
        );
        let m = resolve_effective_mode(&decl, "access-control", ProbeMode::Enforce, Some("nis2"));
        assert_eq!(m, ProbeMode::Observe);
    }

    #[test]
    fn resolve_effective_mode_custom_controls_unlisted_dropped() {
        let mut decl = base_decl(None);
        decl.controls.insert(
            "access-control".into(),
            ControlOverrideDecl {
                mode: "enforce".into(),
                reason: String::new(),
            },
        );
        // Unlisted control → Disabled (filtered out downstream).
        let unlisted =
            resolve_effective_mode(&decl, "secure-boot", ProbeMode::Enforce, None);
        assert_eq!(unlisted, ProbeMode::Disabled);
        // Listed control → its declared mode.
        let listed =
            resolve_effective_mode(&decl, "access-control", ProbeMode::Enforce, None);
        assert_eq!(listed, ProbeMode::Enforce);
    }

    #[test]
    fn push_entry_framework_probe_one_sub_result_per_article() {
        let e = entry("access-control", true, "nis2", &["21.i", "21.j"]);
        let mut subs = Vec::new();
        push_entry_sub_results(&mut subs, &e, Some("nis2"), ProbeStatus::Pass, ProbeMode::Enforce);
        assert_eq!(subs.len(), 2);
        assert!(subs.iter().all(|s| s.framework == "nis2"));
        assert!(subs.iter().all(|s| s.control_id == "access-control"));
    }

    #[test]
    fn push_entry_framework_probe_skips_when_framework_absent_from_articles() {
        let e = entry("access-control", true, "nis2", &["21.i"]);
        let mut subs = Vec::new();
        push_entry_sub_results(
            &mut subs,
            &e,
            Some("iso27001"),
            ProbeStatus::Pass,
            ProbeMode::Enforce,
        );
        assert!(subs.is_empty());
    }

    #[test]
    fn push_entry_custom_emits_all_native_frameworks() {
        let mut e = entry("access-control", false, "nis2", &["21.i"]);
        e.framework_articles
            .insert("iso27001".into(), vec!["A.5.1".into()]);
        let mut subs = Vec::new();
        push_entry_sub_results(&mut subs, &e, None, ProbeStatus::Fail, ProbeMode::Enforce);
        assert_eq!(subs.len(), 2);
        let frameworks: std::collections::HashSet<_> =
            subs.iter().map(|s| s.framework.clone()).collect();
        assert!(frameworks.contains("nis2"));
        assert!(frameworks.contains("iso27001"));
        assert!(subs.iter().all(|s| s.effective_mode == ProbeMode::Enforce));
    }

    #[test]
    fn push_entry_custom_synthetic_no_framework_articles() {
        // Control with empty frameworkArticles (e.g. the always-fail
        // synthetic control). Custom-framework selection emits one
        // sub-result with framework="custom".
        let e = EvidenceControlEntry {
            control_id: "synthetic".into(),
            passed: false,
            framework_articles: HashMap::new(),
            details: None,
            schema: None,
        };
        let mut subs = Vec::new();
        push_entry_sub_results(&mut subs, &e, None, ProbeStatus::Fail, ProbeMode::Enforce);
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].framework, "custom");
        assert_eq!(subs[0].article, None);
    }
}
