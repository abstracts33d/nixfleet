-- Issue #60 — durable per-host event log.
--
-- Backs `AppState.host_reports` (the in-memory ring buffer that
-- previously accepted ComplianceFailure / RuntimeGateError events
-- but lost them on CP restart, silently unblocking any held wave
-- promotion until the next agent activation re-fired the gate).
-- ARCHITECTURE.md §6 Phase 10 classified host_reports as soft-state
-- with a known persistence gap; this migration closes it.
--
-- The table is the persistence layer. The handler writes through to
-- both this table and the in-memory ring (ring stays for hot-path
-- latency in dispatch decisions; SQLite is the durable record).
-- CP startup hydrates the ring from the most recent N rows per host.
--
-- `signature_status` carries the `evidence_verify::SignatureStatus`
-- verdict in kebab-case (`verified` / `unsigned` / `no-pubkey` /
-- `mismatch` / `malformed` / `wrong-algorithm`) — same shape as the
-- enum's serde representation. NULL for events that don't carry the
-- contract (ActivationFailed pre-#61, etc.).
--
-- `report_json` is the raw `ReportRequest` envelope as serialised
-- JSON. We reconstruct typed events on read; storing the canonical
-- envelope keeps us forward-compatible with proto additions without
-- migration churn.
--
-- TTL eviction lives in `prune_timer.rs` (issue #52); default 7 days
-- mirrors `pending_confirms`.

CREATE TABLE host_reports (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    hostname           TEXT NOT NULL,
    event_id           TEXT NOT NULL UNIQUE,
    received_at        TEXT NOT NULL,                  -- RFC3339 UTC
    event_kind         TEXT NOT NULL,                  -- ReportEvent kebab-case discriminator
    rollout            TEXT,                           -- nullable; matches ReportRequest.rollout
    signature_status   TEXT,                           -- kebab-case SignatureStatus, NULL for non-signed events
    report_json        TEXT NOT NULL                   -- full ReportRequest envelope
);

CREATE INDEX idx_host_reports_hostname ON host_reports(hostname);
CREATE INDEX idx_host_reports_received ON host_reports(received_at);
