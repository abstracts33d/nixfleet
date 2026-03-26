use anyhow::{Context, Result};
use tracing::debug;

use crate::config::Config;
use crate::types::{DesiredGeneration, Report};

/// HTTP client for control plane communication.
pub struct Client {
    http: reqwest::Client,
    base_url: String,
}

impl Client {
    /// Create a new client configured for the control plane.
    pub fn new(config: &Config) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self {
            http,
            base_url: config.control_plane_url.trim_end_matches('/').to_string(),
        })
    }

    /// Poll the control plane for the desired generation.
    ///
    /// GET /api/v1/machines/{machine_id}/desired-generation
    pub async fn get_desired_generation(&self, machine_id: &str) -> Result<DesiredGeneration> {
        let url = format!(
            "{}/api/v1/machines/{}/desired-generation",
            self.base_url, machine_id
        );
        debug!(url, "Polling for desired generation");

        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .context("failed to reach control plane")?
            .error_for_status()
            .context("control plane returned error status")?;

        let desired: DesiredGeneration = resp
            .json()
            .await
            .context("failed to parse desired generation response")?;

        Ok(desired)
    }

    /// Report status back to the control plane.
    ///
    /// POST /api/v1/machines/{machine_id}/report
    pub async fn post_report(&self, report: &Report) -> Result<()> {
        let url = format!(
            "{}/api/v1/machines/{}/report",
            self.base_url, report.machine_id
        );
        debug!(url, "Sending report");

        self.http
            .post(&url)
            .json(report)
            .send()
            .await
            .context("failed to send report")?
            .error_for_status()
            .context("control plane rejected report")?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::time::Duration;

    fn test_config(url: &str) -> Config {
        Config {
            control_plane_url: url.to_string(),
            machine_id: "krach".to_string(),
            poll_interval: Duration::from_secs(300),
            cache_url: None,
            db_path: ":memory:".to_string(),
            dry_run: false,
        }
    }

    #[test]
    fn test_api_url_construction_desired_generation() {
        let base = "https://fleet.example.com";
        let machine_id = "krach";
        let url = format!("{}/api/v1/machines/{}/desired-generation", base, machine_id);
        assert_eq!(
            url,
            "https://fleet.example.com/api/v1/machines/krach/desired-generation"
        );
    }

    #[test]
    fn test_api_url_construction_report() {
        let base = "https://fleet.example.com";
        let machine_id = "krach";
        let url = format!("{}/api/v1/machines/{}/report", base, machine_id);
        assert_eq!(
            url,
            "https://fleet.example.com/api/v1/machines/krach/report"
        );
    }

    #[test]
    fn test_client_new_strips_trailing_slash() {
        // URL with trailing slash should be normalized
        let config = test_config("https://fleet.example.com/");
        let client = Client::new(&config).unwrap();
        assert_eq!(client.base_url, "https://fleet.example.com");
    }

    #[test]
    fn test_client_new_no_trailing_slash() {
        let config = test_config("https://fleet.example.com");
        let client = Client::new(&config).unwrap();
        assert_eq!(client.base_url, "https://fleet.example.com");
    }

    #[test]
    fn test_client_new_multiple_trailing_slashes() {
        // trim_end_matches only removes one trailing slash
        let config = test_config("https://fleet.example.com///");
        let client = Client::new(&config).unwrap();
        // trim_end_matches('/') removes all trailing slashes
        assert_eq!(client.base_url, "https://fleet.example.com");
    }

    #[test]
    fn test_url_construction_with_different_machine_ids() {
        let base = "https://fleet.example.com";
        for machine_id in &["krach", "ohm", "aether", "lab"] {
            let url = format!("{}/api/v1/machines/{}/desired-generation", base, machine_id);
            assert!(url.contains(machine_id));
            assert!(url.starts_with("https://fleet.example.com/api/v1/machines/"));
            assert!(url.ends_with("/desired-generation"));
        }
    }

    #[test]
    fn test_url_does_not_double_slash_with_clean_base() {
        let base = "https://fleet.example.com";
        let url = format!("{}/api/v1/machines/{}/desired-generation", base, "krach");
        assert!(!url.contains("//api"));
    }
}
