//! Pure projection: fleet.resolved + channel context → RolloutManifest.
//! Producer (nixfleet-release) and CP (re-derivation) share this fn.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use nixfleet_proto::{FleetResolved, HostWave, Meta, RolloutManifest};

/// CP-side rolloutId for a host on `channel`. `Ok(None)` when the channel
/// has no host with a declared closure.
pub fn compute_rollout_id_for_channel(
    fleet: &FleetResolved,
    fleet_resolved_hash: &str,
    channel: &str,
) -> Result<Option<String>> {
    let signed_at = fleet
        .meta
        .signed_at
        .ok_or_else(|| anyhow!("fleet.meta.signedAt is None — cannot project manifest"))?;
    let ci_commit = fleet.meta.ci_commit.as_deref();
    let manifest = match project_manifest(
        fleet,
        channel,
        fleet_resolved_hash,
        signed_at,
        ci_commit,
        &fleet.meta.signature_algorithm,
    )? {
        Some(m) => m,
        None => return Ok(None),
    };
    let id = crate::verify::compute_rollout_id(&manifest)
        .map_err(|e| anyhow!("compute_rollout_id: {e:?}"))?;
    Ok(Some(id))
}

/// Project one channel out of fleet.resolved. `Ok(None)` when no host on
/// the channel has a `closureHash`. `host_set` sorted for canonical-byte
/// stability.
pub fn project_manifest(
    fleet: &FleetResolved,
    channel: &str,
    fleet_resolved_hash: &str,
    signed_at: DateTime<Utc>,
    ci_commit: Option<&str>,
    signature_algorithm: &str,
) -> Result<Option<RolloutManifest>> {
    let channel_def = fleet
        .channels
        .get(channel)
        .ok_or_else(|| anyhow!("channel {channel} missing from fleet.channels"))?;

    let policy = fleet
        .rollout_policies
        .get(&channel_def.rollout_policy)
        .ok_or_else(|| {
            anyhow!(
                "rollout policy {} for channel {channel} not found in fleet.rolloutPolicies",
                channel_def.rollout_policy
            )
        })?;

    let waves = fleet.waves.get(channel);

    let mut host_set: Vec<HostWave> = Vec::new();
    for (hostname, host) in fleet.hosts.iter() {
        if host.channel != channel {
            continue;
        }
        let target_closure = match host.closure_hash.as_ref() {
            Some(c) => c.clone(),
            None => continue,
        };
        let wave_index: u32 = match waves {
            Some(ws) => ws
                .iter()
                .position(|w| w.hosts.iter().any(|h| h == hostname))
                .map(|i| i as u32)
                .unwrap_or(0),
            None => 0,
        };
        host_set.push(HostWave {
            hostname: hostname.clone(),
            wave_index,
            target_closure,
        });
    }

    if host_set.is_empty() {
        return Ok(None);
    }
    host_set.sort_by(|a, b| a.hostname.cmp(&b.hostname));

    let display_name = format!(
        "{}@{}",
        channel,
        ci_commit
            .map(|c| c.chars().take(8).collect::<String>())
            .unwrap_or_else(|| "unknown".to_string())
    );

    let channel_ref = ci_commit.unwrap_or_default().to_string();

    Ok(Some(RolloutManifest {
        schema_version: 1,
        display_name,
        channel: channel.to_string(),
        channel_ref,
        fleet_resolved_hash: fleet_resolved_hash.to_string(),
        host_set,
        health_gate: policy.health_gate.clone(),
        compliance_frameworks: channel_def.compliance.frameworks.clone(),
        meta: Meta {
            schema_version: 1,
            signed_at: Some(signed_at),
            ci_commit: ci_commit.map(|c| c.to_string()),
            signature_algorithm: signature_algorithm.to_string(),
        },
    }))
}
