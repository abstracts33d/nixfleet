//! `nixfleet-release` — produces `releases/fleet.resolved.json{,.sig}`
//! by orchestrating build → inject closureHash → stamp meta →
//! canonicalize → sign → write → (optional) git commit + push.
//!
//! See `lib.rs` for the orchestration spec and the hook contract
//! (`$NIXFLEET_HOST`, `$NIXFLEET_PATH`, `$NIXFLEET_CLOSURE_HASH` for
//! `--push-cmd`; `$NIXFLEET_INPUT`, `$NIXFLEET_OUTPUT` for
//! `--sign-cmd`).
//!
//! Exit codes:
//!   0 — release produced (or `NoChange` when reuse_unchanged_signature
//!       is set and inputs haven't moved)
//!   1 — config / build / nix eval failure
//!   2 — push or sign hook failed
//!   3 — smoke verify failed

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use nixfleet_release::{GitPushTarget, HostsSpec, ReleaseConfig, RunOutcome};

#[derive(Parser, Debug)]
#[command(
    name = "nixfleet-release",
    about = "Produce a signed fleet.resolved.json release (CONTRACTS §I #1)"
)]
struct Cli {
    /// Hosts to release. `auto` (default) = every
    /// `nixosConfigurations.*`. `auto:exclude=foo,bar` = auto minus
    /// the listed names. Or a comma-separated explicit list.
    #[arg(long, default_value = "auto")]
    hosts: String,

    /// Path to the consumer flake. Defaults to cwd.
    #[arg(long, default_value = ".")]
    build_flake: PathBuf,

    /// Attribute path yielding the FleetResolved-shaped JSON.
    #[arg(long, default_value = ".#fleet.resolved")]
    fleet_resolved_attr: String,

    /// Optional shell command run once per built closure. Receives
    /// env: NIXFLEET_HOST, NIXFLEET_PATH, NIXFLEET_CLOSURE_HASH.
    /// Typical: `attic push fleet "$NIXFLEET_PATH"` or
    /// `nix copy --to ssh://lab "$NIXFLEET_PATH"`.
    #[arg(long, env = "NIXFLEET_PUSH_CMD")]
    push_cmd: Option<String>,

    /// Required. Shell command that signs the canonical bytes.
    /// Receives env: NIXFLEET_INPUT (file path with canonical
    /// bytes), NIXFLEET_OUTPUT (file path the hook MUST write the
    /// raw signature to). Typical: `tpm-sign "$NIXFLEET_INPUT" >
    /// "$NIXFLEET_OUTPUT"`.
    #[arg(long, env = "NIXFLEET_SIGN_CMD")]
    sign_cmd: String,

    /// Stamped into `meta.signatureAlgorithm`. Must match the
    /// algorithm of the key the sign hook uses. One of `ed25519`,
    /// `ecdsa-p256`.
    #[arg(long, default_value = "ed25519", env = "NIXFLEET_SIGNATURE_ALGORITHM")]
    signature_algorithm: String,

    /// Output directory for the release files. Created if missing.
    #[arg(long, default_value = "releases")]
    release_dir: PathBuf,

    /// Filename of the artifact. Signature is `<name>.sig`.
    #[arg(long, default_value = "fleet.resolved.json")]
    artifact_name: String,

    /// Stage + commit the release files when they change.
    #[arg(long)]
    git_commit: bool,

    /// Push HEAD to a remote/branch after committing. Format
    /// `<remote>:<branch>`, e.g. `origin:main`. Implies
    /// `--git-commit`.
    #[arg(long, value_name = "REMOTE:BRANCH")]
    git_push: Option<String>,

    /// Commit message template. Substitutions: `{sha}`,
    /// `{sha:0:8}`, `{ts}`.
    #[arg(
        long,
        default_value = "chore(ci): release {sha:0:8} [skip ci]"
    )]
    commit_template: String,

    /// Sets `git config user.name` before committing. Use when the
    /// runner has no committer identity configured.
    #[arg(long, env = "NIXFLEET_GIT_USER_NAME")]
    git_user_name: Option<String>,

    /// Sets `git config user.email` before committing.
    #[arg(long, env = "NIXFLEET_GIT_USER_EMAIL")]
    git_user_email: Option<String>,

    /// Smoke-verify the (artifact, signature) pair before
    /// publishing. Catches "we just signed bytes the verifier
    /// rejects." Structural only — re-canonicalize round-trip +
    /// schema parse + non-zero signature length. Default on.
    #[arg(long = "smoke-verify", default_value_t = true, action = clap::ArgAction::Set)]
    smoke_verify: bool,

    /// When the existing release file's closureHashes match the
    /// just-built ones, reuse its `meta.signedAt` instead of
    /// stamping a new one. Produces byte-stable releases on no-op
    /// runs (no new commit, no new signature).
    #[arg(long)]
    reuse_unchanged_signature: bool,

    /// Log format. `pretty` (default) for humans, `json` for CI
    /// log scrapers.
    #[arg(long, default_value = "pretty")]
    log_format: String,
}

fn parse_hosts_spec(spec: &str) -> Result<HostsSpec, String> {
    if spec == "auto" {
        return Ok(HostsSpec::Auto);
    }
    if let Some(rest) = spec.strip_prefix("auto:exclude=") {
        let exc: Vec<String> = rest.split(',').filter(|s| !s.is_empty()).map(String::from).collect();
        return Ok(HostsSpec::AutoExclude(exc));
    }
    let list: Vec<String> = spec.split(',').filter(|s| !s.is_empty()).map(String::from).collect();
    if list.is_empty() {
        return Err("hosts spec is empty".into());
    }
    Ok(HostsSpec::Explicit(list))
}

fn parse_git_push(s: &str) -> Result<GitPushTarget, String> {
    let (remote, branch) = s
        .split_once(':')
        .ok_or_else(|| format!("--git-push expects REMOTE:BRANCH, got {s:?}"))?;
    Ok(GitPushTarget {
        remote: remote.to_string(),
        branch: branch.to_string(),
    })
}

fn init_tracing(format: &str) {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,nixfleet_release=info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    match format {
        "json" => {
            let _ = builder.json().try_init();
        }
        _ => {
            let _ = builder.try_init();
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(&cli.log_format);

    let hosts = match parse_hosts_spec(&cli.hosts) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("nixfleet-release: --hosts: {e}");
            return ExitCode::from(1);
        }
    };

    let git_push = match cli.git_push.as_deref().map(parse_git_push) {
        Some(Ok(t)) => Some(t),
        Some(Err(e)) => {
            eprintln!("nixfleet-release: {e}");
            return ExitCode::from(1);
        }
        None => None,
    };

    let git_commit = cli.git_commit || git_push.is_some();

    let config = ReleaseConfig {
        flake_dir: cli.build_flake,
        fleet_resolved_attr: cli.fleet_resolved_attr,
        hosts,
        push_cmd: cli.push_cmd,
        sign_cmd: cli.sign_cmd,
        signature_algorithm: cli.signature_algorithm,
        release_dir: cli.release_dir,
        artifact_name: cli.artifact_name,
        git_commit,
        git_push,
        commit_template: cli.commit_template,
        git_user_name: cli.git_user_name,
        git_user_email: cli.git_user_email,
        smoke_verify: cli.smoke_verify,
        reuse_unchanged_signature: cli.reuse_unchanged_signature,
    };

    match nixfleet_release::run(&config) {
        Ok(RunOutcome::Released { commit_sha, hosts }) => {
            tracing::info!(
                hosts = hosts.len(),
                commit = commit_sha.as_deref().unwrap_or("(none)"),
                "release ok"
            );
            ExitCode::SUCCESS
        }
        Ok(RunOutcome::NoChange) => {
            tracing::info!("no release change");
            ExitCode::SUCCESS
        }
        Err(err) => {
            // Classify by message keyword for the CI alerting hook.
            let msg = format!("{err:#}");
            let exit = if msg.contains("smoke verify") {
                3
            } else if msg.contains("sign hook") || msg.contains("push hook") {
                2
            } else {
                1
            };
            eprintln!("nixfleet-release: {msg}");
            ExitCode::from(exit)
        }
    }
}
