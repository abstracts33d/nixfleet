//! Exec probe runner (RFC-0007 §3.1). Pass iff exit code 0 within
//! `timeoutSecs` wallclock. Argv runs as the agent's user; declare
//! absolute paths to avoid PATH surprises.

use chrono::{DateTime, Utc};
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

use super::{ProbeDecl, RunnerOutcome};

pub async fn run(decl: &ProbeDecl, now: DateTime<Utc>) -> RunnerOutcome {
    if decl.command.is_empty() {
        return RunnerOutcome::fail(now, "exec probe: command argv missing");
    }
    let mut cmd = Command::new(&decl.command[0]);
    cmd.args(&decl.command[1..]);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::piped());

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(err) => return RunnerOutcome::fail(now, format!("exec probe: spawn: {err}")),
    };
    match timeout(
        Duration::from_secs(decl.timeout_secs),
        child.wait_with_output(),
    )
    .await
    {
        Ok(Ok(out)) => {
            if out.status.success() {
                RunnerOutcome::pass(now)
            } else {
                let stderr_tail = String::from_utf8_lossy(&out.stderr);
                let stderr_tail = stderr_tail
                    .lines()
                    .rev()
                    .take(3)
                    .collect::<Vec<_>>()
                    .join(" / ");
                RunnerOutcome::fail(
                    now,
                    format!(
                        "exec probe: exit {:?}; stderr: {stderr_tail}",
                        out.status.code()
                    ),
                )
            }
        }
        Ok(Err(err)) => RunnerOutcome::fail(now, format!("exec probe: wait: {err}")),
        Err(_elapsed) => RunnerOutcome::fail(
            now,
            format!("exec probe: timed out after {}s", decl.timeout_secs),
        ),
    }
}
