-- Phase 4 follow-up: per-host rollout state for the soak timer
-- (RFC-0002 §3.2 / §4.4). The reconciler's host-state machine in
-- `crates/nixfleet-reconciler/src/host_state.rs` reads this through
-- the projection (step 2 of gap #2 in
-- docs/roadmap/0002-v0.2-completeness-gaps.md); the Healthy → Soaked
-- transition consults `last_healthy_since` against
-- `wave.soak_minutes` (step 3).
--
-- Sibling to `pending_confirms`, intentionally not an extension: the
-- pending_confirms state column tracks the dispatch-acknowledgement
-- lifecycle (`pending|confirmed|rolled-back|cancelled`) under a
-- CHECK constraint, while host_state tracks the per-host rollout
-- machine from RFC-0002 §3.2. Confirmed rows in pending_confirms
-- are terminal; soak / converge happen *after* confirm. Keeping the
-- two state machines separate avoids conflating their lifecycles
-- and leaves room for probe results (Phase 7) to land here later.
--
-- `last_healthy_since` is NULL until the host enters Healthy. The
-- confirm handler stamps it on /v1/agent/confirm acceptance; the
-- checkin handler clears it (sets NULL) when the host's reported
-- current_generation no longer matches the rollout's target closure
-- — i.e. the host has left Healthy.
--
-- `host_state` defaults to 'Dispatched' so the row's existence
-- (created at confirm time) maps to a sane value if anything reads
-- it before Healthy is recorded. State transitions across the
-- machine land in step 3 (reconciler arm + CP-side action handler).

CREATE TABLE host_rollout_state (
    rollout_id          TEXT NOT NULL,
    hostname            TEXT NOT NULL,
    host_state          TEXT NOT NULL DEFAULT 'Dispatched'
        CHECK (host_state IN ('Queued', 'Dispatched', 'Activating',
                              'ConfirmWindow', 'Healthy', 'Soaked',
                              'Converged', 'Reverted', 'Failed')),
    last_healthy_since  TEXT,
    updated_at          TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (rollout_id, hostname)
);

CREATE INDEX idx_host_rollout_state_hostname
    ON host_rollout_state(hostname);
