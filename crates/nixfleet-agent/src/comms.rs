//! mTLS HTTP client to the control plane: typed checkin/confirm/report calls.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use nixfleet_proto::agent_wire::{
    CheckinRequest, CheckinResponse, ConfirmRequest, ReportEvent, ReportRequest, ReportResponse,
    PROTOCOL_MAJOR_VERSION, PROTOCOL_VERSION_HEADER,
};
use reqwest::{Certificate, Client, Identity, StatusCode};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// TLS-only mode (None cert/key) supported but production always wires both.
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

/// 204 → Acknowledged; 410 → Cancelled (agent must rollback); else Other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmOutcome {
    Acknowledged,
    Cancelled,
    Other,
}

/// `endpoint` is the wire-carried `target.activate.confirm_endpoint` from
/// the dispatch reply. Required, not optional — the CP must always set it
/// for any target the agent will confirm; agents refuse to confirm against
/// a target with no activate block.
pub async fn confirm(
    client: &Client,
    cp_url: &str,
    endpoint: &str,
    req: &ConfirmRequest,
) -> Result<ConfirmOutcome> {
    let url = format!(
        "{}{}",
        cp_url.trim_end_matches('/'),
        if endpoint.starts_with('/') {
            endpoint.to_string()
        } else {
            format!("/{endpoint}")
        }
    );
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

/// Best-effort by contract: telemetry must never crash the activation loop.
pub trait Reporter: Send + Sync {
    fn post_report(
        &self,
        rollout: Option<&str>,
        event: ReportEvent,
    ) -> impl std::future::Future<Output = ()> + Send;
}

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

    pub fn replace_client(&mut self, client: Client) {
        self.client = client;
    }

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

