//! mTLS cert renewal worker. Reads the cert at `cfg.client_cert` on a
//! timer, computes the remaining-validity fraction, and POSTs
//! `/v1/agent/renew` when below `cfg.renewal_threshold_fraction`. The
//! CSR is signed by the host's SSH ed25519 key (same key as
//! enrollment per RFC-0003 §2), so the renewed cert's pubkey is
//! unchanged.
//!
//! The renewed cert lands at the same on-disk path atomically via
//! `enrollment::renew`. Running workers (heartbeat, longpoll,
//! manifest_poll) keep their existing in-memory mTLS clients — those
//! clients still hold the OLD cert bytes. Production: the agent's
//! systemd unit has `Restart=always RestartSec=30`, so any restart
//! event (next operator-driven nixos-rebuild, the cert-near-expiry
//! handover, or any crash) loads the freshly-written cert into the
//! workers' clients. The renewal worker exists to keep the on-disk
//! cert from ever expiring; it does NOT hot-swap the running clients'
//! identities.
//!
//! Disabled when `cfg.renewal_threshold_fraction.is_none()` — the
//! worker logs a single info line and exits. Use this in tests where
//! the cert is fixture-baked and renewal would be noise.

use std::time::Duration;

use nixfleet_proto::clock::ClockHandle;
use tokio::task::JoinHandle;

use super::super::{AgentConfig, ShutdownToken};

/// Cadence at which the worker re-reads the cert + checks the
/// threshold. 60s matches the heartbeat ticker; the renewal decision
/// is cheap (parse PEM, compute fraction) so amortised cost is
/// trivial. Independent of the renewal HTTP cost because that only
/// fires when the threshold trips.
const CHECK_INTERVAL: Duration = Duration::from_secs(60);

const ERROR_BACKOFF: Duration = Duration::from_secs(30);

pub fn spawn(
    cfg: AgentConfig,
    clock: ClockHandle,
    shutdown: ShutdownToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut shutdown_rx = shutdown.into_inner();
        let Some(threshold) = cfg.renewal_threshold_fraction else {
            tracing::info!(
                target: "agent_cert_renewal",
                "renewal_threshold_fraction unset — cert renewal worker disabled",
            );
            // Stay alive on the shutdown channel so the runtime's
            // shutdown handshake doesn't see a missing receiver.
            let _ = shutdown_rx.await;
            return;
        };
        if !(0.0 < threshold && threshold < 1.0) {
            tracing::error!(
                target: "agent_cert_renewal",
                threshold,
                "renewal_threshold_fraction must be strictly between 0 and 1 — worker exiting",
            );
            let _ = shutdown_rx.await;
            return;
        }
        let Some(cert_path) = cfg.client_cert.clone() else {
            tracing::info!(
                target: "agent_cert_renewal",
                "client_cert unset — renewal worker has nothing to renew; exiting",
            );
            let _ = shutdown_rx.await;
            return;
        };

        let client = match crate::comms::build_client(
            cfg.ca_cert.as_deref(),
            cfg.client_cert.as_deref(),
            cfg.client_key.as_deref(),
        ) {
            Ok(c) => c,
            Err(err) => {
                tracing::error!(
                    target: "agent_cert_renewal",
                    error = %err,
                    "failed to build mTLS HTTP client; worker exits",
                );
                let _ = shutdown_rx.await;
                return;
            }
        };

        let mut ticker = tokio::time::interval(CHECK_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => {
                    tracing::info!(
                        target: "shutdown",
                        task = "agent_cert_renewal",
                        "task shut down",
                    );
                    return;
                }
                _ = ticker.tick() => {
                    if let Err(err) = maybe_renew_once(
                        &client,
                        &cfg,
                        &clock,
                        &cert_path,
                        threshold,
                    ).await {
                        tracing::warn!(
                            target: "agent_cert_renewal",
                            error = %err,
                            "renewal check failed; backing off",
                        );
                        tokio::time::sleep(ERROR_BACKOFF).await;
                    }
                }
            }
        }
    })
}

async fn maybe_renew_once(
    client: &reqwest::Client,
    cfg: &AgentConfig,
    clock: &ClockHandle,
    cert_path: &std::path::Path,
    threshold: f64,
) -> anyhow::Result<()> {
    let (remaining, not_after) =
        crate::enrollment::cert_remaining_fraction(cert_path, clock.now())?;
    if remaining >= threshold {
        return Ok(());
    }
    tracing::info!(
        target: "agent_cert_renewal",
        remaining,
        threshold,
        not_after = %not_after.to_rfc3339(),
        "cert remaining fraction below threshold; renewing",
    );
    crate::enrollment::renew(
        client,
        &cfg.control_plane_url,
        &cfg.machine_id,
        cert_path,
        &cfg.ssh_host_key_file,
    )
    .await?;
    tracing::info!(
        target: "agent_cert_renewal",
        cert = %cert_path.display(),
        "cert renewed — file rewritten; running workers continue with prior in-memory client \
         until next agent restart picks up the new cert",
    );
    Ok(())
}
