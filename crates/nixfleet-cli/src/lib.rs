//! Shared CLI logic - table rendering, age math, status classification.
//! Library form so binaries compose against it and unit tests exercise
//! formatting without spinning up a real CP.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use nixfleet_proto::{HostStatusEntry, HostsResponse, RolloutEvents, RolloutHosts};
use reqwest::{Certificate, Identity};

pub mod color;
pub mod commands;
pub mod config;
pub mod operator_cert;
pub use config::{ConfigError, FileConfig, Overrides};
pub use operator_cert::{MintOperatorCertArgs, MintOutcome, mint_operator_cert};

/// Write `~/.config/nixfleet/config.toml` (or `--path`). Returns the absolute
/// path so the bin can report it.
pub fn run_config_init(
    path: &Path,
    cp_url: String,
    ca_cert: PathBuf,
    client_cert: PathBuf,
    client_key: PathBuf,
    overwrite: bool,
) -> Result<PathBuf> {
    if path.exists() && !overwrite {
        anyhow::bail!(
            "{} already exists; pass --force to overwrite",
            path.display(),
        );
    }
    let cfg = config::FileConfig {
        cp_url: Some(cp_url),
        ca_cert: Some(ca_cert),
        client_cert: Some(client_cert),
        client_key: Some(client_key),
    };
    cfg.save(path)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(path.to_path_buf())
}

/// Resolved operator-side config. Every field is required by the time we
/// reach a network call; layered loader (flag > env > file) populates this.
#[derive(Debug, Clone)]
pub struct ResolvedClientConfig {
    pub cp_url: String,
    pub ca_cert: PathBuf,
    pub client_cert: PathBuf,
    pub client_key: PathBuf,
}

pub fn build_client(cfg: &ResolvedClientConfig) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder().use_rustls_tls();
    let pem = std::fs::read(&cfg.ca_cert)
        .with_context(|| format!("read CA cert {}", cfg.ca_cert.display()))?;
    let cert = Certificate::from_pem(&pem).context("parse CA cert PEM")?;
    builder = builder.add_root_certificate(cert);

    let mut id_pem = std::fs::read(&cfg.client_cert)
        .with_context(|| format!("read client cert {}", cfg.client_cert.display()))?;
    let key_pem = std::fs::read(&cfg.client_key)
        .with_context(|| format!("read client key {}", cfg.client_key.display()))?;
    id_pem.extend_from_slice(&key_pem);
    let identity = Identity::from_pem(&id_pem).context("parse client identity PEM")?;
    builder = builder.identity(identity);

    builder.build().context("build HTTP client")
}

pub async fn run_status(cfg: &ResolvedClientConfig, json: bool, color: bool) -> Result<String> {
    let cp_url = cfg.cp_url.trim_end_matches('/');
    let client = build_client(cfg)?;

    let hosts: HostsResponse = client
        .get(format!("{cp_url}/v1/hosts"))
        .send()
        .await
        .with_context(|| format!("GET {cp_url}/v1/hosts"))?
        .error_for_status()?
        .json()
        .await
        .context("parse /v1/hosts response")?;

    if json {
        return serde_json::to_string_pretty(&hosts).context("serialize HostsResponse to JSON");
    }

    let mut channels_seen: Vec<String> = hosts.hosts.iter().map(|h| h.channel.clone()).collect();
    channels_seen.sort();
    channels_seen.dedup();
    let mut channel_freshness: BTreeMap<String, u32> = BTreeMap::new();
    for channel in &channels_seen {
        let resp: serde_json::Value = client
            .get(format!("{cp_url}/v1/channels/{channel}"))
            .send()
            .await
            .with_context(|| format!("GET {cp_url}/v1/channels/{channel}"))?
            .error_for_status()?
            .json()
            .await
            .context("parse /v1/channels response")?;
        if let Some(window) = resp
            .get("freshness_window_minutes")
            .and_then(serde_json::Value::as_u64)
        {
            channel_freshness.insert(channel.clone(), window as u32);
        }
    }

    let inputs = StatusInputs {
        now: Utc::now(),
        hosts: hosts.hosts,
        channel_freshness,
    };
    Ok(render_status_table_with_color(&inputs, color))
}

/// `GET /v1/rollouts/{id}/hosts` — per-host summary. CLI subcommand:
/// `nixfleet rollout hosts <id>`.
pub async fn run_hosts(cfg: &ResolvedClientConfig, rollout_id: &str, json: bool) -> Result<String> {
    let cp_url = cfg.cp_url.trim_end_matches('/');
    let client = build_client(cfg)?;
    let url = format!("{cp_url}/v1/rollouts/{}/hosts", rollout_id);
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!(
            "rollout {rollout_id} has no host_rollout_records (never dispatched, or rollout id unknown)",
        );
    }
    let hosts: RolloutHosts = resp
        .error_for_status()?
        .json()
        .await
        .context("parse /v1/rollouts/{id}/hosts response")?;
    if json {
        return serde_json::to_string_pretty(&hosts).context("serialize RolloutHosts to JSON");
    }
    Ok(render_hosts_table(&hosts))
}

/// `GET /v1/rollouts/{id}/events` — chronological event_log stream.
/// CLI subcommand: `nixfleet rollout events <id>`. Default output is
/// JSON because payload shapes vary by `kind` — a single rendered
/// table would mislead. `json = false` falls back to a compact
/// summary (seq / ts / kind / host).
pub async fn run_events(
    cfg: &ResolvedClientConfig,
    rollout_id: &str,
    limit: Option<i64>,
    json: bool,
) -> Result<String> {
    let cp_url = cfg.cp_url.trim_end_matches('/');
    let client = build_client(cfg)?;
    let mut url = format!("{cp_url}/v1/rollouts/{}/events", rollout_id);
    if let Some(n) = limit {
        url.push_str(&format!("?limit={n}"));
    }
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!("rollout {rollout_id} unknown");
    }
    let events: RolloutEvents = resp
        .error_for_status()?
        .json()
        .await
        .context("parse /v1/rollouts/{id}/events response")?;
    if json {
        return serde_json::to_string_pretty(&events).context("serialize RolloutEvents to JSON");
    }
    Ok(render_events_summary(&events))
}

/// Compact summary table for `--no-json` mode. One line per event:
/// `seq  ts  kind  host`. The full payload is only available via
/// the default JSON output.
pub fn render_events_summary(events: &RolloutEvents) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "rollout {} — {} event(s)\n",
        events.rollout_id,
        events.events.len()
    ));
    s.push_str(&format!(
        "{:>8} {:24} {:18} {}\n",
        "SEQ", "TS", "KIND", "HOST"
    ));
    for e in &events.events {
        s.push_str(&format!(
            "{:>8} {:24} {:18} {}\n",
            e.seq,
            e.ts,
            e.kind,
            e.host.as_deref().unwrap_or("-"),
        ));
    }
    s
}

pub struct StatusInputs {
    pub now: DateTime<Utc>,
    pub hosts: Vec<HostStatusEntry>,
    /// channel name -> freshness_window in minutes (from `/v1/channels/{name}`).
    pub channel_freshness: BTreeMap<String, u32>,
}

pub fn render_status_table(input: &StatusInputs) -> String {
    let mut rows: Vec<[String; 6]> = Vec::with_capacity(input.hosts.len() + 1);
    rows.push([
        "HOST".into(),
        "CHANNEL".into(),
        "CURRENT".into(),
        "DECLARED".into(),
        "STATUS".into(),
        "COMPLIANCE".into(),
    ]);
    for host in &input.hosts {
        rows.push([
            host.hostname.clone(),
            host.channel.clone(),
            display_hash(host.current_closure_hash.as_deref(), "<unseen>"),
            display_hash(host.declared_closure_hash.as_deref(), "<unset>"),
            status_label(
                host,
                input.now,
                input.channel_freshness.get(&host.channel).copied(),
            ),
            compliance_label(host),
        ]);
    }

    let mut widths = [0usize; 6];
    for row in &rows {
        for (i, col) in row.iter().enumerate() {
            widths[i] = widths[i].max(col.chars().count());
        }
    }

    let mut out = String::new();
    for row in &rows {
        for (i, col) in row.iter().enumerate() {
            if i > 0 {
                out.push_str("  ");
            }
            out.push_str(col);
            if i + 1 < row.len() {
                let pad = widths[i].saturating_sub(col.chars().count());
                for _ in 0..pad {
                    out.push(' ');
                }
            }
        }
        out.push('\n');
    }
    out
}

pub fn render_status_table_with_color(input: &StatusInputs, color: bool) -> String {
    use crate::color::Stylizer;
    let st = Stylizer { enabled: color };
    let mut rows: Vec<[(String, String); 6]> = Vec::with_capacity(input.hosts.len() + 1);
    rows.push([
        ("HOST".into(), "HOST".into()),
        ("CHANNEL".into(), "CHANNEL".into()),
        ("CURRENT".into(), "CURRENT".into()),
        ("DECLARED".into(), "DECLARED".into()),
        ("STATUS".into(), "STATUS".into()),
        ("COMPLIANCE".into(), "COMPLIANCE".into()),
    ]);
    for host in &input.hosts {
        let raw_status = status_label(
            host,
            input.now,
            input.channel_freshness.get(&host.channel).copied(),
        );
        let painted = paint_status(&st, &raw_status);
        let current = display_hash(host.current_closure_hash.as_deref(), "<unseen>");
        let declared = display_hash(host.declared_closure_hash.as_deref(), "<unset>");
        let compliance = compliance_label(host);
        rows.push([
            (host.hostname.clone(), host.hostname.clone()),
            (host.channel.clone(), host.channel.clone()),
            (current.clone(), current),
            (declared.clone(), declared),
            (painted, raw_status),
            (compliance.clone(), compliance),
        ]);
    }
    layout_styled(&rows)
}

/// Map a STATUS label to its colored variant. FOOTGUN: `\u{2026}` also marks
/// hash-column truncation in `display_hash` - only call this on STATUS
/// labels emitted by `status_label`, never on hash columns.
fn paint_status(st: &crate::color::Stylizer, label: &str) -> String {
    use crate::color::Style;
    if label.contains('\u{2713}') {
        st.paint(Style::Green, label)
    } else if label.contains('\u{26A0}')
        || label.contains('\u{27F3}')
        || label.contains('\u{2192}')
        || label.contains('\u{2026}')
    {
        st.paint(Style::Yellow, label)
    } else if label.contains('\u{2717}') {
        st.paint(Style::Red, label)
    } else {
        label.to_string()
    }
}

fn layout_styled(rows: &[[(String, String); 6]]) -> String {
    let mut widths = [0usize; 6];
    for row in rows {
        for (i, (_render, width_src)) in row.iter().enumerate() {
            widths[i] = widths[i].max(width_src.chars().count());
        }
    }
    let mut out = String::new();
    for row in rows {
        for (i, (render, width_src)) in row.iter().enumerate() {
            if i > 0 {
                out.push_str("  ");
            }
            out.push_str(render);
            if i + 1 < row.len() {
                let pad = widths[i].saturating_sub(width_src.chars().count());
                for _ in 0..pad {
                    out.push(' ');
                }
            }
        }
        out.push('\n');
    }
    out
}

fn display_hash(h: Option<&str>, fallback: &str) -> String {
    match h {
        None => fallback.to_string(),
        Some(s) if s.chars().count() <= 14 => s.to_string(),
        Some(s) => {
            let prefix: String = s.chars().take(13).collect();
            format!("{prefix}\u{2026}")
        }
    }
}

fn status_label(
    host: &HostStatusEntry,
    now: DateTime<Utc>,
    freshness_minutes: Option<u32>,
) -> String {
    let base = base_status_label(host, now, freshness_minutes);
    // Pin is operator metadata, not a status of its own - appended as a suffix
    // so the health signal stays primary. Short-prefix to keep the column tidy.
    match host.pin.as_ref() {
        Some(pin) => {
            let short: String = pin.commit.chars().take(7).collect();
            format!("{base} \u{1F512}{short}")
        }
        None => base,
    }
}

fn base_status_label(
    host: &HostStatusEntry,
    now: DateTime<Utc>,
    freshness_minutes: Option<u32>,
) -> String {
    use nixfleet_proto::HostRolloutState;

    // 6-state machine per RFC-0008 §3. The pre-v0.2 conditional ladder
    // (Failed+current!=declared → "→ reverting", Healthy/Soaked → label
    // soaking, etc.) collapsed into one match arm per variant because the
    // new state machine forbids the shapes that ladder masked.
    let quarantined = host.quarantined_closure.is_some();

    // Stale check trumps in-flight labels: a host stuck in `Activating`
    // for 3 days isn't activating, it's offline. Failures + Reverted
    // remain operator-visible (they're not "in flight") so stale only
    // applies to in-flight states.
    let stale_label = host
        .last_checkin_at
        .zip(freshness_minutes)
        .and_then(|(last, window)| {
            let age = now.signed_duration_since(last);
            let stale_threshold = chrono::Duration::minutes(i64::from(window) * 2);
            (age > stale_threshold).then(|| format!("\u{26A0} stale ({})", format_age(age)))
        });

    if let Some(state) = host.rollout_state {
        if state.is_in_flight()
            && let Some(label) = stale_label
        {
            return label;
        }
        return match state {
            HostRolloutState::Pending => "\u{2192} in progress".to_string(),
            HostRolloutState::Activating => "\u{2192} activating".to_string(),
            HostRolloutState::Deferred => {
                // Activation staged at bootloader; live switch deferred
                // because a critical component cannot be live-swapped.
                // Operator action: reboot the host. LIFT #1's heartbeat
                // synthesis then transitions Deferred → Soaking.
                "\u{25B2} pending reboot".to_string()
            }
            HostRolloutState::Soaking => {
                // Probe failures during soak are operator-visible even
                // though the state itself is non-terminal.
                if host.outstanding_health_failures > 0 {
                    "\u{26A0} probes failing".to_string()
                } else {
                    "\u{2192} soaking".to_string()
                }
            }
            HostRolloutState::Converged => "\u{2713} converged".to_string(),
            HostRolloutState::Failed => {
                if quarantined {
                    "\u{2717} failed - channel halted, push fix".to_string()
                } else {
                    "\u{2717} failed".to_string()
                }
            }
            HostRolloutState::Reverted => {
                if quarantined {
                    "\u{2717} reverted - channel halted, push fix".to_string()
                } else {
                    "\u{2717} reverted".to_string()
                }
            }
        };
    }

    // No rollout_state recorded: fall through to the "did the closure
    // match anyway?" + freshness ladder. Quarantine still surfaces; the
    // pending-reboot hint stays as an operator carve-out.
    if quarantined {
        return "\u{2717} quarantined - channel halted, push fix".to_string();
    }
    if host.pending_reboot {
        return "\u{27F3} pending reboot".to_string();
    }
    if host.converged {
        return "\u{2713} converged".to_string();
    }

    if host.last_checkin_at.is_none() {
        return "\u{2717} never".to_string();
    }
    if let Some(label) = stale_label {
        return label;
    }
    "\u{2192} in progress".to_string()
}

fn format_age(d: chrono::Duration) -> String {
    let total_seconds = d.num_seconds().max(0);
    if total_seconds >= 86400 {
        format!("{}d", total_seconds / 86400)
    } else if total_seconds >= 3600 {
        format!("{}h", total_seconds / 3600)
    } else {
        format!("{}m", total_seconds / 60)
    }
}

fn compliance_label(host: &HostStatusEntry) -> String {
    // Compliance + runtime-gate + health failures all surface as one
    // "outstanding" number; drill-down lives in the dashboard / JSON.
    let total = host.outstanding_compliance_failures
        + host.outstanding_runtime_gate_errors
        + host.outstanding_health_failures;
    format!("{total} outstanding")
}

/// Render `nixfleet rollout hosts`: wave-major listing of per-host
/// dispatch state. Open dispatches show `<open>` in TERMINAL.
pub fn render_hosts_table(rollout: &RolloutHosts) -> String {
    let mut rows: Vec<[String; 5]> = Vec::with_capacity(rollout.hosts.len() + 1);
    rows.push([
        "WAVE".into(),
        "HOST".into(),
        "DISPATCHED".into(),
        "TERMINAL".into(),
        "AT".into(),
    ]);
    for h in &rollout.hosts {
        rows.push([
            h.wave.to_string(),
            h.host.clone(),
            short_ts(&h.dispatched_at),
            h.terminal_state.clone().unwrap_or_else(|| "<open>".into()),
            h.terminal_at.as_deref().map(short_ts).unwrap_or_default(),
        ]);
    }

    let mut widths = [0usize; 5];
    for row in &rows {
        for (i, col) in row.iter().enumerate() {
            widths[i] = widths[i].max(col.chars().count());
        }
    }

    let mut out = format!("rollout {}\n", rollout.rollout_id);
    for row in &rows {
        for (i, col) in row.iter().enumerate() {
            if i > 0 {
                out.push_str("  ");
            }
            out.push_str(col);
            if i + 1 < row.len() {
                let pad = widths[i].saturating_sub(col.chars().count());
                for _ in 0..pad {
                    out.push(' ');
                }
            }
        }
        out.push('\n');
    }
    out
}

/// "2026-05-05T12:34:56.789Z" -> "2026-05-05 12:34:56" (denser column).
/// Falls back to the original on parse fail so malformed rows surface.
fn short_ts(rfc3339: &str) -> String {
    DateTime::parse_from_rfc3339(rfc3339)
        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|_| rfc3339.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fixture_host(
        hostname: &str,
        channel: &str,
        converged: bool,
        last_checkin_min_ago: Option<i64>,
        outstanding: usize,
    ) -> HostStatusEntry {
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        HostStatusEntry {
            hostname: hostname.into(),
            channel: channel.into(),
            declared_closure_hash: Some("aaaaaaaaaaaaaaaaaaaa".into()),
            current_closure_hash: last_checkin_min_ago.map(|_| "bbbbbbbbbbbbbbbbbbbb".to_string()),
            pending_closure_hash: None,
            last_checkin_at: last_checkin_min_ago.map(|m| now - chrono::Duration::minutes(m)),
            last_rollout_id: None,
            converged,
            outstanding_compliance_failures: outstanding,
            outstanding_runtime_gate_errors: 0,
            verified_event_count: 0,
            last_uptime_secs: None,
            rollout_state: None,
            pending_reboot: false,
            quarantined_closure: None,
            pin: None,
            outstanding_health_failures: 0,
        }
    }

    #[test]
    fn renders_three_status_classes() {
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        let inputs = StatusInputs {
            now,
            hosts: vec![
                fixture_host("host-05", "stable", true, Some(0), 0),
                fixture_host("host-01", "stable", false, None, 0),
                fixture_host("host-02", "stable", false, Some(60 * 24 * 3), 2),
            ],
            channel_freshness: BTreeMap::from([("stable".to_string(), 180)]),
        };
        let out = render_status_table(&inputs);
        assert!(out.contains("\u{2713} converged"), "no converged: {out}");
        assert!(out.contains("\u{2717} never"), "no never: {out}");
        assert!(out.contains("\u{26A0} stale (3d)"), "no stale: {out}");
        assert!(out.contains("HOST"));
        assert!(out.contains("0 outstanding"));
        assert!(out.contains("2 outstanding"));
    }

    #[test]
    fn long_hashes_truncate_with_ellipsis() {
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        let mut h = fixture_host("a", "stable", true, Some(0), 0);
        h.declared_closure_hash = Some("0123456789abcdef0123456789abcdef".into());
        let inputs = StatusInputs {
            now,
            hosts: vec![h],
            channel_freshness: BTreeMap::new(),
        };
        let out = render_status_table(&inputs);
        assert!(
            out.contains("0123456789abc\u{2026}"),
            "no truncation: {out}"
        );
    }

    #[test]
    fn missing_freshness_window_skips_staleness_check() {
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        let inputs = StatusInputs {
            now,
            hosts: vec![fixture_host("a", "stable", false, Some(60 * 24 * 7), 0)],
            channel_freshness: BTreeMap::new(),
        };
        let out = render_status_table(&inputs);
        assert!(
            out.contains("\u{2192} in progress"),
            "fell through to in-progress without a window: {out}"
        );
        assert!(
            !out.contains("stale"),
            "shouldn't be stale without window: {out}"
        );
    }

    /// Priority contract: quarantined (CI-side fix) ranks above
    /// pending-reboot (operator reboot).
    #[test]
    fn quarantined_renders_above_pending_reboot_priority() {
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        let mut h = fixture_host("a", "stable", false, Some(1), 0);
        h.quarantined_closure = Some("broken-closure-h1".into());
        h.pending_reboot = true;
        let inputs = StatusInputs {
            now,
            hosts: vec![h],
            channel_freshness: BTreeMap::from([("stable".to_string(), 180)]),
        };
        let out = render_status_table(&inputs);
        assert!(
            out.contains("\u{2717} quarantined"),
            "expected quarantined label: {out}",
        );
        assert!(
            !out.contains("pending reboot"),
            "quarantined must out-rank pending-reboot: {out}",
        );
    }

    #[test]
    fn health_failures_roll_into_outstanding_count() {
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        let mut h = fixture_host("a", "stable", true, Some(0), 1);
        h.outstanding_runtime_gate_errors = 1;
        h.outstanding_health_failures = 2;
        let inputs = StatusInputs {
            now,
            hosts: vec![h],
            channel_freshness: BTreeMap::from([("stable".to_string(), 180)]),
        };
        let out = render_status_table(&inputs);
        assert!(
            out.contains("4 outstanding"),
            "expected combined count: {out}"
        );
    }

    #[test]
    fn pin_appends_to_converged_label() {
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        let mut h = fixture_host("a", "stable", true, Some(0), 0);
        h.pin = Some(nixfleet_proto::Pin {
            commit: "abc12345-deadbeef".into(),
            reason: "investigating CVE".into(),
            expires_at: None,
        });
        let inputs = StatusInputs {
            now,
            hosts: vec![h],
            channel_freshness: BTreeMap::from([("stable".to_string(), 180)]),
        };
        let out = render_status_table(&inputs);
        assert!(
            out.contains("\u{2713} converged"),
            "must keep converged: {out}"
        );
        assert!(
            out.contains("\u{1F512}abc1234"),
            "must show 7-char pin prefix: {out}"
        );
        assert!(
            !out.contains("abc12345"),
            "8th char must be truncated: {out}"
        );
    }

    /// Pin info stays visible on failure paths so operators see "supposed
    /// to be on commit X, and it's failed".
    #[test]
    fn pin_appends_to_failed_label_too() {
        use nixfleet_proto::HostRolloutState;
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        let mut h = fixture_host("a", "stable", false, Some(1), 0);
        h.rollout_state = Some(HostRolloutState::Failed);
        // Pin the host to the bad SHA so it shows "✗ failed" (on declared)
        // rather than "→ reverting" (off declared).
        h.current_closure_hash = h.declared_closure_hash.clone();
        h.pin = Some(nixfleet_proto::Pin {
            commit: "frozen1".into(),
            reason: "Q2 audit".into(),
            expires_at: None,
        });
        let inputs = StatusInputs {
            now,
            hosts: vec![h],
            channel_freshness: BTreeMap::from([("stable".to_string(), 180)]),
        };
        let out = render_status_table(&inputs);
        assert!(out.contains("\u{2717} failed"));
        assert!(out.contains("\u{1F512}frozen1"));
    }

    #[test]
    fn pending_reboot_renders_distinctly_when_not_converged() {
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        let mut h = fixture_host("a", "stable", false, Some(1), 0);
        h.pending_reboot = true;
        let inputs = StatusInputs {
            now,
            hosts: vec![h],
            channel_freshness: BTreeMap::from([("stable".to_string(), 180)]),
        };
        let out = render_status_table(&inputs);
        assert!(
            out.contains("\u{27F3} pending reboot"),
            "expected pending-reboot label: {out}",
        );
        assert!(
            !out.contains("converged"),
            "should not show converged: {out}"
        );
        assert!(
            !out.contains("in progress"),
            "pending-reboot is louder than in-progress: {out}"
        );
    }

    #[test]
    fn rollout_state_failed_takes_priority_over_converged() {
        use nixfleet_proto::HostRolloutState;
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        let mut h = fixture_host("a", "stable", true, Some(1), 0);
        h.rollout_state = Some(HostRolloutState::Failed);
        // Force current == declared so the "on declared" arm renders ("failed").
        // The "off declared" arm ("reverting") is exercised separately.
        h.current_closure_hash = h.declared_closure_hash.clone();
        let inputs = StatusInputs {
            now,
            hosts: vec![h],
            channel_freshness: BTreeMap::from([("stable".to_string(), 180)]),
        };
        let out = render_status_table(&inputs);
        assert!(
            out.contains("\u{2717} failed"),
            "expected failed label: {out}"
        );
        assert!(
            !out.contains("converged"),
            "should not show converged: {out}"
        );
    }

    /// Issue (state-machine clarity): Failed with the host already off the
    /// declared (bad) SHA means the agent has rolled back -- CP just hasn't
    /// transitioned to Reverted yet. Render as "→ reverting" so the operator
    /// knows recovery is in flight rather than seeing a stale "✗ failed".
    #[test]
    fn rollout_state_failed_renders_as_failed_under_new_state_machine() {
        // RFC-0008 §3 forbids the v0.1 "Failed + current != declared →
        // → reverting" shape. The agent owns its own rollback; CP sees
        // either Failed or Reverted as terminal-but-stuck. The label
        // collapsed accordingly in Phase 7h.
        use nixfleet_proto::HostRolloutState;
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        let mut h = fixture_host("a", "stable", false, Some(1), 0);
        h.rollout_state = Some(HostRolloutState::Failed);
        let inputs = StatusInputs {
            now,
            hosts: vec![h],
            channel_freshness: BTreeMap::from([("stable".to_string(), 180)]),
        };
        let out = render_status_table(&inputs);
        assert!(
            out.contains("\u{2717} failed"),
            "Failed renders as '✗ failed' regardless of agent's current closure: {out}",
        );
        assert!(
            !out.contains("\u{2192} reverting"),
            "v0.1 '→ reverting' transition label is gone: {out}",
        );
    }

    #[test]
    fn soaked_with_failing_probes_does_not_render_as_converged() {
        use nixfleet_proto::HostRolloutState;
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        let mut h = fixture_host("a", "stable", true, Some(1), 0);
        h.rollout_state = Some(HostRolloutState::Soaking);
        h.outstanding_health_failures = 1;
        let inputs = StatusInputs {
            now,
            hosts: vec![h],
            channel_freshness: BTreeMap::from([("stable".to_string(), 180)]),
        };
        let out = render_status_table(&inputs);
        assert!(
            out.contains("\u{26A0} probes failing"),
            "expected probes-failing label: {out}",
        );
        assert!(
            !out.contains("\u{2713} converged"),
            "should not show converged when probes are failing: {out}",
        );
    }

    #[test]
    fn healthy_with_failing_probes_does_not_render_as_converged() {
        use nixfleet_proto::HostRolloutState;
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        // Pre-soak window: closure activated, host still in Healthy, probes
        // already failing. Same misleading-display bug as the Soaked case.
        let mut h = fixture_host("a", "stable", true, Some(1), 0);
        h.rollout_state = Some(HostRolloutState::Soaking);
        h.outstanding_health_failures = 1;
        let inputs = StatusInputs {
            now,
            hosts: vec![h],
            channel_freshness: BTreeMap::from([("stable".to_string(), 180)]),
        };
        let out = render_status_table(&inputs);
        assert!(
            out.contains("\u{26A0} probes failing"),
            "expected probes-failing label: {out}",
        );
        assert!(
            !out.contains("\u{2713} converged"),
            "should not show converged when probes are failing: {out}",
        );
    }

    #[test]
    fn soaking_with_no_failing_probes_renders_soaking_not_converged() {
        // A host in Soaking has activated cleanly but the soak window
        // hasn't elapsed yet — distinct from Converged.
        use nixfleet_proto::HostRolloutState;
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        let mut h = fixture_host("a", "stable", true, Some(1), 0);
        h.rollout_state = Some(HostRolloutState::Soaking);
        let inputs = StatusInputs {
            now,
            hosts: vec![h],
            channel_freshness: BTreeMap::from([("stable".to_string(), 180)]),
        };
        let out = render_status_table(&inputs);
        assert!(
            out.contains("\u{2192} soaking"),
            "soaking+passing must render as '→ soaking': {out}",
        );
        assert!(
            !out.contains("\u{2713} converged"),
            "must not collapse soaking into converged: {out}",
        );
    }

    /// Companion: Healthy + passing probes is the brief window between
    /// confirm and Soaked. Show "→ healthy" so the operator sees the host
    /// is still progressing through the rollout, not done.
    #[test]
    fn healthy_with_passing_probes_renders_soaking_not_converged() {
        // Healthy is the soak window between confirm and Soaked. Label it as
        // "→ soaking" so the operator sees rollout progress accurately --
        // "healthy" reads as a terminal state and lies about the transient
        // nature of this phase.
        use nixfleet_proto::HostRolloutState;
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        let mut h = fixture_host("a", "stable", true, Some(1), 0);
        h.rollout_state = Some(HostRolloutState::Soaking);
        let inputs = StatusInputs {
            now,
            hosts: vec![h],
            channel_freshness: BTreeMap::from([("stable".to_string(), 180)]),
        };
        let out = render_status_table(&inputs);
        assert!(
            !out.contains("\u{2713} converged"),
            "must not collapse healthy into converged: {out}",
        );
        assert!(
            out.contains("\u{2192} soaking"),
            "Healthy must render as '→ soaking': {out}",
        );
        assert!(
            !out.contains("\u{2192} healthy"),
            "must not surface the raw '→ healthy' label: {out}",
        );
    }

    /// Issue #5: reverted + quarantined surface together so the operator
    /// sees the channel-halt actionability ("push a new closure") rather
    /// than just the failure label.
    #[test]
    fn reverted_with_quarantine_appends_channel_halt_hint() {
        use nixfleet_proto::HostRolloutState;
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        let mut h = fixture_host("a", "stable", false, Some(1), 0);
        h.rollout_state = Some(HostRolloutState::Reverted);
        h.quarantined_closure = Some("bad-sha".to_string());
        let inputs = StatusInputs {
            now,
            hosts: vec![h],
            channel_freshness: BTreeMap::from([("stable".to_string(), 180)]),
        };
        let out = render_status_table(&inputs);
        assert!(
            out.contains("reverted") && out.contains("channel halted"),
            "must surface channel-halt hint alongside reverted: {out}",
        );
    }

    /// Reverted without quarantine keeps the original label.
    #[test]
    fn reverted_without_quarantine_stays_plain() {
        use nixfleet_proto::HostRolloutState;
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        let mut h = fixture_host("a", "stable", false, Some(1), 0);
        h.rollout_state = Some(HostRolloutState::Reverted);
        let inputs = StatusInputs {
            now,
            hosts: vec![h],
            channel_freshness: BTreeMap::from([("stable".to_string(), 180)]),
        };
        let out = render_status_table(&inputs);
        assert!(
            out.contains("reverted") && !out.contains("channel halted"),
            "no quarantine -> no halt hint: {out}",
        );
    }

    /// Quarantined-only (no rollout-state failure yet) carries the hint too.
    #[test]
    fn quarantined_label_includes_channel_halt_hint() {
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        let mut h = fixture_host("a", "stable", false, Some(1), 0);
        h.quarantined_closure = Some("bad-sha".to_string());
        let inputs = StatusInputs {
            now,
            hosts: vec![h],
            channel_freshness: BTreeMap::from([("stable".to_string(), 180)]),
        };
        let out = render_status_table(&inputs);
        assert!(
            out.contains("quarantined") && out.contains("channel halted"),
            "quarantined-only label must include halt hint: {out}",
        );
    }

    /// Sanity: Converged state still renders as converged (this is the
    /// terminal state where the green check is genuinely earned).
    #[test]
    fn converged_state_renders_converged() {
        use nixfleet_proto::HostRolloutState;
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        let mut h = fixture_host("a", "stable", true, Some(1), 0);
        h.rollout_state = Some(HostRolloutState::Converged);
        let inputs = StatusInputs {
            now,
            hosts: vec![h],
            channel_freshness: BTreeMap::from([("stable".to_string(), 180)]),
        };
        let out = render_status_table(&inputs);
        assert!(
            out.contains("\u{2713} converged"),
            "Converged state must render as converged: {out}",
        );
    }

    #[test]
    fn rollout_state_in_flight_renders_active_state() {
        use nixfleet_proto::HostRolloutState;
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        let mut h = fixture_host("a", "stable", false, Some(1), 0);
        h.rollout_state = Some(HostRolloutState::Activating);
        let inputs = StatusInputs {
            now,
            hosts: vec![h],
            channel_freshness: BTreeMap::from([("stable".to_string(), 180)]),
        };
        let out = render_status_table(&inputs);
        assert!(
            out.contains("\u{2192} activating"),
            "expected activating: {out}"
        );
    }

    #[test]
    fn rollout_state_soaking_renders_in_flight_not_converged() {
        // RFC-0008 §3 collapsed Soaked into Soaking and made Converged
        // the sole terminal-for-ordering state. Soaking must render as
        // in-flight (→ soaking), not as ✓.
        use nixfleet_proto::HostRolloutState;
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        let mut h = fixture_host("a", "stable", false, Some(1), 0);
        h.rollout_state = Some(HostRolloutState::Soaking);
        let inputs = StatusInputs {
            now,
            hosts: vec![h],
            channel_freshness: BTreeMap::from([("stable".to_string(), 180)]),
        };
        let out = render_status_table(&inputs);
        assert!(
            out.contains("\u{2192} soaking"),
            "Soaking must render in-flight: {out}",
        );
        assert!(
            !out.contains("\u{2713}"),
            "Soaking is not terminal-for-ordering in v0.2: {out}",
        );
    }

    #[test]
    fn rollout_state_pending_renders_in_progress() {
        // v0.1 Queued + Dispatched + ConfirmWindow all collapsed into
        // Pending. The label collapsed with them.
        use nixfleet_proto::HostRolloutState;
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        let mut h = fixture_host("a", "stable", false, Some(1), 0);
        h.rollout_state = Some(HostRolloutState::Pending);
        let inputs = StatusInputs {
            now,
            hosts: vec![h],
            channel_freshness: BTreeMap::from([("stable".to_string(), 180)]),
        };
        let out = render_status_table(&inputs);
        assert!(
            out.contains("\u{2192} in progress"),
            "Pending must render '→ in progress': {out}",
        );
    }

    fn host_entry(
        host: &str,
        wave: u32,
        terminal: Option<&str>,
    ) -> nixfleet_proto::RolloutHostEntry {
        nixfleet_proto::RolloutHostEntry {
            host: host.into(),
            channel: "stable".into(),
            wave,
            target_closure_hash: "system-r1".into(),
            target_channel_ref: "stable@trace1".into(),
            dispatched_at: "2026-05-05T12:00:00Z".into(),
            terminal_state: terminal.map(String::from),
            terminal_at: terminal.map(|_| "2026-05-05T12:30:00Z".into()),
        }
    }

    #[test]
    fn render_hosts_table_shows_open_dispatches_distinctly() {
        let rollout = RolloutHosts {
            rollout_id: "stable@trace1".into(),
            hosts: vec![
                host_entry("host-05", 0, Some("converged")),
                host_entry("host-01", 1, None),
            ],
        };
        let out = render_hosts_table(&rollout);
        assert!(
            out.contains("rollout stable@trace1"),
            "missing header: {out}"
        );
        assert!(out.contains("WAVE"), "missing column header: {out}");
        assert!(out.contains("converged"), "missing terminal state: {out}");
        assert!(out.contains("<open>"), "missing open marker: {out}");
        assert!(
            out.contains("2026-05-05 12:00:00"),
            "timestamp not shortened: {out}"
        );
    }

    #[test]
    fn stale_checkin_overrides_in_flight_state() {
        use nixfleet_proto::HostRolloutState;
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        let mut h = fixture_host("a", "stable", false, Some(60 * 24 * 3), 0);
        h.rollout_state = Some(HostRolloutState::Activating);
        let inputs = StatusInputs {
            now,
            hosts: vec![h],
            channel_freshness: BTreeMap::from([("stable".to_string(), 180)]),
        };
        let out = render_status_table(&inputs);
        assert!(
            out.contains("\u{26A0} stale"),
            "stale should win over in-flight Activating: {out}"
        );
    }

    /// Compile-time guard for the `run_status(cfg, json, color)` signature.
    #[test]
    fn run_status_json_branch_compiles() {
        fn _typecheck(cfg: &crate::ResolvedClientConfig) {
            let _fut = crate::run_status(cfg, true, false);
        }
    }

    #[test]
    fn color_render_preserves_column_widths() {
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();
        let inputs = StatusInputs {
            now,
            hosts: vec![
                fixture_host("a", "stable", true, Some(0), 0),
                fixture_host("verylonghostname", "stable", false, None, 0),
            ],
            channel_freshness: BTreeMap::from([("stable".to_string(), 180)]),
        };
        let plain = render_status_table(&inputs);
        let painted = render_status_table_with_color(&inputs, true);
        assert_eq!(plain.lines().count(), painted.lines().count());
        assert!(painted.contains("\x1b["), "expected ANSI in painted output");
        assert!(!plain.contains("\x1b["), "plain must not have ANSI escapes");
        // Strip ANSI then compare line-by-line modulo trailing whitespace
        // (column padding can collapse differently across renderers).
        let strip_ansi = |s: &str| -> String {
            let mut out = String::new();
            let mut chars = s.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\x1b' && chars.peek() == Some(&'[') {
                    chars.next();
                    while let Some(&c2) = chars.peek() {
                        chars.next();
                        if c2 == 'm' {
                            break;
                        }
                    }
                } else {
                    out.push(c);
                }
            }
            out
        };
        let painted_plain: Vec<&str> = painted.lines().collect();
        let stripped: Vec<String> = painted_plain.iter().map(|l| strip_ansi(l)).collect();
        let plain_lines: Vec<&str> = plain.lines().collect();
        for (a, b) in stripped.iter().zip(plain_lines.iter()) {
            assert_eq!(
                a.trim_end(),
                b.trim_end(),
                "row mismatch:\nstripped: {a}\nplain:    {b}"
            );
        }
    }

    #[test]
    fn paint_status_glyph_color_mapping_locks_in() {
        use nixfleet_proto::HostRolloutState;
        let now = Utc.with_ymd_and_hms(2026, 5, 5, 0, 0, 0).unwrap();

        let inputs = StatusInputs {
            now,
            hosts: vec![fixture_host("a", "stable", true, Some(0), 0)],
            channel_freshness: BTreeMap::from([("stable".to_string(), 180)]),
        };
        let painted = render_status_table_with_color(&inputs, true);
        assert!(
            painted.contains("\x1b[32m") && painted.contains("\u{2713} converged"),
            "converged should be green: {painted}",
        );

        let mut h = fixture_host("a", "stable", false, Some(1), 0);
        h.rollout_state = Some(HostRolloutState::Failed);
        // Force current == declared so the label is "✗ failed" (red), not
        // "→ reverting" (off-declared transient).
        h.current_closure_hash = h.declared_closure_hash.clone();
        let inputs = StatusInputs {
            now,
            hosts: vec![h],
            channel_freshness: BTreeMap::from([("stable".to_string(), 180)]),
        };
        let painted = render_status_table_with_color(&inputs, true);
        assert!(
            painted.contains("\x1b[31m") && painted.contains("\u{2717} failed"),
            "failed should be red: {painted}",
        );

        let inputs = StatusInputs {
            now,
            hosts: vec![fixture_host("a", "stable", false, Some(60 * 24 * 3), 0)],
            channel_freshness: BTreeMap::from([("stable".to_string(), 180)]),
        };
        let painted = render_status_table_with_color(&inputs, true);
        assert!(
            painted.contains("\x1b[33m") && painted.contains("\u{26A0} stale"),
            "stale should be yellow: {painted}",
        );
    }
}
