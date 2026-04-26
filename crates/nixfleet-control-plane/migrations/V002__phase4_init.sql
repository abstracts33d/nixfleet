-- Phase 4 PR-1: SQLite foundation.
--
-- Three tables land here: token_replay (already in-memory in
-- Phase 3 — promote to DB), cert_revocations (Phase 3 RFC §2 calls
-- for it but we never built the data structure), and pending_confirms
-- (the activation-deadline tracker that /v1/agent/confirm needs in
-- Phase 4 PR-2). Subsequent migrations add hosts + rollouts tables
-- when the dispatch loop is built (Phase 4 PR-3+).
--
-- The schema is intentionally additive: every table can be queried
-- standalone, no foreign keys yet (rollouts → hosts → confirms
-- relationships land when those tables exist). FK ON gets enforced
-- at the connection level (PRAGMA foreign_keys = ON) so the column
-- declarations are forward-compat.

-- Bootstrap-token replay set. nonce → first_seen_at.
-- Was an in-memory HashSet on AppState in PR-5; promoted here so a
-- CP restart doesn't lose replay protection. Tokens have a 24h
-- default validity (D6), so a row older than 24h is a no-op (the
-- cleanup task can prune those — Phase 4 PR-2).
CREATE TABLE token_replay (
    nonce       TEXT PRIMARY KEY,
    hostname    TEXT NOT NULL,
    first_seen  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_token_replay_first_seen
    ON token_replay(first_seen);

-- Cert revocation set per RFC-0003 §2. hostname → notBefore.
-- Agents whose cert's notBefore predates the entry are rejected at
-- mTLS time. Simpler than CRLs; works because cert lifetime is
-- short (30d default).
CREATE TABLE cert_revocations (
    hostname     TEXT PRIMARY KEY,
    not_before   TEXT NOT NULL,
    reason       TEXT,
    revoked_at   TEXT NOT NULL DEFAULT (datetime('now')),
    revoked_by   TEXT
);

-- Pending activation confirmations. Key shape per RFC-0003 §4.2:
-- agent posts /v1/agent/confirm with hostname + rollout + wave +
-- generation; CP records the dispatch and waits for confirm within
-- confirm_window_secs. PR-1 just creates the schema; PR-2 wires the
-- agent activation loop, PR-3 wires the rollback timer.
CREATE TABLE pending_confirms (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    hostname            TEXT NOT NULL,
    rollout_id          TEXT NOT NULL,
    wave                INTEGER NOT NULL,
    target_closure_hash TEXT NOT NULL,
    target_channel_ref  TEXT NOT NULL,
    dispatched_at       TEXT NOT NULL DEFAULT (datetime('now')),
    confirm_deadline    TEXT NOT NULL,
    confirmed_at        TEXT,
    state               TEXT NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'confirmed', 'rolled-back', 'cancelled'))
);

CREATE INDEX idx_pending_confirms_hostname
    ON pending_confirms(hostname, dispatched_at DESC);

CREATE INDEX idx_pending_confirms_deadline
    ON pending_confirms(confirm_deadline)
    WHERE state = 'pending';
