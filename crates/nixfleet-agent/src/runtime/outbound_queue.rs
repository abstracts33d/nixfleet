//! Disk-backed durable outbound event queue (Plan 07 locked-in
//! decision; RFC-0008 §9.7).
//!
//! Each event is one file on disk under `{state_dir}/outbound-queue/`,
//! named `{seq:020}-{hostname}-{rollout}-{event_kind}.json`. Zero-
//! padded seq so directory listing is in seq-order. Atomic write via
//! tmp + rename so a crash mid-write leaves no partially-formed file
//! visible to the drainer. On successful POST, the file is deleted.
//!
//! Properties:
//!   - Survives agent process crashes: every outbound event hits disk
//!     before the network call returns.
//!   - Single fsync per event: the rename hops the rename-survives-
//!     reboot guarantee on POSIX filesystems; the data fsync ensures
//!     the bytes are durable before the rename swings the pointer.
//!   - Replay-from-seq friendly: a CP `X-Nixfleet-Replay-From: N`
//!     response triggers a directory scan for files with seq ≥ N.
//!   - Crash mid-write: a partial `.tmp` file is invisible to
//!     [`OutboundQueue::scan_pending`] because the filename pattern
//!     filters out non-`.json` paths. The next restart's drainer
//!     picks up where it left off.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use nixfleet_state_machine::OutboundAgentEvent;
use serde::{Deserialize, Serialize};

/// One entry in the on-disk queue. Persisted as JSON via serde.
/// `payload` is the typed wire event (RFC-0011 §2 lift: the wire
/// envelope + AgentEvent live in `nixfleet-proto`, both sides of the
/// agent <-> CP boundary import the same types). The outbound worker
/// wraps each QueuedEvent in an `AgentEventEnvelope` at POST time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QueuedEvent {
    pub seq: u64,
    pub hostname: String,
    pub rollout_id: nixfleet_proto::RolloutId,
    pub event_kind: String,
    pub created_at: DateTime<Utc>,
    /// Typed wire event. Same shape both sides verify against.
    pub payload: nixfleet_proto::AgentEvent,
}

/// Disk-backed queue handle. Cheap to clone via `Arc`.
#[derive(Clone)]
pub struct OutboundQueue {
    dir: PathBuf,
}

impl OutboundQueue {
    /// Open / create the queue directory. Idempotent.
    pub fn open(state_dir: &Path) -> Result<Self> {
        let dir = state_dir.join("outbound-queue");
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create outbound-queue dir {}", dir.display()))?;
        Ok(Self { dir })
    }

    /// Atomically persist an event. fsync before rename ensures the
    /// payload bytes hit disk before the directory entry flips; a
    /// crash between fsync and rename leaves a `.tmp` file that the
    /// drainer ignores.
    pub fn enqueue(&self, event: &QueuedEvent) -> Result<PathBuf> {
        let final_name = filename_for(event);
        let final_path = self.dir.join(&final_name);
        let tmp_path = self.dir.join(format!("{final_name}.tmp"));

        let bytes = serde_json::to_vec_pretty(event)
            .with_context(|| format!("serialize QueuedEvent seq={}", event.seq))?;
        write_atomic(&tmp_path, &final_path, &bytes)
            .with_context(|| format!("atomic write {}", final_path.display()))?;
        Ok(final_path)
    }

    /// All pending events, sorted ascending by seq. Filenames sort
    /// lexicographically thanks to the 20-char zero-padded seq prefix;
    /// the BTreeMap-by-seq pass below is belt-and-braces in case the
    /// scan picks up files with unexpected sort order (rare on POSIX
    /// but defensible).
    pub fn scan_pending(&self) -> Result<Vec<QueuedEvent>> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(it) => it,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(anyhow::anyhow!("read_dir {}: {err}", self.dir.display())),
        };
        let mut by_seq: BTreeMap<u64, QueuedEvent> = BTreeMap::new();
        for entry in entries {
            let entry = entry.context("read_dir entry")?;
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                continue;
            };
            // Filter to .json (rejects .tmp partials).
            if !name_str.ends_with(".json") || name_str.ends_with(".json.tmp") {
                continue;
            }
            let path = entry.path();
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(err) => {
                    tracing::warn!(
                        target: "outbound_queue",
                        path = %path.display(),
                        error = %err,
                        "scan_pending: read failed; skipping",
                    );
                    continue;
                }
            };
            let event: QueuedEvent = match serde_json::from_slice(&bytes) {
                Ok(e) => e,
                Err(err) => {
                    tracing::warn!(
                        target: "outbound_queue",
                        path = %path.display(),
                        error = %err,
                        "scan_pending: parse failed; skipping (operator should rm the bad file)",
                    );
                    continue;
                }
            };
            by_seq.insert(event.seq, event);
        }
        Ok(by_seq.into_values().collect())
    }

    /// Delete the on-disk file for `event`. Called after a successful
    /// POST to `/v1/agent/events`.
    pub fn mark_sent(&self, event: &QueuedEvent) -> Result<()> {
        let path = self.dir.join(filename_for(event));
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            // Already gone (concurrent drainer or operator hand-removed)
            // is fine — the postcondition is "file is absent".
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(anyhow::anyhow!("remove_file {}: {err}", path.display())),
        }
    }

    /// Drop all queued events. Test-only entry point.
    #[cfg(test)]
    pub fn clear(&self) -> Result<()> {
        let entries = std::fs::read_dir(&self.dir).context("read_dir for clear")?;
        for entry in entries {
            let entry = entry?;
            let _ = std::fs::remove_file(entry.path());
        }
        Ok(())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// Map an `OutboundAgentEvent` to its kebab-case `event_kind`
/// discriminator (used in the filename + the OutboundEventKind enum
/// in `db/event_log` on the CP side).
pub fn outbound_event_kind(payload: &OutboundAgentEvent) -> &'static str {
    match payload {
        OutboundAgentEvent::DispatchAck { .. } => "DispatchAck",
        OutboundAgentEvent::ActivationStarted { .. } => "ActivationStarted",
        OutboundAgentEvent::ActivationCompleted { .. } => "ActivationCompleted",
        OutboundAgentEvent::ActivationFailed { .. } => "ActivationFailed",
        OutboundAgentEvent::ProbeTopologyDeclared { .. } => "ProbeTopologyDeclared",
        OutboundAgentEvent::ProbeObservedFirst { .. } => "ProbeObservedFirst",
        OutboundAgentEvent::ProbeResult { .. } => "ProbeResult",
        OutboundAgentEvent::ProbeFailureFirst { .. } => "ProbeFailureFirst",
        OutboundAgentEvent::Failed { .. } => "Failed",
        OutboundAgentEvent::RollbackComplete { .. } => "RollbackComplete",
        OutboundAgentEvent::Converged { .. } => "Converged",
    }
}

/// Read the `seq` field off an `OutboundAgentEvent`.
pub fn outbound_event_seq(payload: &OutboundAgentEvent) -> u64 {
    match payload {
        OutboundAgentEvent::DispatchAck { seq, .. }
        | OutboundAgentEvent::ActivationStarted { seq, .. }
        | OutboundAgentEvent::ActivationCompleted { seq, .. }
        | OutboundAgentEvent::ActivationFailed { seq, .. }
        | OutboundAgentEvent::ProbeTopologyDeclared { seq, .. }
        | OutboundAgentEvent::ProbeObservedFirst { seq, .. }
        | OutboundAgentEvent::ProbeResult { seq, .. }
        | OutboundAgentEvent::ProbeFailureFirst { seq, .. }
        | OutboundAgentEvent::Failed { seq, .. }
        | OutboundAgentEvent::RollbackComplete { seq, .. }
        | OutboundAgentEvent::Converged { seq, .. } => *seq,
    }
}

/// `{seq:020}-{hostname}-{rollout}-{event_kind}.json`. The
/// zero-padded seq gives lexicographic = chronological filename order;
/// the `.json` suffix is what `scan_pending` filters on (vs `.tmp`).
fn filename_for(event: &QueuedEvent) -> String {
    let hostname = sanitize(&event.hostname);
    let rollout = sanitize(event.rollout_id.as_str());
    let kind = sanitize(&event.event_kind);
    format!("{:020}-{hostname}-{rollout}-{kind}.json", event.seq)
}

/// Filename sanitisation: replace path separators + spaces with `_`.
/// Belt-and-braces; the wire types should already constrain these
/// strings to URL-safe shapes, but we don't trust the input.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '@' | '.' => c,
            _ => '_',
        })
        .collect()
}

fn write_atomic(tmp: &Path, final_path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(tmp)
            .with_context(|| format!("open {}", tmp.display()))?;
        f.write_all(bytes)
            .with_context(|| format!("write {}", tmp.display()))?;
        f.sync_all()
            .with_context(|| format!("fsync {}", tmp.display()))?;
    }
    std::fs::rename(tmp, final_path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), final_path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::TempDir;

    fn t(seq: u64) -> QueuedEvent {
        QueuedEvent {
            seq,
            hostname: "host-05".into(),
            rollout_id: "stable@deadbeef".into(),
            event_kind: "ActivationCompleted".into(),
            created_at: Utc.with_ymd_and_hms(2026, 5, 16, 1, 0, 0).unwrap(),
            payload: nixfleet_proto::AgentEvent::ActivationCompleted {
                observed_current_closure: "closure-a".into(),
                exit_code: 0,
                completed_at: Utc.with_ymd_and_hms(2026, 5, 16, 1, 0, 0).unwrap(),
                seq,
            },
        }
    }

    #[test]
    fn enqueue_then_scan_round_trip() {
        let dir = TempDir::new().unwrap();
        let q = OutboundQueue::open(dir.path()).unwrap();
        q.enqueue(&t(1)).unwrap();
        q.enqueue(&t(2)).unwrap();
        q.enqueue(&t(3)).unwrap();
        let pending = q.scan_pending().unwrap();
        assert_eq!(pending.len(), 3);
        assert_eq!(pending[0].seq, 1);
        assert_eq!(pending[1].seq, 2);
        assert_eq!(pending[2].seq, 3);
    }

    #[test]
    fn mark_sent_removes_from_queue() {
        let dir = TempDir::new().unwrap();
        let q = OutboundQueue::open(dir.path()).unwrap();
        q.enqueue(&t(1)).unwrap();
        q.enqueue(&t(2)).unwrap();
        q.mark_sent(&t(1)).unwrap();
        let pending = q.scan_pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].seq, 2);
    }

    #[test]
    fn mark_sent_is_idempotent_when_file_gone() {
        let dir = TempDir::new().unwrap();
        let q = OutboundQueue::open(dir.path()).unwrap();
        q.enqueue(&t(1)).unwrap();
        q.mark_sent(&t(1)).unwrap();
        // Second call: file is already gone, must succeed.
        q.mark_sent(&t(1)).unwrap();
    }

    #[test]
    fn crash_mid_write_leaves_no_visible_event() {
        // Simulate a crash between fsync and rename: drop a .tmp file
        // with arbitrary contents. scan_pending must ignore it; the
        // queue is otherwise empty so no event is delivered.
        let dir = TempDir::new().unwrap();
        let q = OutboundQueue::open(dir.path()).unwrap();
        let tmp_path = q
            .dir()
            .join("00000000000000000007-host-x-rollout-y-Kind.json.tmp");
        std::fs::write(&tmp_path, b"partial garbage").unwrap();
        let pending = q.scan_pending().unwrap();
        assert!(
            pending.is_empty(),
            "partial .tmp must not surface as a queued event",
        );
        // And no `.json` file exists for that seq either.
        let listing: Vec<_> = std::fs::read_dir(q.dir())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert_eq!(listing.len(), 1);
        assert_eq!(
            listing[0].to_str().unwrap(),
            tmp_path.file_name().unwrap().to_str().unwrap()
        );
    }

    #[test]
    fn replay_from_seq_yields_correct_subset() {
        // Operator scenario: CP returns Replay-From=2; agent re-POSTs
        // events with seq ≥ 2. scan_pending is total; consumer filters.
        let dir = TempDir::new().unwrap();
        let q = OutboundQueue::open(dir.path()).unwrap();
        for seq in 1..=5 {
            q.enqueue(&t(seq)).unwrap();
        }
        let pending = q.scan_pending().unwrap();
        let replay: Vec<_> = pending.into_iter().filter(|e| e.seq >= 2).collect();
        assert_eq!(replay.len(), 4);
        assert_eq!(replay[0].seq, 2);
    }

    #[test]
    fn enqueue_overwrites_same_seq() {
        // The atomic rename hops over any prior file with the same
        // name — this is the recoverable-after-crash property. A
        // duplicate enqueue is just a no-op update.
        let dir = TempDir::new().unwrap();
        let q = OutboundQueue::open(dir.path()).unwrap();
        q.enqueue(&t(1)).unwrap();
        // Different payload, same seq.
        let mut second = t(1);
        let perturbed_payload = nixfleet_proto::AgentEvent::ActivationCompleted {
            observed_current_closure: "closure-b".into(),
            exit_code: 99,
            completed_at: Utc.with_ymd_and_hms(2026, 5, 16, 1, 0, 0).unwrap(),
            seq: 1,
        };
        second.payload = perturbed_payload.clone();
        q.enqueue(&second).unwrap();
        let pending = q.scan_pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].payload, perturbed_payload);
    }
}
