//! HTTP client wiring for talking to the control plane.
//!
//! Builds an mTLS `reqwest::Client` from the operator-supplied PEM
//! paths. Provides typed `checkin` and `report` calls that round-
//! trip the wire types defined in `nixfleet_proto::agent_wire`.
//!
//! Two traits abstract the CP-bound side-effects so the dispatch
//! handlers in `dispatch.rs` can be unit-tested with capturing fakes:
//!
//! - [`Reporter`] — wraps `post_report`'s capture of `(client, cp_url,
//!   hostname, agent_version)`. Production impl: [`ReqwestReporter`].
//! - [`Confirmer`] — wraps `/v1/agent/confirm` POSTs. Production
//!   impl: [`ReqwestConfirmer`]. Used by the activation
//!   `confirm_target` helper and the boot-recovery path; concrete
//!   callers are still on the free-function form for now (the trait
//!   surface is wired so a future PR can swap them in without a
//!   contract change).

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use nixfleet_proto::agent_wire::{
    CheckinRequest, CheckinResponse, ConfirmRequest, ReportEvent, ReportRequest, ReportResponse,
    PROTOCOL_MAJOR_VERSION, PROTOCOL_VERSION_HEADER,
};
use reqwest::{Certificate, Client, Identity, StatusCode};

/// Connect timeout. Generous because lab is often on Tailscale and
/// the first connect after a sleep can be slow. The poll cadence
/// itself is 60s, so even ~10s connects don't compound badly.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Per-request timeout (handshake + full request lifecycle).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Construct an mTLS-enabled HTTP client. CA cert pins the CP's
/// fleet CA; the client identity is the agent's per-host cert +
/// key. TLS-only mode is supported (caller passes None for
/// `client_cert` and `client_key`); production deploys always wire
/// both.
pub fn build_client(
    ca_cert: Option<&Path>,
    client_cert: Option<&Path>,
    client_key: Option<&Path>,
) -> Result<Client> {
    let mut builder = Client::builder()
        .use_rustls_tls()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT);

    if let Some(ca_path) = ca_cert {
        let pem = std::fs::read(ca_path)
            .with_context(|| format!("read CA cert {}", ca_path.display()))?;
        let cert = Certificate::from_pem(&pem).context("parse CA cert PEM")?;
        builder = builder.add_root_certificate(cert);
    }

    if let (Some(cert), Some(key)) = (client_cert, client_key) {
        let mut pem = std::fs::read(cert)
            .with_context(|| format!("read client cert {}", cert.display()))?;
        let key_pem = std::fs::read(key)
            .with_context(|| format!("read client key {}", key.display()))?;
        pem.extend_from_slice(&key_pem);
        let identity = Identity::from_pem(&pem).context("parse client identity PEM")?;
        builder = builder.identity(identity);
    }

    builder.build().context("build reqwest client")
}

/// POST /v1/agent/checkin. Returns the typed response for the agent
/// to consume.
pub async fn checkin(
    client: &Client,
    cp_url: &str,
    req: &CheckinRequest,
) -> Result<CheckinResponse> {
    let url = format!("{}/v1/agent/checkin", cp_url.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .header(PROTOCOL_VERSION_HEADER, PROTOCOL_MAJOR_VERSION.to_string())
        .json(req)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("{url}: {status}: {body}");
    }
    resp.json::<CheckinResponse>().await.context("parse checkin response")
}

/// Outcome of POST /v1/agent/confirm. Distinguishes the three
/// cases the activation loop needs to handle differently:
/// 204 acknowledged, 410 cancelled (trigger local rollback per
/// ), other (deadline timer will sort it out).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmOutcome {
    /// 204 No Content — CP accepted the confirmation.
    Acknowledged,
    /// 410 Gone — CP says the rollout was cancelled OR the deadline
    /// already passed. Agent should run `nixos-rebuild --rollback`
    /// per
    Cancelled,
    /// Any other status code. Treated as "couldn't confirm but
    /// don't need to take immediate action" — the CP-side rollback
    /// timer will handle deadline expiry independently.
    Other,
}

/// POST /v1/agent/confirm. Called after a successful
/// `nixos-rebuild switch` to acknowledge the activation. Wire shape
/// per
pub async fn confirm(
    client: &Client,
    cp_url: &str,
    req: &ConfirmRequest,
) -> Result<ConfirmOutcome> {
    let url = format!("{}/v1/agent/confirm", cp_url.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .header(PROTOCOL_VERSION_HEADER, PROTOCOL_MAJOR_VERSION.to_string())
        .json(req)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let outcome = match resp.status() {
        StatusCode::NO_CONTENT => ConfirmOutcome::Acknowledged,
        StatusCode::GONE => ConfirmOutcome::Cancelled,
        other => {
            tracing::warn!(
                status = other.as_u16(),
                "confirm: unexpected status — treating as 'other'"
            );
            ConfirmOutcome::Other
        }
    };
    Ok(outcome)
}

/// POST /v1/agent/report. Used for out-of-band failure events
/// (verify-failed, fetch-failed, trust-error).
pub async fn report(
    client: &Client,
    cp_url: &str,
    req: &ReportRequest,
) -> Result<ReportResponse> {
    let url = format!("{}/v1/agent/report", cp_url.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .header(PROTOCOL_VERSION_HEADER, PROTOCOL_MAJOR_VERSION.to_string())
        .json(req)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("{url}: {status}: {body}");
    }
    resp.json::<ReportResponse>().await.context("parse report response")
}

/// Out-of-band event posting (the `/v1/agent/report` endpoint).
///
/// Best-effort by contract: telemetry must never crash the activation
/// loop. Implementations log on failure and return; they don't
/// propagate errors. The matching production impl is
/// [`ReqwestReporter`]; tests inject a capturing fake.
pub trait Reporter: Send + Sync {
    fn post_report(
        &self,
        rollout: Option<&str>,
        event: ReportEvent,
    ) -> impl std::future::Future<Output = ()> + Send;
}

/// Production [`Reporter`]: wraps a configured `reqwest::Client`
/// plus the constants every report needs (`cp_url`, `hostname`,
/// `agent_version`). Constructed once in `main.rs` and shared
/// across the dispatch tree.
pub struct ReqwestReporter {
    client: Client,
    cp_url: String,
    hostname: String,
    agent_version: String,
}

impl ReqwestReporter {
    pub fn new(
        client: Client,
        cp_url: impl Into<String>,
        hostname: impl Into<String>,
        agent_version: impl Into<String>,
    ) -> Self {
        Self {
            client,
            cp_url: cp_url.into(),
            hostname: hostname.into(),
            agent_version: agent_version.into(),
        }
    }

    /// Replace the inner `reqwest::Client` (called after cert renewal
    /// rebuilds the mTLS identity).
    pub fn replace_client(&mut self, client: Client) {
        self.client = client;
    }

    /// Borrow the inner client for callers that still need to thread
    /// a `&reqwest::Client` directly (checkin, confirm, manifest
    /// fetch). Trait-only callers should use the [`Reporter`]
    /// surface instead.
    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn cp_url(&self) -> &str {
        &self.cp_url
    }
}

impl Reporter for ReqwestReporter {
    async fn post_report(&self, rollout: Option<&str>, event: ReportEvent) {
        let req = ReportRequest {
            hostname: self.hostname.clone(),
            agent_version: self.agent_version.clone(),
            occurred_at: chrono::Utc::now(),
            rollout: rollout.map(String::from),
            event,
        };
        if let Err(err) = report(&self.client, &self.cp_url, &req).await {
            tracing::warn!(
                error = %err,
                hostname = %self.hostname,
                "report post failed; event is in local journal only",
            );
        }
    }
}

/// Confirmation POSTs (`/v1/agent/confirm`).
///
/// Surface is wired alongside [`Reporter`] for consistency; concrete
/// callers (`activation::confirm_target`, `recovery::decide_and_run`)
/// still take `&reqwest::Client + cp_url` for now. A future PR can
/// migrate them when the test surface starts wanting per-variant
/// confirm assertions.
pub trait Confirmer: Send + Sync {
    fn confirm(
        &self,
        req: &ConfirmRequest,
    ) -> impl std::future::Future<Output = Result<ConfirmOutcome>> + Send;
}

/// Production [`Confirmer`]: wraps `(client, cp_url)`. Borrowed via
/// `ReqwestReporter::client()` + `ReqwestReporter::cp_url()` so the
/// agent's run-loop holds a single reqwest client across the
/// reporter and confirmer surfaces.
pub struct ReqwestConfirmer<'a> {
    client: &'a Client,
    cp_url: &'a str,
}

impl<'a> ReqwestConfirmer<'a> {
    pub fn new(client: &'a Client, cp_url: &'a str) -> Self {
        Self { client, cp_url }
    }
}

impl Confirmer for ReqwestConfirmer<'_> {
    async fn confirm(&self, req: &ConfirmRequest) -> Result<ConfirmOutcome> {
        confirm(self.client, self.cp_url, req).await
    }
}
