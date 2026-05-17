//! HTTP probe runner (RFC-0010 §3.1). `GET <url>` with `timeoutSecs`
//! wallclock budget; Pass iff response status matches `expectStatus`.
//! Error classes that count as Fail (RFC-0010 §6 uniform strict mode):
//! - missing url field
//! - network error (connect refused, DNS, TLS)
//! - timeout
//! - unexpected status

use chrono::{DateTime, Utc};

use super::{ProbeDecl, RunnerOutcome};

pub async fn run(decl: &ProbeDecl, now: DateTime<Utc>) -> RunnerOutcome {
    let Some(url) = decl.url.as_deref() else {
        return RunnerOutcome::fail(now, "http probe: url missing");
    };
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(decl.timeout_secs))
        .build()
    {
        Ok(c) => c,
        Err(err) => return RunnerOutcome::fail(now, format!("http probe: client build: {err}")),
    };
    match client.get(url).send().await {
        Ok(resp) => {
            let got = resp.status().as_u16();
            if got == decl.expect_status {
                RunnerOutcome::pass(now)
            } else {
                RunnerOutcome::fail(
                    now,
                    format!(
                        "http probe: status {got} != expected {}",
                        decl.expect_status
                    ),
                )
            }
        }
        Err(err) => RunnerOutcome::fail(now, format!("http probe: send: {err}")),
    }
}
