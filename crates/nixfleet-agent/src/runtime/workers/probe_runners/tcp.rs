//! TCP probe runner (RFC-0007 §3.1). Pass iff `connect_timeout_secs`
//! TCP connect succeeds against `host:port`. `host` defaults to
//! `127.0.0.1` if absent.

use chrono::{DateTime, Utc};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;

use super::{ProbeDecl, RunnerOutcome};

pub async fn run(decl: &ProbeDecl, now: DateTime<Utc>) -> RunnerOutcome {
    let Some(port) = decl.port else {
        return RunnerOutcome::fail(now, "tcp probe: port missing");
    };
    let host = decl.host.as_deref().unwrap_or("127.0.0.1");
    let addr = format!("{host}:{port}");
    let connect = TcpStream::connect(&addr);
    match timeout(Duration::from_secs(decl.connect_timeout_secs), connect).await {
        Ok(Ok(_stream)) => RunnerOutcome::pass(now),
        Ok(Err(err)) => RunnerOutcome::fail(now, format!("tcp probe: connect {addr}: {err}")),
        Err(_elapsed) => RunnerOutcome::fail(
            now,
            format!(
                "tcp probe: connect {addr} timed out after {}s",
                decl.connect_timeout_secs
            ),
        ),
    }
}
