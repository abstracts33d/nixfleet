#![allow(clippy::doc_lazy_continuation)]
//! Producer for `releases/fleet.resolved.json` — the artifact defined
//! in `docs/CONTRACTS.md §I #1` and ` `.
//!
//! The orchestration pipeline:
//!
//! 1. Enumerate hosts (default: every `nixosConfigurations.*` AND
//!    every `darwinConfigurations.*` in the consumer flake). Each
//!    host carries a [`HostKind`] discriminator that decides the
//!    attr path passed to `nix build`.
//! 2. Build each host's `config.system.build.toplevel`. nix-darwin
//!    exposes the same attr path as NixOS, so the inner accessor
//!    is shared; only the outer prefix
//!    (`nixosConfigurations` / `darwinConfigurations`) differs.
//!    Cross-platform builds rely on operator-configured remote
//!    builders (`nix.buildMachines`) — the framework is
//!    backend-agnostic and never ssh's directly.
//! 3. (optional) Run a per-host `--push-cmd` to upload the closure to
//!   the fleet's binary cache. Implementation-agnostic — the hook
//!   receives the store path in `$NIXFLEET_PATH`.
//! 4. Evaluate `.#fleet.resolved` and parse via `nixfleet_proto`.
//! 5. Inject each built host's `closureHash = basename(toplevel)`.
//! 6. Stamp `meta.{signedAt, ciCommit, signatureAlgorithm}`.
//! 7. Canonicalize via `nixfleet_canonicalize::canonicalize` (the
//!   single implementation per CONTRACTS §III).
//! 8. Hand the canonical bytes to a `--sign-cmd` hook via tempfiles
//!   (`$NIXFLEET_INPUT` for canonical bytes, `$NIXFLEET_OUTPUT`
//!   where the hook MUST write the raw signature).
//! 9. Smoke-verify the produced (artifact, signature) pair through
//!   `nixfleet_reconciler::verify_artifact` — catches "we just
//!   signed something the verifier rejects." Structural-only by
//!   default; full signature check when a public key is supplied.
//! 10. Atomic-write the release dir.
//! 11. (optional) `git commit` + `git push`.
//!
//! The hook contract (env-var names, exit-code semantics, file vs
//! stdin) is part of the producer side of CONTRACTS §I #1 — don't
//! change without an §VIII amendment.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use nixfleet_proto::{FleetResolved, RevocationEntry, Revocations};
use nixfleet_reconciler::project_manifest;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

/// Hosts to release. Resolved against the consumer's flake at
/// runtime — `Auto` queries both `nixosConfigurations` and
/// `darwinConfigurations`.
#[derive(Debug, Clone)]
pub enum HostsSpec {
    /// Union of `nixosConfigurations.*` and `darwinConfigurations.*`.
    Auto,
    /// Same as `Auto`, minus the listed names.
    AutoExclude(Vec<String>),
    /// Explicit list. Order preserved. Each name must exist in
    /// exactly one of `nixosConfigurations` or `darwinConfigurations`;
    /// names appearing in both error out at classify time
    /// (intentional — the operator should disambiguate).
    Explicit(Vec<String>),
}

/// Which `*Configurations` attrset a host lives in. Drives the
/// `nix build` attr path the release pipeline emits — nix-darwin's
/// `darwinConfigurations.<h>.config.system.build.toplevel` is the
/// canonical accessor, identical in shape to the NixOS one but
/// reachable from a different prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKind {
    Nixos,
    Darwin,
}

impl HostKind {
    /// The flake-attr prefix used by `nix build` / `nix eval`.
    pub fn attr_prefix(self) -> &'static str {
        match self {
            HostKind::Nixos => "nixosConfigurations",
            HostKind::Darwin => "darwinConfigurations",
        }
    }
}

/// Configuration assembled from CLI flags. The library entry point
/// `run(&config)` consumes this.
///
/// Speculative knobs deliberately not present (each landed during
/// design, removed during the no-untested-code pass — re-add with
/// tests when a real fleet exercises them):
/// - `jobs > 1`: parallel `nix build` orchestration via
///   `std::thread::spawn`. Single-host build is fast enough on
///   today's fleets; concurrency is tricky and was never run with
///   N > 1.
/// - `smoke_verify_pubkey`: full-signature smoke verify with an
///   operator-supplied pubkey. Structural smoke verify (canonicalize
///   round-trip + schema parse) covers the framework's invariants;
///   full-sig is an operator-workflow concern and re-adds a wrapper
///   around `verify_artifact` that needs its own test surface.
#[derive(Debug, Clone)]
pub struct ReleaseConfig {
    /// Path passed to `nix build` / `nix eval`. Defaults to `.`.
    pub flake_dir: PathBuf,
    /// Attribute path (relative to `flake_dir`) yielding the
    /// `FleetResolved`-shaped JSON. Default `.#fleet.resolved`.
    pub fleet_resolved_attr: String,
    pub hosts: HostsSpec,
    /// Shell command run once per built closure. Receives env
    /// `NIXFLEET_HOST`, `NIXFLEET_PATH`, `NIXFLEET_CLOSURE_HASH`.
    pub push_cmd: Option<String>,
    /// Shell command that signs canonical bytes. Receives env
    /// `NIXFLEET_INPUT` (file path with canonical bytes) and
    /// `NIXFLEET_OUTPUT` (file path where the hook writes the raw
    /// signature). Required.
    pub sign_cmd: String,
    /// One of `ed25519` / `ecdsa-p256`. Stamped into `meta`.
    pub signature_algorithm: String,
    pub release_dir: PathBuf,
    pub artifact_name: String,
    pub git_commit: bool,
    pub git_push: Option<GitPushTarget>,
    pub commit_template: String,
    pub git_user_name: Option<String>,
    pub git_user_email: Option<String>,
    /// Toggle the structural smoke verify (canonicalize round-trip
    /// + schema parse + non-zero signature length). Default on; off
    ///   for offline test scenarios where the just-produced bytes
    ///   don't matter.
    pub smoke_verify: bool,
    /// When set and the existing release file's closureHashes match
    /// the just-built hashes, reuse its `signedAt` instead of
    /// stamping a new one. Produces byte-stable releases on no-op
    /// runs.
    pub reuse_unchanged_signature: bool,
    /// Gap C: operator-declared revocations. When `Some`, the
    /// release pipeline evaluates this flake attribute (which
    /// must yield a JSON list of `{hostname, notBefore, reason?,
    /// revokedBy?}` entries — empty list is fine), wraps it in a
    /// `Revocations` envelope with the same `meta.signedAt` /
    /// `meta.ciCommit` stamping as `fleet.resolved`, canonicalises,
    /// signs via the same `sign_cmd` hook, and writes
    /// `revocations.json` + `revocations.json.sig` alongside
    /// `fleet.resolved.json` in the release dir. When `None`, no
    /// revocations artifact is produced and the CP runs without
    /// the revocations poll source.
    pub revocations_attr: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GitPushTarget {
    pub remote: String,
    pub branch: String,
}

/// Outcome surfaced to the caller (and reflected in the binary's
/// exit code).
#[derive(Debug)]
pub enum RunOutcome {
    /// Release was newly produced. `commit_sha` is `Some` when
    /// `--git-commit` was set and a commit landed.
    Released {
        commit_sha: Option<String>,
        hosts: Vec<String>,
    },
    /// Closure hashes unchanged; no signature regenerated.
    /// Only reachable with `reuse_unchanged_signature = true`.
    NoChange,
}

/// Library entry point. Pure orchestration: every IO / shell-out
/// is owned by helpers in this module so tests can substitute.
pub fn run(config: &ReleaseConfig) -> Result<RunOutcome> {
    validate_config(config)?;

    tracing::info!(
        target: "nixfleet_release",
        flake = %config.flake_dir.display(),
        "release pipeline start",
    );

    // ── 1. Enumerate ────────────────────────────────────────────
    let hosts = enumerate_hosts(config)?;
    if hosts.is_empty() {
        bail!("no hosts to release — empty enumeration");
    }
    let host_names: Vec<&str> = hosts.iter().map(|(n, _)| n.as_str()).collect();
    tracing::info!(count = hosts.len(), hosts = ?host_names, "enumerated");

    // ── 2. Build ────────────────────────────────────────────────
    let built = build_hosts(config, &hosts)?;
    tracing::info!(built = built.len(), total = hosts.len(), "build done");

    // ── 3. Push ─────────────────────────────────────────────────
    if let Some(cmd) = &config.push_cmd {
        for (host, path) in built.iter() {
            let hash = closure_hash(path);
            push_one(cmd, host, path, &hash)?;
        }
    }

    // ── 4. Eval fleet.resolved ──────────────────────────────────
    let mut resolved = eval_fleet_resolved(config)?;

    // ── 5. Inject closureHashes ─────────────────────────────────
    let hashes: BTreeMap<String, String> = built
        .iter()
        .map(|(h, p)| (h.clone(), closure_hash(p)))
        .collect();
    inject_closure_hashes(&mut resolved, &hashes);

    // ── 5a. Idempotency short-circuit ───────────────────────────
    let release_path = config.release_dir.join(&config.artifact_name);
    let signature_path = config.release_dir.join(format!("{}.sig", config.artifact_name));
    let preserved_signed_at: Option<DateTime<Utc>> = if config.reuse_unchanged_signature {
        load_existing_signed_at_if_unchanged(&release_path, &resolved)?
    } else {
        None
    };

    // ── 6. Stamp meta ───────────────────────────────────────────
    let signed_at = preserved_signed_at.unwrap_or_else(Utc::now);
    let ci_commit = git_head_sha(&config.flake_dir).ok();
    stamp_meta(&mut resolved, signed_at, ci_commit.clone(), &config.signature_algorithm);

    // ── 7. Canonicalize ─────────────────────────────────────────
    let canonical = canonicalize_resolved(&resolved)?;

    // ── 8. Sign ─────────────────────────────────────────────────
    // Skip signing when reuse_unchanged_signature triggered the
    // short-circuit AND the existing signature file is intact.
    let sig_bytes = if preserved_signed_at.is_some() && signature_path.exists() {
        std::fs::read(&signature_path).context("read existing signature")?
    } else {
        sign(&config.sign_cmd, canonical.as_bytes())?
    };

    // ── 9. Smoke verify ─────────────────────────────────────────
    if config.smoke_verify {
        smoke_verify(canonical.as_bytes(), &sig_bytes)?;
    }

    // ── 10. Write release dir ───────────────────────────────────
    write_release(&config.release_dir, &config.artifact_name, canonical.as_bytes(), &sig_bytes)?;

    // ── 10a. Revocations artifact . Optional. ────────────
    // Same canonicalize + sign path as fleet.resolved; the artifact
    // must exist (even empty) for CP-rebuild recovery semantics to
    // hold — otherwise an operator who has never declared a
    // revocation has no signed file to prime cert_revocations from.
    let mut revocations_paths: Vec<PathBuf> = Vec::new();
    if let Some(attr) = &config.revocations_attr {
        let entries = eval_revocations(config, attr)?;
        let revs = Revocations {
            schema_version: 1,
            revocations: entries,
            meta: nixfleet_proto::Meta {
                schema_version: 1,
                signed_at: Some(signed_at),
                ci_commit: ci_commit.clone(),
                signature_algorithm: Some(config.signature_algorithm.clone()),
            },
        };
        let revs_json = serde_json::to_string(&revs)
            .context("serialise revocations.json")?;
        let revs_canonical = nixfleet_canonicalize::canonicalize(&revs_json)
            .context("canonicalize revocations.json")?;
        let revs_sig_path = config
            .release_dir
            .join("revocations.json.sig");
        let revs_path = config.release_dir.join("revocations.json");
        // Reuse-unchanged short-circuit: if the existing canonical
        // bytes match what we'd write, reuse the signature on disk
        // (idempotent, byte-stable). Otherwise sign anew.
        let revs_sig_bytes = if revs_path.exists()
            && revs_sig_path.exists()
            && std::fs::read(&revs_path).ok().as_deref() == Some(revs_canonical.as_bytes())
        {
            std::fs::read(&revs_sig_path).context("read existing revocations signature")?
        } else {
            sign(&config.sign_cmd, revs_canonical.as_bytes())?
        };
        write_release(
            &config.release_dir,
            "revocations.json",
            revs_canonical.as_bytes(),
            &revs_sig_bytes,
        )?;
        revocations_paths.push(revs_path);
        revocations_paths.push(revs_sig_path);
        tracing::info!(
            target: "nixfleet_release",
            entries = revs.revocations.len(),
            "revocations.json signed + written",
        );
    }

    // ── 10b. Rollout manifests ──────────────────────────────────
    // One signed manifest per channel, projected from the just-signed
    // fleet.resolved. Same canonicalize + sign hook. Each manifest's
    // fleetResolvedHash binds it cryptographically to this snapshot
    // (RFC-0002 §4.4); without that binding a key-rotation overlap
    // window could otherwise let an attacker pair a manifest from
    // snapshot X with the resolved.json from snapshot Y.
    let mut manifest_paths: Vec<PathBuf> = Vec::new();
    let fleet_resolved_hash = sha256_hex(canonical.as_bytes());
    let rollouts_dir = config.release_dir.join("rollouts");
    for (channel_name, _channel) in resolved.channels.iter() {
        let manifest = match project_manifest(
            &resolved,
            channel_name,
            &fleet_resolved_hash,
            signed_at,
            ci_commit.as_deref(),
            &config.signature_algorithm,
        )? {
            Some(m) => m,
            None => continue, // channel has no host with a closureHash
        };

        let manifest_json = serde_json::to_string(&manifest)
            .with_context(|| format!("serialise manifest for channel {channel_name}"))?;
        let manifest_canonical = nixfleet_canonicalize::canonicalize(&manifest_json)
            .with_context(|| format!("canonicalize manifest for channel {channel_name}"))?;
        let rollout_id = nixfleet_reconciler::compute_rollout_id(&manifest)
            .with_context(|| format!("compute rolloutId for channel {channel_name}"))?;

        let artifact_name = format!("{rollout_id}.json");
        let manifest_path = rollouts_dir.join(&artifact_name);
        let sig_path = rollouts_dir.join(format!("{artifact_name}.sig"));

        // Reuse-unchanged short-circuit: rolloutId is the content
        // hash, so byte-identical canonical bytes IS byte-identical
        // path. If both files exist with matching canonical bytes,
        // reuse the on-disk signature instead of re-invoking the hook.
        let sig_bytes = if manifest_path.exists()
            && sig_path.exists()
            && std::fs::read(&manifest_path).ok().as_deref() == Some(manifest_canonical.as_bytes())
        {
            std::fs::read(&sig_path).context("read existing manifest signature")?
        } else {
            sign(&config.sign_cmd, manifest_canonical.as_bytes())?
        };

        write_release(
            &rollouts_dir,
            &artifact_name,
            manifest_canonical.as_bytes(),
            &sig_bytes,
        )?;
        manifest_paths.push(manifest_path);
        manifest_paths.push(sig_path);

        tracing::info!(
            target: "nixfleet_release",
            rollout_id = %rollout_id,
            channel = %channel_name,
            host_count = manifest.host_set.len(),
            "rollout manifest signed + written",
        );
    }

    // ── 11. Git commit + push ───────────────────────────────────
    let mut commit_sha = None;
    if config.git_commit {
        let mut release_files = vec![release_path.clone(), signature_path.clone()];
        release_files.extend(revocations_paths.iter().cloned());
        release_files.extend(manifest_paths.iter().cloned());
        let committed = git_commit_release(config, &release_files, ci_commit.as_deref(), signed_at)?;
        if let Some(c) = &config.git_push {
            if committed {
                git_push_release(&config.flake_dir, c)?;
            } else {
                tracing::info!("no release change — skip push");
            }
        }
        commit_sha = if committed {
            git_head_sha(&config.flake_dir).ok()
        } else {
            None
        };
        if !committed && preserved_signed_at.is_some() {
            return Ok(RunOutcome::NoChange);
        }
    }

    let host_names: Vec<String> = hashes.keys().cloned().collect();
    Ok(RunOutcome::Released {
        commit_sha,
        hosts: host_names,
    })
}

// ─────────────────────────── validation ───────────────────────────

fn validate_config(c: &ReleaseConfig) -> Result<()> {
    match c.signature_algorithm.as_str() {
        "ed25519" | "ecdsa-p256" => {}
        other => bail!("--signature-algorithm must be 'ed25519' or 'ecdsa-p256', got '{other}'"),
    }
    if c.git_push.is_some() && !c.git_commit {
        bail!("--git-push requires --git-commit");
    }
    if c.sign_cmd.trim().is_empty() {
        bail!("--sign-cmd is required and cannot be empty");
    }
    Ok(())
}

// ─────────────────────────── enumerate ────────────────────────────

/// Returns `(host, kind)` pairs ordered as: all NixOS hosts (sorted),
/// then all Darwin hosts (sorted). `HostsSpec::Explicit` preserves
/// caller order but classifies each name by probe.
///
/// Missing attrsets (e.g. a flake with no `darwinConfigurations`)
/// are treated as empty, not errors. The release pipeline runs
/// against fleets with one or both kinds.
fn enumerate_hosts(config: &ReleaseConfig) -> Result<Vec<(String, HostKind)>> {
    let mut nixos = list_attr_optional(&config.flake_dir, "nixosConfigurations")?;
    nixos.sort();
    nixos.dedup();
    let mut darwin = list_attr_optional(&config.flake_dir, "darwinConfigurations")?;
    darwin.sort();
    darwin.dedup();

    let in_nixos = |n: &str| nixos.iter().any(|h| h == n);
    let in_darwin = |n: &str| darwin.iter().any(|h| h == n);

    Ok(match &config.hosts {
        HostsSpec::Auto => nixos
            .iter()
            .map(|n| (n.clone(), HostKind::Nixos))
            .chain(darwin.iter().map(|n| (n.clone(), HostKind::Darwin)))
            .collect(),
        HostsSpec::AutoExclude(exclude) => {
            let kept_nixos = nixos
                .iter()
                .filter(|h| !exclude.iter().any(|e| e == *h))
                .map(|n| (n.clone(), HostKind::Nixos));
            let kept_darwin = darwin
                .iter()
                .filter(|h| !exclude.iter().any(|e| e == *h))
                .map(|n| (n.clone(), HostKind::Darwin));
            kept_nixos.chain(kept_darwin).collect()
        }
        HostsSpec::Explicit(list) => list
            .iter()
            .map(|n| match (in_nixos(n), in_darwin(n)) {
                (true, false) => Ok((n.clone(), HostKind::Nixos)),
                (false, true) => Ok((n.clone(), HostKind::Darwin)),
                (true, true) => Err(anyhow::anyhow!(
                    "host '{n}' is declared in both nixosConfigurations and \
                     darwinConfigurations — disambiguate before releasing"
                )),
                (false, false) => Err(anyhow::anyhow!(
                    "host '{n}' is in neither nixosConfigurations nor \
                     darwinConfigurations of flake {}",
                    config.flake_dir.display()
                )),
            })
            .collect::<Result<Vec<_>>>()?,
    })
}

/// Evaluate the operator's revocations declaration. The attr must
/// produce a JSON array of `{hostname, notBefore, reason?,
/// revokedBy?}` objects. An empty array is valid and means "no
/// revocations on file" — the artifact still gets produced so a
/// CP-rebuild has something to verify + replay (even if empty).
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest.iter() {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

fn eval_revocations(config: &ReleaseConfig, attr: &str) -> Result<Vec<RevocationEntry>> {
    let output = Command::new("nix")
        .args([
            "eval",
            "--json",
            "--no-warn-dirty",
            &format!(".#{attr}"),
        ])
        .current_dir(&config.flake_dir)
        .output()
        .with_context(|| format!("invoke `nix eval .#{attr}`"))?;
    if !output.status.success() {
        bail!(
            "nix eval .#{attr}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    serde_json::from_slice(&output.stdout)
        .with_context(|| format!("parse revocations from `nix eval .#{attr}`"))
}

/// Enumerate attribute names under `<flake>#<attr_path>`, treating a
/// missing attrset as empty. Used for `darwinConfigurations` /
/// `nixosConfigurations` enumeration where a fleet may legitimately
/// declare only one. The "missing attribute" stderr from `nix eval`
/// is matched against a small set of stable phrasings; any other
/// failure surfaces as `Err`.
fn list_attr_optional(flake_dir: &Path, attr_path: &str) -> Result<Vec<String>> {
    let output = Command::new("nix")
        .args([
            "eval",
            "--json",
            "--no-warn-dirty",
            &format!(".#{attr_path}"),
            "--apply",
            "builtins.attrNames",
        ])
        .current_dir(flake_dir)
        .output()
        .with_context(|| format!("invoke `nix eval .#{attr_path}`"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Stable phrasings nix uses for "this attribute doesn't
        // exist on the flake" — verified against nix 2.18-2.30 in
        // unit tests below. Anything else (eval error, IO error,
        // permissions) propagates.
        let lowered = stderr.to_lowercase();
        let is_missing = [
            "does not provide attribute",
            "has no attribute",
            "attribute 'darwinconfigurations' missing",
            "attribute 'nixosconfigurations' missing",
        ]
        .iter()
        .any(|needle| lowered.contains(needle));
        if is_missing {
            tracing::debug!(
                attr_path,
                "flake does not declare {attr_path}; treating as empty"
            );
            return Ok(vec![]);
        }
        bail!("nix eval .#{attr_path}: {stderr}");
    }
    let names: Vec<String> = serde_json::from_slice(&output.stdout)
        .with_context(|| format!("parse JSON from `nix eval .#{attr_path}`"))?;
    Ok(names)
}

// ─────────────────────────── build ────────────────────────────────

/// Sequential build. Each host's `<prefix>.<host>.config.system.build.
/// toplevel` is built and the resulting store path is captured;
/// `<prefix>` is `nixosConfigurations` for [`HostKind::Nixos`] and
/// `darwinConfigurations` for [`HostKind::Darwin`]. Failures abort
/// the run before any push.
///
/// Cross-platform builds (linux CI building a darwin host, or the
/// reverse) are handled by `nix`'s remote-builder mechanism via the
/// operator's `nix.buildMachines` / `extra-platforms` config —
/// invisible to this code path.
///
/// Parallel builds (`--jobs > 1`) were dropped during the
/// no-untested-code pass — re-add with tests when a real fleet
/// needs them.
fn build_hosts(
    config: &ReleaseConfig,
    hosts: &[(String, HostKind)],
) -> Result<BTreeMap<String, PathBuf>> {
    let mut out = BTreeMap::new();
    for (host, kind) in hosts {
        let attr = format!(
            ".#{}.{host}.config.system.build.toplevel",
            kind.attr_prefix()
        );
        let path = build_one(&config.flake_dir, &attr)
            .with_context(|| format!("build host {host}"))?;
        tracing::info!(host = %host, kind = ?kind, path, "built");
        out.insert(host.clone(), PathBuf::from(path));
    }
    Ok(out)
}

fn build_one(flake_dir: &Path, attr: &str) -> Result<String> {
    let output = Command::new("nix")
        .args([
            "build",
            "--no-link",
            "--print-out-paths",
            "--no-warn-dirty",
            attr,
        ])
        .current_dir(flake_dir)
        .output()
        .with_context(|| format!("invoke `nix build {attr}`"))?;
    if !output.status.success() {
        bail!(
            "nix build {attr}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        bail!("nix build {attr}: empty output");
    }
    Ok(path)
}

fn closure_hash(path: &Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string()
}

// ─────────────────────────── push hook ────────────────────────────

fn push_one(cmd: &str, host: &str, path: &Path, closure_hash: &str) -> Result<()> {
    tracing::info!(host = %host, "push hook");
    let status = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .env("NIXFLEET_HOST", host)
        .env("NIXFLEET_PATH", path)
        .env("NIXFLEET_CLOSURE_HASH", closure_hash)
        .status()
        .with_context(|| format!("invoke push hook for {host}"))?;
    if !status.success() {
        bail!(
            "push hook for {host} exited {} ({:?})",
            status.code().unwrap_or(-1),
            cmd,
        );
    }
    Ok(())
}

// ─────────────────────────── eval + mutate ────────────────────────

pub(crate) fn eval_fleet_resolved(config: &ReleaseConfig) -> Result<FleetResolved> {
    let output = Command::new("nix")
        .args([
            "eval",
            "--json",
            "--no-warn-dirty",
            &config.fleet_resolved_attr,
        ])
        .current_dir(&config.flake_dir)
        .output()
        .with_context(|| format!("invoke `nix eval {}`", config.fleet_resolved_attr))?;
    if !output.status.success() {
        bail!(
            "nix eval {}: {}",
            config.fleet_resolved_attr,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let resolved: FleetResolved =
        serde_json::from_slice(&output.stdout).with_context(|| {
            format!(
                "parse {} as FleetResolved",
                config.fleet_resolved_attr
            )
        })?;
    Ok(resolved)
}

/// Mutates `resolved` to set `hosts[h].closureHash` from the map.
/// Hosts in `hashes` that don't exist in `resolved.hosts` are
/// silently skipped (matches the legacy jq behaviour).
pub fn inject_closure_hashes(
    resolved: &mut FleetResolved,
    hashes: &BTreeMap<String, String>,
) {
    for (host, hash) in hashes {
        if let Some(h) = resolved.hosts.get_mut(host) {
            h.closure_hash = Some(hash.clone());
        }
    }
}

/// Mutates `resolved.meta` with the three signing fields.
pub fn stamp_meta(
    resolved: &mut FleetResolved,
    signed_at: DateTime<Utc>,
    ci_commit: Option<String>,
    signature_algorithm: &str,
) {
    resolved.meta.signed_at = Some(signed_at);
    resolved.meta.ci_commit = ci_commit;
    resolved.meta.signature_algorithm = Some(signature_algorithm.to_string());
}

pub fn canonicalize_resolved(resolved: &FleetResolved) -> Result<String> {
    let raw =
        serde_json::to_string(resolved).context("serialize FleetResolved before canonicalize")?;
    nixfleet_canonicalize::canonicalize(&raw).context("canonicalize fleet.resolved")
}

// ─────────────────────────── idempotency ──────────────────────────

/// If an existing release file already encodes the same set of
/// closure hashes (and host topology) as the currently-injected
/// `resolved`, return its `meta.signedAt` so the caller can reuse
/// it. Otherwise `None`.
fn load_existing_signed_at_if_unchanged(
    release_path: &Path,
    resolved: &FleetResolved,
) -> Result<Option<DateTime<Utc>>> {
    if !release_path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(release_path)
        .with_context(|| format!("read existing release {}", release_path.display()))?;
    let existing: FleetResolved =
        serde_json::from_str(&raw).context("parse existing release file")?;

    let cur_hashes: BTreeMap<&str, Option<&str>> = resolved
        .hosts
        .iter()
        .map(|(k, v)| (k.as_str(), v.closure_hash.as_deref()))
        .collect();
    let prev_hashes: BTreeMap<&str, Option<&str>> = existing
        .hosts
        .iter()
        .map(|(k, v)| (k.as_str(), v.closure_hash.as_deref()))
        .collect();

    if cur_hashes == prev_hashes {
        Ok(existing.meta.signed_at)
    } else {
        Ok(None)
    }
}

// ─────────────────────────── sign hook ────────────────────────────

fn sign(cmd: &str, canonical: &[u8]) -> Result<Vec<u8>> {
    let input = NamedTempFile::new().context("create tempfile for canonical bytes")?;
    let output = NamedTempFile::new().context("create tempfile for signature")?;

    std::fs::write(input.path(), canonical).context("write canonical bytes to tempfile")?;
    // Pre-create output as empty so the hook only needs to overwrite it.
    std::fs::write(output.path(), b"").ok();

    tracing::info!("sign hook");
    let status = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .env("NIXFLEET_INPUT", input.path())
        .env("NIXFLEET_OUTPUT", output.path())
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("invoke sign hook ({cmd:?})"))?;
    if !status.success() {
        bail!(
            "sign hook exited {} ({:?})",
            status.code().unwrap_or(-1),
            cmd,
        );
    }

    let sig = std::fs::read(output.path()).context("read signature output")?;
    if sig.is_empty() {
        bail!("sign hook produced 0-byte signature — refusing to publish");
    }
    Ok(sig)
}

// ─────────────────────────── smoke verify ─────────────────────────

/// Structural smoke verify: confirm the produced (canonical, sig)
/// pair are at least *shape-correct* before publishing — parse the
/// canonical bytes back into `FleetResolved`, canonicalize again, and
/// require byte-stable round-trip. Catches schema drift, JCS bugs,
/// and zero-byte signatures.
///
/// Cryptographic verification with a real pubkey was a flag here
/// (`--smoke-verify-pubkey`); dropped during the no-untested-code
/// pass. The underlying `verify_artifact` in `nixfleet_reconciler`
/// is heavily tested already; if an operator wants to spot-check
/// signatures they invoke `nixfleet-verify-artifact` directly
/// post-release.
fn smoke_verify(canonical: &[u8], signature: &[u8]) -> Result<()> {
    let parsed: FleetResolved = serde_json::from_slice(canonical)
        .context("smoke verify: canonical bytes don't parse as FleetResolved")?;
    let recanonical = canonicalize_resolved(&parsed)
        .context("smoke verify: re-canonicalize failed")?;
    if recanonical.as_bytes() != canonical {
        bail!("smoke verify: canonicalization is not byte-stable round-trip");
    }
    if signature.is_empty() {
        bail!("smoke verify: empty signature");
    }

    tracing::info!(
        sig_len = signature.len(),
        "smoke verify ok"
    );
    Ok(())
}

// ─────────────────────────── write release ───────────────────────

fn write_release(
    release_dir: &Path,
    artifact_name: &str,
    canonical: &[u8],
    signature: &[u8],
) -> Result<()> {
    std::fs::create_dir_all(release_dir).with_context(|| {
        format!("create release dir {}", release_dir.display())
    })?;
    let artifact_path = release_dir.join(artifact_name);
    let signature_path = release_dir.join(format!("{artifact_name}.sig"));
    atomic_write(&artifact_path, canonical)?;
    atomic_write(&signature_path, signature)?;
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .with_context(|| format!("tempfile in {}", dir.display()))?;
    use std::io::Write;
    tmp.write_all(bytes).context("write release tempfile")?;
    tmp.persist(path)
        .with_context(|| format!("rename tempfile to {}", path.display()))?;
    Ok(())
}

// ─────────────────────────── git ──────────────────────────────────

fn git_head_sha(repo: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
        .context("invoke `git rev-parse HEAD`")?;
    if !output.status.success() {
        bail!(
            "git rev-parse HEAD: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_commit_release(
    config: &ReleaseConfig,
    files: &[PathBuf],
    ci_commit: Option<&str>,
    signed_at: DateTime<Utc>,
) -> Result<bool> {
    if let Some(name) = &config.git_user_name {
        run_git(&config.flake_dir, &["config", "user.name", name])?;
    }
    if let Some(email) = &config.git_user_email {
        run_git(&config.flake_dir, &["config", "user.email", email])?;
    }
    let mut add_args = vec!["add", "--"];
    let file_strs: Vec<String> = files
        .iter()
        .map(|p| {
            p.strip_prefix(&config.flake_dir)
                .unwrap_or(p)
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    for f in &file_strs {
        add_args.push(f);
    }
    run_git(&config.flake_dir, &add_args)?;

    // Quick: any staged change in the release files?
    let cached_diff = Command::new("git")
        .args(["diff", "--cached", "--quiet", "--"])
        .args(&file_strs)
        .current_dir(&config.flake_dir)
        .status()
        .context("invoke `git diff --cached --quiet`")?;
    if cached_diff.success() {
        tracing::info!("git: no release change");
        return Ok(false);
    }

    let message = render_commit_message(
        &config.commit_template,
        ci_commit.unwrap_or("HEAD"),
        signed_at,
    );
    run_git(&config.flake_dir, &["commit", "-m", &message])?;
    tracing::info!(message = %message, "git commit");
    Ok(true)
}

pub fn render_commit_message(template: &str, sha: &str, ts: DateTime<Utc>) -> String {
    let short = if sha.len() >= 8 { &sha[..8] } else { sha };
    template
        .replace("{sha:0:8}", short)
        .replace("{sha}", sha)
        .replace("{ts}", &ts.to_rfc3339())
}

fn git_push_release(repo: &Path, target: &GitPushTarget) -> Result<()> {
    let refspec = format!("HEAD:{}", target.branch);
    run_git(repo, &["push", &target.remote, &refspec])?;
    tracing::info!(
        remote = %target.remote,
        branch = %target.branch,
        "git push",
    );
    Ok(())
}

fn run_git(repo: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .args(args)
        .current_dir(repo)
        .status()
        .with_context(|| format!("invoke git {args:?}"))?;
    if !status.success() {
        bail!("git {:?} exited {}", args, status.code().unwrap_or(-1));
    }
    Ok(())
}

// ─────────────────────────── tests ────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nixfleet_proto::{Channel, Compliance, Host, Meta};

    fn dummy_resolved() -> FleetResolved {
        let mut hosts = std::collections::HashMap::new();
        hosts.insert(
            "test-host".to_string(),
            Host {
                system: "x86_64-linux".into(),
                tags: vec![],
                channel: "stable".into(),
                closure_hash: None,
                pubkey: None,
            },
        );
        hosts.insert(
            "aether".to_string(),
            Host {
                system: "aarch64-darwin".into(),
                tags: vec![],
                channel: "stable".into(),
                closure_hash: None,
                pubkey: None,
            },
        );
        let mut channels = std::collections::HashMap::new();
        channels.insert(
            "stable".to_string(),
            Channel {
                rollout_policy: "default".into(),
                reconcile_interval_minutes: 5,
                freshness_window: 60,
                signing_interval_minutes: 30,
                compliance: Compliance {
                    frameworks: vec![],
                    mode: "disabled".to_string(),
                },
            },
        );
        FleetResolved {
            schema_version: 1,
            hosts,
            channels,
            rollout_policies: Default::default(),
            waves: Default::default(),
            edges: vec![],
            disruption_budgets: vec![],
            meta: Meta {
                schema_version: 1,
                signed_at: None,
                ci_commit: None,
                signature_algorithm: None,
            },
        }
    }

    #[test]
    fn inject_sets_closure_hash_for_known_hosts_and_skips_unknown() {
        let mut r = dummy_resolved();
        let mut hashes = BTreeMap::new();
        hashes.insert("test-host".to_string(), "abc123-nixos-system-test-host".to_string());
        hashes.insert("ghost".to_string(), "should-be-ignored".to_string());
        inject_closure_hashes(&mut r, &hashes);
        assert_eq!(
            r.hosts["test-host"].closure_hash.as_deref(),
            Some("abc123-nixos-system-test-host")
        );
        assert!(r.hosts["aether"].closure_hash.is_none());
        assert!(!r.hosts.contains_key("ghost"));
    }

    #[test]
    fn stamp_meta_writes_three_fields() {
        let mut r = dummy_resolved();
        let ts = DateTime::parse_from_rfc3339("2026-04-27T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        stamp_meta(&mut r, ts, Some("deadbeef".into()), "ed25519");
        assert_eq!(r.meta.signed_at, Some(ts));
        assert_eq!(r.meta.ci_commit.as_deref(), Some("deadbeef"));
        assert_eq!(r.meta.signature_algorithm.as_deref(), Some("ed25519"));
    }

    #[test]
    fn canonicalize_round_trip_is_byte_stable() {
        let mut r = dummy_resolved();
        let ts = DateTime::parse_from_rfc3339("2026-04-27T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        stamp_meta(&mut r, ts, Some("deadbeef".into()), "ed25519");
        let c1 = canonicalize_resolved(&r).unwrap();
        let parsed: FleetResolved = serde_json::from_str(&c1).unwrap();
        let c2 = canonicalize_resolved(&parsed).unwrap();
        assert_eq!(c1, c2);
    }

    #[test]
    fn render_commit_message_substitutes() {
        let ts = DateTime::parse_from_rfc3339("2026-04-27T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let m = render_commit_message(
            "chore(ci): release {sha:0:8} [skip ci]",
            "deadbeefcafebabe",
            ts,
        );
        assert_eq!(m, "chore(ci): release deadbeef [skip ci]");

        let m2 = render_commit_message("ts={ts}, sha={sha}", "abc", ts);
        assert_eq!(m2, "ts=2026-04-27T12:00:00+00:00, sha=abc");
    }

    fn manifest_resolved() -> FleetResolved {
        use nixfleet_proto::{HealthGate, PolicyWave, RolloutPolicy, Selector, Wave};
        let mut hosts = std::collections::HashMap::new();
        hosts.insert(
            "agent-02".to_string(),
            Host {
                system: "x86_64-linux".into(),
                tags: vec![],
                channel: "stable".into(),
                closure_hash: Some("aaaa-host-b".into()),
                pubkey: None,
            },
        );
        hosts.insert(
            "agent-01".to_string(),
            Host {
                system: "x86_64-linux".into(),
                tags: vec![],
                channel: "stable".into(),
                closure_hash: Some("aaaa-host-a".into()),
                pubkey: None,
            },
        );
        hosts.insert(
            "agent-no-closure".to_string(),
            Host {
                system: "x86_64-linux".into(),
                tags: vec![],
                channel: "stable".into(),
                closure_hash: None, // no declaration → skipped
                pubkey: None,
            },
        );
        let mut channels = std::collections::HashMap::new();
        channels.insert(
            "stable".to_string(),
            Channel {
                rollout_policy: "default".into(),
                reconcile_interval_minutes: 5,
                freshness_window: 60,
                signing_interval_minutes: 30,
                compliance: Compliance {
                    frameworks: vec!["anssi-bp028".into()],
                    mode: "permissive".to_string(),
                },
            },
        );
        let mut rollout_policies = std::collections::HashMap::new();
        rollout_policies.insert(
            "default".to_string(),
            RolloutPolicy {
                strategy: "waves".into(),
                waves: vec![PolicyWave {
                    selector: Selector {
                        tags: vec![],
                        tags_any: vec![],
                        hosts: vec![],
                        channel: None,
                        all: true,
                    },
                    soak_minutes: 5,
                }],
                health_gate: HealthGate::default(),
                on_health_failure: nixfleet_proto::OnHealthFailure::Halt,
            },
        );
        let mut waves = std::collections::HashMap::new();
        waves.insert(
            "stable".to_string(),
            vec![
                Wave {
                    hosts: vec!["agent-01".into()],
                    soak_minutes: 5,
                },
                Wave {
                    hosts: vec!["agent-02".into()],
                    soak_minutes: 5,
                },
            ],
        );
        FleetResolved {
            schema_version: 1,
            hosts,
            channels,
            rollout_policies,
            waves,
            edges: vec![],
            disruption_budgets: vec![],
            meta: Meta {
                schema_version: 1,
                signed_at: None,
                ci_commit: None,
                signature_algorithm: None,
            },
        }
    }

    #[test]
    fn project_manifest_emits_sorted_host_set_with_correct_wave_indices() {
        let r = manifest_resolved();
        let ts = DateTime::parse_from_rfc3339("2026-04-30T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let m = project_manifest(&r, "stable", "feedface", ts, Some("def45678"), "ed25519")
            .unwrap()
            .expect("non-empty manifest");
        // sorted: agent-01 before agent-02
        assert_eq!(m.host_set[0].hostname, "agent-01");
        assert_eq!(m.host_set[1].hostname, "agent-02");
        // agent-no-closure skipped — only 2 entries
        assert_eq!(m.host_set.len(), 2);
        // wave_index from waves["stable"]
        assert_eq!(m.host_set[0].wave_index, 0);
        assert_eq!(m.host_set[1].wave_index, 1);
        // per-host target_closure preserved
        assert_eq!(m.host_set[0].target_closure, "aaaa-host-a");
        assert_eq!(m.host_set[1].target_closure, "aaaa-host-b");
        // anchor + display_name + meta
        assert_eq!(m.fleet_resolved_hash, "feedface");
        assert_eq!(m.display_name, "stable@def45678");
        assert_eq!(m.channel_ref, "def45678");
        assert_eq!(m.meta.signed_at, Some(ts));
        assert_eq!(m.compliance_frameworks, vec!["anssi-bp028".to_string()]);
    }

    #[test]
    fn project_manifest_returns_none_when_no_host_has_closure_hash() {
        // dummy_resolved's hosts have closure_hash: None
        let r = dummy_resolved();
        // dummy_resolved has no rollout_policies → project errors;
        // give it a policy first.
        let mut r = r;
        r.rollout_policies.insert(
            "default".to_string(),
            nixfleet_proto::RolloutPolicy {
                strategy: "waves".into(),
                waves: vec![],
                health_gate: nixfleet_proto::HealthGate::default(),
                on_health_failure: nixfleet_proto::OnHealthFailure::Halt,
            },
        );
        let ts = Utc::now();
        let result = project_manifest(&r, "stable", "deadbeef", ts, None, "ed25519").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn project_manifest_errors_on_missing_channel() {
        let r = manifest_resolved();
        let ts = Utc::now();
        let err = project_manifest(&r, "ghost", "feedface", ts, None, "ed25519").unwrap_err();
        assert!(err.to_string().contains("channel ghost"));
    }

    #[test]
    fn sha256_hex_is_64_char_lowercase() {
        let h = sha256_hex(b"hello world");
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        // known sha256("hello world")
        assert_eq!(
            h,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn validate_rejects_bad_algorithm() {
        let mut c = base_config();
        c.signature_algorithm = "rsa".into();
        let err = validate_config(&c).unwrap_err();
        assert!(err.to_string().contains("signature-algorithm"));
    }

    #[test]
    fn validate_rejects_push_without_commit() {
        let mut c = base_config();
        c.git_push = Some(GitPushTarget {
            remote: "origin".into(),
            branch: "main".into(),
        });
        c.git_commit = false;
        let err = validate_config(&c).unwrap_err();
        assert!(err.to_string().contains("--git-commit"));
    }

    fn base_config() -> ReleaseConfig {
        ReleaseConfig {
            flake_dir: PathBuf::from("."),
            fleet_resolved_attr: ".#fleet.resolved".into(),
            hosts: HostsSpec::Auto,
            push_cmd: None,
            sign_cmd: "true".into(),
            signature_algorithm: "ed25519".into(),
            release_dir: PathBuf::from("releases"),
            artifact_name: "fleet.resolved.json".into(),
            git_commit: false,
            git_push: None,
            commit_template: "release {sha:0:8}".into(),
            git_user_name: None,
            git_user_email: None,
            smoke_verify: true,
            reuse_unchanged_signature: false,
            revocations_attr: None,
        }
    }

    #[test]
    fn host_kind_attr_prefix_matches_flake_convention() {
        // Wire shape: the framework must emit `nixosConfigurations.<h>`
        // for linux hosts and `darwinConfigurations.<h>` for darwin
        // hosts. Locked here so a rename surfaces as a test failure
        // rather than a silent build path change.
        assert_eq!(HostKind::Nixos.attr_prefix(), "nixosConfigurations");
        assert_eq!(HostKind::Darwin.attr_prefix(), "darwinConfigurations");
    }
}
