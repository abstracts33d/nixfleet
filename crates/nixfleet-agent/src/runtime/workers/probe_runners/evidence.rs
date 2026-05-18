//! Evidence probe runner (RFC-0007 §3.1 + §7). READ-ONLY consumer of
//! the local collector unit's signed evidence file.
//!
//! The collector (compliance-evidence-collector.service) owns its own
//! systemd timer + cadence; this runner does NOT trigger it. On each
//! tick the runner:
//! 1. Reads `evidence_path` (default `/var/lib/nixfleet-compliance/evidence.json`).
//! 2. Verifies the ed25519 signature against the host's SSH ed25519
//!    public key half (RFC-0009 §5). Signature is read from
//!    `<path>.sig` (base64 64-byte sig of the JCS canonical bytes of
//!    the payload's `controls` array).
//! 3. Filters `controls` to `framework == decl.framework` and produces
//!    per-control sub_results. Aggregate Pass iff every framework-
//!    matching control is Pass.
//!
//! Any error (file missing, parse, signature mismatch, framework
//! missing) → `Fail` (RFC-0007 §6 uniform strict mode).

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

    // Host SSH host pubkey (RFC-0009 §5). Read from a conventional
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

    // LOADBEARING: signature is over JCS-canonical bytes (per
    // `nixfleet-compliance-tools/src/lib.rs::sign_evidence` and
    // `docs/evidence-format.md`), not over the on-disk bytes.
    // probe-runner.sh writes evidence.json via `jq` which produces
    // pretty-printed JSON; the signer canonicalises before signing.
    // Verifying against `payload_bytes` (the file as-read) fails
    // unconditionally because the bytes differ. Re-canonicalise here
    // so the verifier signs the same bytes the signer did.
    let parsed: EvidenceFile = match serde_json::from_slice(&payload_bytes) {
        Ok(p) => p,
        Err(err) => return RunnerOutcome::fail(now, format!("evidence probe: parse: {err}")),
    };
    let canonical_bytes = match serde_jcs::to_vec(&parsed) {
        Ok(b) => b,
        Err(err) => {
            return RunnerOutcome::fail(now, format!("evidence probe: canonicalise: {err}"));
        }
    };
    if vk.verify(&canonical_bytes, &sig).is_err() {
        return RunnerOutcome::fail(now, "evidence probe: signature verify failed");
    }
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
    // Parse + verify-against-canonical happened above; both consume
    // `parsed`/`payload_bytes`. The remainder of the runner uses
    // `parsed` directly.

    let probe_mode = probe_level_mode(decl);

    // Expand the one-entry-per-control wire shape into one
    // ProbeSubResult per (control, framework, article) tuple. Each
    // sub-result carries the resolved effective_mode so the CP-side
    // probe_failures applier (RFC-0007 §7.2) can gate by control
    // rather than by whole probe. Controls with effective_mode =
    // Disabled are dropped entirely (no event_log noise for opted-
    // out controls).
    let mut sub_results: Vec<ProbeSubResult> = Vec::new();
    for entry in &parsed.controls {
        let (effective_mode, override_reason) =
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
            override_reason.as_deref(),
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
/// the caller — this fn assumes the control is in scope. Returns the
/// resolved mode plus the operator's audit rationale (`reason`) when
/// an override applied; `None` when the probe-level mode was the
/// fallback (no per-control override declared).
fn resolve_effective_mode(
    decl: &ProbeDecl,
    control_id: &str,
    probe_mode: ProbeMode,
    framework_filter: Option<&str>,
) -> (ProbeMode, Option<String>) {
    if framework_filter.is_some() {
        if let Some(o) = decl.control_overrides.get(control_id) {
            return (o.resolved_mode(), Some(o.reason.clone()));
        }
        return (probe_mode, None);
    }
    // Custom-framework (controls map) declaration. Only listed controls
    // contribute; the listed entry's mode is the effective mode (no
    // fallback to probe-level mode — operators declare each one).
    if let Some(c) = decl.controls.get(control_id) {
        return (c.resolved_mode(), Some(c.reason.clone()));
    }
    // Control not in the explicit list → drop it (mark Disabled so
    // the caller's filter excludes it from sub_results).
    (ProbeMode::Disabled, None)
}

/// Expand one EvidenceControlEntry into ProbeSubResults respecting the
/// probe's selection mode. Pushes one sub-result per (framework,
/// article) tuple in scope. `override_reason` carries the operator's
/// audit rationale (when an override applied) onto every sub-result
/// the entry produces; the value is shared across all per-article
/// rows because the override is on the control, not the article.
fn push_entry_sub_results(
    sub_results: &mut Vec<ProbeSubResult>,
    entry: &nixfleet_proto::evidence::EvidenceControlEntry,
    framework_filter: Option<&str>,
    status: ProbeStatus,
    effective_mode: ProbeMode,
    override_reason: Option<&str>,
) {
    let reason = override_reason.map(|s| s.to_string());
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
                override_reason: reason.clone(),
            });
        } else {
            for article in articles {
                sub_results.push(ProbeSubResult {
                    control_id: entry.control_id.clone(),
                    status,
                    framework: framework.to_string(),
                    article: Some(article.clone()),
                    effective_mode,
                    override_reason: reason.clone(),
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
                override_reason: reason.clone(),
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
                        override_reason: reason.clone(),
                    });
                } else {
                    for article in articles {
                        sub_results.push(ProbeSubResult {
                            control_id: entry.control_id.clone(),
                            status,
                            framework: framework.clone(),
                            article: Some(article.clone()),
                            effective_mode,
                            override_reason: reason.clone(),
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
    // the raw ed25519 public key (RFC-0009 §5 / RFC 4253).
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
        let (m, r) =
            resolve_effective_mode(&decl, "access-control", ProbeMode::Enforce, Some("nis2"));
        assert_eq!(m, ProbeMode::Enforce);
        assert_eq!(r, None, "no override → no reason");
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
        let (m, r) =
            resolve_effective_mode(&decl, "access-control", ProbeMode::Enforce, Some("nis2"));
        assert_eq!(m, ProbeMode::Observe);
        assert_eq!(r.as_deref(), Some("Phase-out"));
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
        let (unlisted, _) =
            resolve_effective_mode(&decl, "secure-boot", ProbeMode::Enforce, None);
        assert_eq!(unlisted, ProbeMode::Disabled);
        // Listed control → its declared mode + (empty) reason.
        let (listed, reason) =
            resolve_effective_mode(&decl, "access-control", ProbeMode::Enforce, None);
        assert_eq!(listed, ProbeMode::Enforce);
        assert_eq!(reason.as_deref(), Some(""));
    }

    #[test]
    fn push_entry_framework_probe_one_sub_result_per_article() {
        let e = entry("access-control", true, "nis2", &["21.i", "21.j"]);
        let mut subs = Vec::new();
        push_entry_sub_results(
            &mut subs,
            &e,
            Some("nis2"),
            ProbeStatus::Pass,
            ProbeMode::Enforce,
            None,
        );
        assert_eq!(subs.len(), 2);
        assert!(subs.iter().all(|s| s.framework == "nis2"));
        assert!(subs.iter().all(|s| s.control_id == "access-control"));
        assert!(subs.iter().all(|s| s.override_reason.is_none()));
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
            None,
        );
        assert!(subs.is_empty());
    }

    #[test]
    fn push_entry_custom_emits_all_native_frameworks() {
        let mut e = entry("access-control", false, "nis2", &["21.i"]);
        e.framework_articles
            .insert("iso27001".into(), vec!["A.5.1".into()]);
        let mut subs = Vec::new();
        push_entry_sub_results(
            &mut subs,
            &e,
            None,
            ProbeStatus::Fail,
            ProbeMode::Enforce,
            None,
        );
        assert_eq!(subs.len(), 2);
        let frameworks: std::collections::HashSet<_> =
            subs.iter().map(|s| s.framework.clone()).collect();
        assert!(frameworks.contains("nis2"));
        assert!(frameworks.contains("iso27001"));
        assert!(subs.iter().all(|s| s.effective_mode == ProbeMode::Enforce));
    }

    #[test]
    fn push_entry_override_reason_propagates_to_every_sub_result() {
        // Bug D-style audit-trail regression: when an operator
        // declares `controlOverrides[ac] = { mode = observe; reason
        // = "Phase-out"; }`, every sub_result for that control (one
        // per article) must carry the reason so the CP event_log
        // payload preserves it across the wire.
        let e = entry("access-control", false, "nis2", &["21.i", "21.j"]);
        let mut subs = Vec::new();
        push_entry_sub_results(
            &mut subs,
            &e,
            Some("nis2"),
            ProbeStatus::Fail,
            ProbeMode::Observe,
            Some("Phase-out window"),
        );
        assert_eq!(subs.len(), 2);
        assert!(
            subs.iter()
                .all(|s| s.override_reason.as_deref() == Some("Phase-out window"))
        );
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
        push_entry_sub_results(
            &mut subs,
            &e,
            None,
            ProbeStatus::Fail,
            ProbeMode::Enforce,
            None,
        );
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].framework, "custom");
        assert_eq!(subs[0].article, None);
    }

    /// Regression guard: the agent's evidence runner must verify the
    /// signature over JCS-canonical bytes, not over the on-disk file
    /// bytes. The compliance collector signs the canonical form (per
    /// `nixfleet-compliance-tools/src/lib.rs::sign_evidence`); the
    /// on-disk JSON is pretty-printed by `jq`. Verifying against the
    /// on-disk bytes fails every time because the two byte sequences
    /// differ. This test pins that they differ so any future change
    /// that re-introduces the bug fails loudly here.
    #[test]
    fn canonical_bytes_differ_from_pretty_printed() {
        use nixfleet_proto::evidence::{EvidenceControlEntry, EvidenceFile, SCHEMA_VERSION};
        let mut fa = HashMap::new();
        fa.insert("nis2-essential".to_string(), vec!["art21.i".to_string()]);
        let file = EvidenceFile {
            schema_version: SCHEMA_VERSION,
            hostname: "agent-01".to_string(),
            collected_at: chrono::Utc::now(),
            controls: vec![EvidenceControlEntry {
                control_id: "access-control".to_string(),
                passed: true,
                framework_articles: fa,
                details: Some(serde_json::json!({"k": "v"})),
                schema: None,
            }],
        };

        // Pretty-printed (what jq writes; what payload_bytes contains
        // when the agent reads the file).
        let pretty = serde_json::to_vec_pretty(&file).unwrap();
        // Canonical (what the compliance collector signs).
        let canonical = serde_jcs::to_vec(&file).unwrap();
        assert_ne!(
            pretty, canonical,
            "pretty-printed and JCS-canonical bytes MUST differ; if they ever \
             converge the agent's verify path becomes a no-op signature check",
        );

        // Round-trip stability: canonicalising the canonical bytes is a
        // fixed point. The agent's verify calls serde_jcs::to_vec on a
        // freshly-parsed EvidenceFile; this asserts the result is what
        // the signer signed.
        let reparsed: EvidenceFile = serde_json::from_slice(&canonical).unwrap();
        let recanonical = serde_jcs::to_vec(&reparsed).unwrap();
        assert_eq!(canonical, recanonical);
    }
}
