//! Stateless distributor for pre-signed rollout manifests; CP holds no signing key.

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::IntoResponse;

use super::super::route_error::internal_warn;
use super::super::state::AppState;

/// LOADBEARING: validates the canonical RFC-0008 §6.3 RolloutId shape
/// `"{channel}@{channel_ref}"` and blocks path-traversal smuggling
/// (`/`, `..`, whitespace, multi-`@` all fail the character classes).
/// Channel is locked to lowercase ASCII to match the cycle's convention
/// and avoid case-insensitive-filesystem collisions on macOS hosts; the
/// ref tracks the git SHA shape upstream of the producer.
fn looks_like_rollout_id(s: &str) -> bool {
    let Some((channel, channel_ref)) = s.split_once('@') else {
        return false;
    };
    if channel.is_empty() || channel_ref.is_empty() {
        return false;
    }
    if channel_ref.contains('@') {
        return false;
    }
    if !channel
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return false;
    }
    channel_ref
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

fn manifest_paths(dir: &FsPath, rollout_id: &str) -> (PathBuf, PathBuf) {
    let manifest = dir.join(format!("{rollout_id}.json"));
    let sig = dir.join(format!("{rollout_id}.json.sig"));
    (manifest, sig)
}

type ManifestPair = (Vec<u8>, Vec<u8>);

fn try_load_from_dir(dir: &FsPath, rollout_id: &str) -> Result<Option<ManifestPair>, StatusCode> {
    let (manifest_path, sig_path) = manifest_paths(dir, rollout_id);
    let manifest_bytes = match std::fs::read(&manifest_path) {
        Ok(b) => b,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            tracing::warn!(
                rollout_id = %rollout_id,
                path = %manifest_path.display(),
                error = %err,
                "rollouts handler: read manifest failed",
            );
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    let sig_bytes = match std::fs::read(&sig_path) {
        Ok(b) => b,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // GOTCHA: manifest present but sig missing - refuse rather than serve unverifiable bytes.
            tracing::warn!(
                rollout_id = %rollout_id,
                "rollouts handler: signature file missing for present manifest",
            );
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
        Err(err) => {
            tracing::warn!(
                rollout_id = %rollout_id,
                error = %err,
                "rollouts handler: read signature failed",
            );
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    Ok(Some((manifest_bytes, sig_bytes)))
}

async fn load_pair(state: &AppState, rollout_id: &str) -> Result<ManifestPair, StatusCode> {
    if state.rollouts_dir.is_none() && state.rollouts_source.is_none() {
        tracing::debug!(
            rollout_id = %rollout_id,
            "rollouts handler: neither rollouts_dir nor rollouts_source configured; returning 503",
        );
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    if !looks_like_rollout_id(rollout_id) {
        return Err(StatusCode::NOT_FOUND);
    }

    if let Some(dir) = state.rollouts_dir.as_ref()
        && let Some((manifest_bytes, sig_bytes)) = try_load_from_dir(dir, rollout_id)?
    {
        return Ok((manifest_bytes, sig_bytes));
    }

    if let Some(source) = state.rollouts_source.as_ref() {
        match source.fetch_pair(rollout_id).await {
            Ok((manifest_bytes, sig_bytes)) => {
                tracing::info!(
                    rollout_id = %rollout_id,
                    "rollouts handler: fetched manifest pair from upstream source",
                );
                return Ok((manifest_bytes, sig_bytes));
            }
            Err(err) => {
                tracing::warn!(
                    rollout_id = %rollout_id,
                    error = %err,
                    "rollouts handler: upstream fetch failed",
                );
                return Err(StatusCode::BAD_GATEWAY);
            }
        }
    }

    Err(StatusCode::NOT_FOUND)
}

/// `GET /v1/rollouts/{rolloutId}` - manifest bytes; mTLS via router-level `require_cn_layer`.
pub(in crate::server) async fn manifest(
    State(state): State<Arc<AppState>>,
    Path(rollout_id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let (manifest_bytes, _sig) = load_pair(&state, &rollout_id).await?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok((StatusCode::OK, headers, Bytes::from(manifest_bytes)))
}

/// `GET /v1/rollouts/{rolloutId}/sig` - raw signature bytes.
pub(in crate::server) async fn signature(
    State(state): State<Arc<AppState>>,
    Path(rollout_id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    let (_manifest, sig_bytes) = load_pair(&state, &rollout_id).await?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    Ok((StatusCode::OK, headers, Bytes::from(sig_bytes)))
}

/// `GET /v1/rollouts` - enumerate active (non-superseded) rollouts with
/// per-host state pulled from `host_rollout_state` (DB-authoritative,
/// independent of the journal event window).
///
/// Operators (status renderers) use this for "what's actually deployed"
/// instead of inferring from journal `target=confirm` events - agent
/// confirms only fire on real dispatches, so converged-at-dispatch hosts
/// would otherwise look unconfirmed forever in journal-derived views.
pub(in crate::server) async fn list_active(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, StatusCode> {
    let db = state.db.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    // Per-host states sourced from `host_rollout_records` (RFC-0005 §5).
    // Legacy `host_dispatch_state` is gone; the new projection groups
    // rows by rollout_id and renders the 6-state machine value as-is.
    let rollouts_meta = db
        .rollouts()
        .list_in_flight()
        .map_err(internal_warn("list_in_flight rollouts query failed"))?;

    let rollouts: Vec<serde_json::Value> = rollouts_meta
        .into_iter()
        .map(|r| {
            let host_states: std::collections::HashMap<String, String> = db
                .host_rollout_records()
                .all_for_rollout(r.rollout_id.as_str())
                .unwrap_or_default()
                .into_iter()
                .map(|row| (row.hostname.clone(), format!("{:?}", row.state)))
                .collect();
            serde_json::json!({
                "rolloutId": r.rollout_id.as_str(),
                "channel": r.channel,
                "currentWave": r.current_wave,
                "createdAt": r.created_at,
                "hostStates": host_states,
            })
        })
        .collect();
    let body = serde_json::json!({ "rollouts": rollouts }).to_string();
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok((StatusCode::OK, headers, body))
}

/// `GET /v1/rollouts/{rolloutId}/lifecycle` - supersession state for the
/// rollout, sourced solely from the rollouts table. Returns 404 for any
/// rid not tracked there.
///
/// Distinct from the signed manifest endpoint because we can't inject
/// server-derived metadata into the signed bytes.
pub(in crate::server) async fn lifecycle(
    State(state): State<Arc<AppState>>,
    Path(rollout_id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    if !looks_like_rollout_id(&rollout_id) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let db = state.db.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let row = db
        .rollouts()
        .state(&rollout_id)
        .map_err(internal_warn("lifecycle: state query failed"))?;
    let row = row.ok_or(StatusCode::NOT_FOUND)?;
    let body = serde_json::json!({
        "rolloutId": rollout_id,
        "state": row.state.as_db_str(),
        "supersededAt": row.superseded_at.map(|t: chrono::DateTime<chrono::Utc>| t.to_rfc3339()),
        // `superseded_by` dropped in Phase 10a per RFC-0008 §6.3 + SR-3.
        // Operator-facing successor lookup migrates to an event_log walk
        // for `SuccessorOpened` events; not yet wired (v0.2.x follow-up).
        "supersededBy": serde_json::Value::Null,
        // Distinct from supersededAt - terminal_at fires on natural
        // convergence. UI consumers use this to gray out finished
        // rollouts; gates ignore it (they read host_states directly
        // from list_active).
        "terminalAt": row.terminal_at.map(|t: chrono::DateTime<chrono::Utc>| t.to_rfc3339()),
    })
    .to_string();
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok((StatusCode::OK, headers, body))
}

/// `GET /v1/rollouts/{rolloutId}/hosts` — per-host summary for a rollout.
///
/// Projects `host_rollout_records` into one entry per `(rollout, host)`
/// pair: state, target/current closure, dispatch + terminal timestamps.
/// Operator-facing read: "what state is each host in?" The CLI's
/// `nixfleet rollout hosts <id>` renders this as a table.
///
/// For the chronological event-log stream (engineer-facing replay
/// surface; RFC-0005 §10.5), see [`events`] / `GET /v1/rollouts/{id}/events`.
pub(in crate::server) async fn hosts(
    State(state): State<Arc<AppState>>,
    Path(rollout_id): Path<String>,
) -> Result<axum::Json<nixfleet_proto::RolloutHosts>, StatusCode> {
    if !looks_like_rollout_id(&rollout_id) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let db = state.db.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;

    // Per-host summary rows from host_rollout_records. v0.2 collapses to
    // one row per (rollout, host) because dispatch is now event-driven
    // (RFC-0005 §4) — multiple dispatches against the same pair are
    // re-emits of the same logical intent, not distinct rows. The `wave`
    // field is read from the verified manifest's `host_set` for that
    // host.
    let records = match db.host_rollout_records().all_for_rollout(&rollout_id) {
        Ok(r) => r,
        Err(err) => {
            tracing::error!(
                target: "rollout_hosts",
                %rollout_id,
                error = %err,
                "all_for_rollout failed",
            );
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    if records.is_empty() {
        return Err(StatusCode::NOT_FOUND);
    }

    // Wave-index lookup from the verified manifest (only available when
    // verified_fleet is primed). Missing manifest ⇒ wave 0 placeholder.
    let wave_by_host: std::collections::HashMap<String, u32> =
        match db.rollouts().state(&rollout_id) {
            Ok(Some(_)) => std::collections::HashMap::new(),
            _ => std::collections::HashMap::new(),
        };

    let mut hosts: Vec<nixfleet_proto::RolloutHostEntry> = Vec::with_capacity(records.len());
    for r in &records {
        let (terminal_state, terminal_at) = derive_terminal(r);
        hosts.push(nixfleet_proto::RolloutHostEntry {
            host: r.hostname.clone(),
            channel: r.channel.clone(),
            wave: wave_by_host.get(&r.hostname).copied().unwrap_or(0),
            target_closure_hash: r.target_closure.clone(),
            target_channel_ref: r.rollout_id.as_str().to_string(),
            dispatched_at: r.dispatched_at.to_rfc3339(),
            terminal_state,
            terminal_at,
        });
    }
    // Stable rendering order: wave asc, hostname asc.
    hosts.sort_by(|a, b| a.wave.cmp(&b.wave).then_with(|| a.host.cmp(&b.host)));

    // looks_like_rollout_id has already enforced the canonical
    // `channel@channel_ref` shape; split_once cannot return None here.
    let (channel, channel_ref) = rollout_id.split_once('@').unwrap();
    Ok(axum::Json(nixfleet_proto::RolloutHosts {
        rollout_id: nixfleet_proto::RolloutId::new(channel, channel_ref),
        hosts,
    }))
}

/// `GET /v1/rollouts/{rolloutId}/events` — chronological event-log
/// stream for a rollout (RFC-0005 §10.5 + Plan 04 §"Event log schema").
///
/// Returns every `event_log` row whose `rollout_id` matches, sorted by
/// `seq` ascending. Engineer-facing surface: feed the rows through
/// `nixfleet_state_machine::step` to reproduce per-host state
/// evolution; or query for specific kinds (`agent_event`, `effect`,
/// `gate_decision`, etc.) to debug a specific layer.
///
/// `?limit=N` caps the number of entries (default 1000 — sized for
/// ~5–8 events per host × ~125 hosts; raise for very large rollouts).
pub(in crate::server) async fn events(
    State(state): State<Arc<AppState>>,
    Path(rollout_id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<EventsQuery>,
) -> Result<axum::Json<nixfleet_proto::RolloutEvents>, StatusCode> {
    if !looks_like_rollout_id(&rollout_id) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let db = state.db.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let limit = query.limit.unwrap_or(1000).max(1);

    let rows = match db.event_log().query_by_rollout(&rollout_id, limit) {
        Ok(r) => r,
        Err(err) => {
            tracing::error!(
                target: "rollout_events",
                %rollout_id,
                error = %err,
                "event_log query_by_rollout failed",
            );
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    let mut events: Vec<nixfleet_proto::RolloutEventEntry> = Vec::with_capacity(rows.len());
    for row in rows {
        // event_log enforces JSON validity at insert (Phase 4 fix
        // f3fcb213); an unparsable payload is a corruption signal, not
        // expected operational state. Log + replace with a placeholder
        // so a single bad row doesn't 500 the whole trace.
        let payload = match serde_json::from_str::<serde_json::Value>(&row.payload) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(
                    target: "rollout_events",
                    seq = row.seq,
                    error = %err,
                    "event_log row payload failed JSON parse — emitting placeholder",
                );
                serde_json::json!({ "_parse_error": err.to_string(), "_raw": row.payload })
            }
        };
        events.push(nixfleet_proto::RolloutEventEntry {
            seq: row.seq,
            ts: row.ts.to_rfc3339(),
            kind: row.kind,
            host: row.host_id,
            payload,
        });
    }

    let (channel, channel_ref) = rollout_id.split_once('@').unwrap();
    Ok(axum::Json(nixfleet_proto::RolloutEvents {
        rollout_id: nixfleet_proto::RolloutId::new(channel, channel_ref),
        events,
    }))
}

#[derive(Debug, serde::Deserialize)]
pub struct EventsQuery {
    pub limit: Option<i64>,
}

/// Map a host_rollout_records row to the CLI's (terminal_state, terminal_at)
/// pair. Open = `None`.
fn derive_terminal(
    r: &nixfleet_state_machine::HostRolloutState,
) -> (Option<String>, Option<String>) {
    use nixfleet_state_machine::HostState;
    match r.state {
        HostState::Converged => (
            Some("converged".into()),
            r.converged_at.map(|t| t.to_rfc3339()),
        ),
        HostState::Reverted => (
            Some("rolled-back".into()),
            r.reverted_at.map(|t| t.to_rfc3339()),
        ),
        HostState::Failed => (Some("failed".into()), r.failed_at.map(|t| t.to_rfc3339())),
        HostState::Pending | HostState::Activating | HostState::Deferred | HostState::Soaking => {
            (None, None)
        }
    }
}

#[cfg(test)]
mod tests {
    //! `/v1/rollouts/{id}/hosts` projection from host_rollout_records.

    use super::*;
    use crate::db::Db;
    use nixfleet_state_machine::{HostRolloutState as SmState, HostState};

    /// Canonical RFC-0008 §6.3 RolloutId, valid per `looks_like_rollout_id`.
    const TEST_ROLLOUT: &str = "stable@deadbeef";

    #[test]
    fn validator_accepts_canonical_rollout_id() {
        assert!(looks_like_rollout_id("stable@deadbeef"));
    }

    #[test]
    fn validator_accepts_long_channel_ref() {
        // 40-char SHA1 + 64-char SHA256 length variants both pass the
        // character class; the validator does not hardcode a ref length.
        assert!(looks_like_rollout_id(
            "stable@deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        ));
    }

    #[test]
    fn validator_rejects_legacy_sha256_hex_alone() {
        // Reject the legacy hex-only format (64 lowercase hex, no `@`
        // separator) per RFC-0008 §6.3.
        assert!(!looks_like_rollout_id(
            "abc1234567890123456789012345678901234567890123456789012345678901",
        ));
    }

    #[test]
    fn validator_rejects_empty_channel() {
        assert!(!looks_like_rollout_id("@deadbeef"));
    }

    #[test]
    fn validator_rejects_empty_ref() {
        assert!(!looks_like_rollout_id("stable@"));
    }

    #[test]
    fn validator_rejects_path_traversal() {
        assert!(!looks_like_rollout_id("../../etc/passwd"));
    }

    #[test]
    fn validator_rejects_slash_in_channel() {
        assert!(!looks_like_rollout_id("stable/branch@abc"));
    }

    #[test]
    fn validator_rejects_uppercase_hex_in_ref() {
        assert!(!looks_like_rollout_id("stable@DEADBEEF"));
    }

    #[test]
    fn validator_rejects_no_separator() {
        assert!(!looks_like_rollout_id("stableABC"));
    }

    #[test]
    fn validator_rejects_multiple_separators() {
        assert!(!looks_like_rollout_id("stable@beta@abc"));
    }

    fn fresh_state() -> Arc<AppState> {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        Arc::new(AppState {
            db: Some(Arc::new(db)),
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn hosts_404_when_rollout_unknown() {
        let state = fresh_state();
        let err = hosts(State(state), axum::extract::Path(TEST_ROLLOUT.into()))
            .await
            .unwrap_err();
        assert_eq!(err, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn hosts_projects_one_entry_per_host_with_terminal_state() {
        let state = fresh_state();
        let db = state.db.clone().unwrap();
        let rollout = TEST_ROLLOUT;

        let now = chrono::Utc::now();
        let mut h1 = SmState::new_pending(
            rollout.into(),
            "h1".into(),
            "stable".into(),
            "h1-closure".into(),
            now,
            now + chrono::Duration::minutes(5),
        );
        h1.state = HostState::Converged;
        h1.current_closure = Some("h1-closure".into());
        h1.converged_at = Some(now + chrono::Duration::minutes(10));
        db.host_rollout_records().upsert(&h1).unwrap();

        let h2 = SmState::new_pending(
            rollout.into(),
            "h2".into(),
            "stable".into(),
            "h2-closure".into(),
            now,
            now + chrono::Duration::minutes(5),
        );
        db.host_rollout_records().upsert(&h2).unwrap();

        let resp = hosts(State(state), axum::extract::Path(rollout.into()))
            .await
            .unwrap();
        let payload = resp.0;
        assert_eq!(payload.rollout_id.as_str(), rollout);
        assert_eq!(payload.hosts.len(), 2);
        // Stable order: wave asc, hostname asc → h1 then h2.
        assert_eq!(payload.hosts[0].host, "h1");
        assert_eq!(
            payload.hosts[0].terminal_state.as_deref(),
            Some("converged"),
        );
        assert!(payload.hosts[0].terminal_at.is_some());
        assert_eq!(payload.hosts[1].host, "h2");
        assert_eq!(payload.hosts[1].terminal_state, None, "h2 still pending");
        assert_eq!(payload.hosts[1].terminal_at, None);
    }
}
