-- Split pending_confirms into operational + audit (#81).
--
-- The legacy table conflated two roles: "what is host X currently
-- being asked to do" (operational, mutated in place) and "what
-- dispatches has host X been through" (audit, append-only). The
-- conflation forced the post-hoc cleanup hooks added in 9f9e339 and
-- 6d267de to keep the snapshot's host counts honest. Splitting the
-- table moves both concerns to the right shape so the cleanup hooks
-- are no longer needed.
--
-- host_dispatch_state: PRIMARY KEY hostname, UPSERTed on every new
-- dispatch, terminal states stay parked on the row until the next
-- dispatch overwrites them. active_rollouts_snapshot reads only this
-- table and filters terminal states out.
--
-- dispatch_history: append-only, one row per dispatch, terminal_state
-- + terminal_at populated when the host hits a terminal for THAT
-- dispatch. Pruned by retention (90d default).

CREATE TABLE host_dispatch_state (
    hostname              TEXT PRIMARY KEY,
    rollout_id            TEXT NOT NULL,
    channel               TEXT NOT NULL,
    wave                  INTEGER NOT NULL,
    target_closure_hash   TEXT NOT NULL,
    target_channel_ref    TEXT NOT NULL,
    state                 TEXT NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'confirmed', 'rolled-back', 'cancelled')),
    dispatched_at         TEXT NOT NULL DEFAULT (datetime('now')),
    confirm_deadline      TEXT NOT NULL,
    confirmed_at          TEXT
);

CREATE INDEX idx_host_dispatch_state_rollout
    ON host_dispatch_state(rollout_id);

-- Partial index covers the rollback timer's deadline scan; matches
-- the V002 partial-index shape on pending_confirms.
CREATE INDEX idx_host_dispatch_state_deadline
    ON host_dispatch_state(confirm_deadline)
    WHERE state = 'pending';

CREATE TABLE dispatch_history (
    id                    INTEGER PRIMARY KEY AUTOINCREMENT,
    hostname              TEXT NOT NULL,
    rollout_id            TEXT NOT NULL,
    channel               TEXT NOT NULL,
    wave                  INTEGER NOT NULL,
    target_closure_hash   TEXT NOT NULL,
    target_channel_ref    TEXT NOT NULL,
    dispatched_at         TEXT NOT NULL DEFAULT (datetime('now')),
    terminal_state        TEXT
        CHECK (terminal_state IN ('converged', 'rolled-back', 'cancelled')),
    terminal_at           TEXT
);

CREATE INDEX dispatch_history_hostname_idx
    ON dispatch_history(hostname);
CREATE INDEX dispatch_history_rollout_idx
    ON dispatch_history(rollout_id);
CREATE INDEX dispatch_history_dispatched_idx
    ON dispatch_history(dispatched_at);

-- Audit copy: every legacy row becomes a history entry. Terminal
-- rows ('rolled-back' / 'cancelled') get terminal_at backfilled from
-- dispatched_at — the legacy schema didn't track a precise terminal
-- timestamp (mark_rolled_back only updated the state column). The
-- bound is a confirm window (default 120s), affecting only the 90-day
-- retention pruner, not correctness.
INSERT INTO dispatch_history (
    hostname, rollout_id, channel, wave,
    target_closure_hash, target_channel_ref,
    dispatched_at, terminal_state, terminal_at
)
SELECT
    hostname, rollout_id, channel, wave,
    target_closure_hash, target_channel_ref,
    dispatched_at,
    CASE state
        WHEN 'rolled-back' THEN 'rolled-back'
        WHEN 'cancelled'   THEN 'cancelled'
        ELSE NULL
    END,
    CASE state
        WHEN 'rolled-back' THEN dispatched_at
        WHEN 'cancelled'   THEN dispatched_at
        ELSE NULL
    END
FROM pending_confirms;

-- Operational seed: keep only the most recent row per hostname.
-- ROW_NUMBER() over (PARTITION BY hostname ORDER BY dispatched_at DESC,
-- id DESC) deterministically resolves sub-second ties (id is
-- AUTOINCREMENT, so newer = larger id).
INSERT INTO host_dispatch_state (
    hostname, rollout_id, channel, wave,
    target_closure_hash, target_channel_ref,
    state, dispatched_at, confirm_deadline, confirmed_at
)
SELECT
    hostname, rollout_id, channel, wave,
    target_closure_hash, target_channel_ref,
    state, dispatched_at, confirm_deadline, confirmed_at
FROM (
    SELECT pc.*,
           ROW_NUMBER() OVER (
               PARTITION BY hostname
               ORDER BY dispatched_at DESC, id DESC
           ) AS rn
    FROM pending_confirms pc
) ranked
WHERE rn = 1;

DROP TABLE pending_confirms;
