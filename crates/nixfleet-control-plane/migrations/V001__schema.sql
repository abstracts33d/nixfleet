-- nixfleet-control-plane v0.2 baseline schema.
--
-- v0.2 is a hard rewrite of the rollout state machine (RFC-0008 §3 +
-- RFC-0009 §3). The migration history that walked the v0.1 schema
-- forward (V001 + V002–V009) collapsed into this single file: the
-- production migration path is "fresh DB on operator wipe" per RFC-0009
-- §12, so there is nothing to preserve.
--
-- Tables (alphabetical):
--   cert_revocations        Agent cert revocation list. Refreshed from
--                           the signed revocations.json sidecar; rows
--                           live until their cert's notBefore.
--   dispatch_queue          Per-(hostname, rollout_id) pending Dispatch
--                           payload. Applier UPSERTs on
--                           PlanAction::QueueDispatch; the long-poll
--                           handler `GET /v1/agent/dispatch` drains via
--                           atomic SELECT+DELETE.
--   event_log               Append-only canonical audit + replay log.
--                           Every PlanAction, Effect, GateDecision, and
--                           inbound agent event lands here. Producers
--                           supply `ts` (no SQL DEFAULT — FakeClock
--                           test parity, see RFC-0009 §8).
--   host_rollout_records    Per-(rollout, host) reducer state. Mirrors
--                           nixfleet_state_machine::HostRolloutState
--                           field-for-field. The 6-state CHECK forbids
--                           the v0.1 9-variant set (Queued, Dispatched,
--                           ConfirmWindow, Healthy, Soaked are gone).
--   probe_failures          Per-(rollout, host) typed denormalization
--                           of enforce-mode probe failures, derived
--                           from event_log via 9b's applier co-write.
--                           Backs the compliance-wave gate
--                           (RFC-0010 §7.2). 9a baseline: schema only,
--                           unwritten until 9b.
--   quarantined_closures    Per-(channel, closure_hash) bad-SHA list.
--                           Append-only derived view (RFC-0012 §6.4): one
--                           row per RollbackComplete event; the
--                           triggering_event_log_seq FK proves re-
--                           derivability. Older `reason`/`cleared_at`
--                           columns are gone — quarantines are append-
--                           only and reason lives in the triggering
--                           event_log payload.
--   rollouts                Per-rollout lifecycle as a derived view of
--                           event_log (RFC-0012 §6.3). The `state` enum
--                           column drives lifecycle (8 states per
--                           RFC-0012 §3); opened_event_log_seq +
--                           last_transition_event_log_seq FK back to
--                           event_log prove re-derivability.
--   token_replay            Bootstrap-token nonce replay defence
--                           (24h TTL, pruned by `prune_timer`).

-- ─────────────────────────────────────────────────────────────────────
-- token_replay
-- ─────────────────────────────────────────────────────────────────────
CREATE TABLE token_replay (
    nonce       TEXT PRIMARY KEY,
    hostname    TEXT NOT NULL,
    first_seen  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_token_replay_first_seen
    ON token_replay(first_seen);

-- ─────────────────────────────────────────────────────────────────────
-- cert_revocations
-- ─────────────────────────────────────────────────────────────────────
CREATE TABLE cert_revocations (
    hostname     TEXT PRIMARY KEY,
    not_before   TEXT NOT NULL,
    reason       TEXT,
    revoked_at   TEXT NOT NULL DEFAULT (datetime('now')),
    revoked_by   TEXT
);

-- ─────────────────────────────────────────────────────────────────────
-- rollouts (RFC-0012 §6.3 — derived view of event_log)
-- ─────────────────────────────────────────────────────────────────────
-- 8-state enum (RFC-0012 §3) drives lifecycle; opened_event_log_seq and
-- last_transition_event_log_seq are FK-back to canonical state.
--
-- FK columns are NULL-able under the v0.2.1 baseline (RFC-0012 §6.1
-- item 3): the event_log writer is a fire-and-forget bounded-mpsc task
-- so the applier doesn't know `seq` at co-write time. Same pattern as
-- probe_failures.event_log_seq below. v0.2.1-followups #1 tightens
-- this once the writer gains synchronous seq return.

CREATE TABLE rollouts (
    rollout_id            TEXT PRIMARY KEY,
    channel               TEXT NOT NULL,
    target_ref            TEXT NOT NULL,
    state                 TEXT NOT NULL
        CHECK (state IN ('Opening', 'Active', 'Converging', 'Terminal',
                         'Reverted', 'Failed', 'Superseded', 'Pruned')),
    current_wave          INTEGER NOT NULL DEFAULT 0,
    opened_event_log_seq  INTEGER
                          REFERENCES event_log(seq) ON DELETE SET NULL,
    last_transition_event_log_seq INTEGER
                          REFERENCES event_log(seq) ON DELETE SET NULL,
    opened_at             TEXT NOT NULL,
    terminal_at           TEXT,
    superseded_at         TEXT
);

CREATE INDEX rollouts_channel_state
    ON rollouts(channel, state);

CREATE INDEX rollouts_in_flight
    ON rollouts(state)
    WHERE state IN ('Opening', 'Active', 'Converging', 'Reverted', 'Failed');

-- ─────────────────────────────────────────────────────────────────────
-- quarantined_closures (RFC-0012 §6.4 — derived view of event_log)
-- ─────────────────────────────────────────────────────────────────────
-- Append-only: one row per RollbackComplete event (RFC-0008 §4.2). The
-- triggering_event_log_seq FK proves re-derivability — walking event_log
-- for RollbackComplete events reconstructs the table from empty.
--
-- NULL-able for the same reason rollouts FKs are (v0.2.1 baseline,
-- §6.1 item 3 + v0.2.1-followups #1).

CREATE TABLE quarantined_closures (
    channel                   TEXT NOT NULL,
    closure_hash              TEXT NOT NULL,
    quarantined_at            TEXT NOT NULL,
    triggering_event_log_seq  INTEGER
                              REFERENCES event_log(seq) ON DELETE SET NULL,
    PRIMARY KEY (channel, closure_hash)
);

CREATE INDEX quarantined_closures_active
    ON quarantined_closures(channel);

-- ─────────────────────────────────────────────────────────────────────
-- host_rollout_records (RFC-0008 §5 / RFC-0009 §5)
-- ─────────────────────────────────────────────────────────────────────
CREATE TABLE host_rollout_records (
    rollout_id                    TEXT NOT NULL,
    hostname                      TEXT NOT NULL,
    channel                       TEXT NOT NULL,
    state                         TEXT NOT NULL
        CHECK (state IN ('Pending', 'Activating', 'Soaking',
                         'Converged', 'Failed', 'Reverted')),

    target_closure                TEXT NOT NULL,
    current_closure_at_dispatch   TEXT,
    current_closure               TEXT,
    reverted_to                   TEXT,

    dispatched_at                 TEXT NOT NULL,
    dispatch_acked_at             TEXT,
    activation_started_at         TEXT,
    activation_completed_at       TEXT,
    activation_failed_at          TEXT,
    probe_observed_first_at       TEXT,
    probe_failure_first_at        TEXT,
    soak_due_at                   TEXT,
    converged_at                  TEXT,
    failed_at                     TEXT,
    policy_applied                TEXT
        CHECK (policy_applied IS NULL
               OR policy_applied IN ('halt', 'rollback-and-halt')),
    reverted_at                   TEXT,

    probes_json                   TEXT NOT NULL DEFAULT '{}',
    last_event_seq                INTEGER NOT NULL DEFAULT 0,

    PRIMARY KEY (rollout_id, hostname)
);

CREATE INDEX idx_host_rollout_records_by_host
    ON host_rollout_records(hostname);

CREATE INDEX idx_host_rollout_records_by_channel_state
    ON host_rollout_records(channel, state);

-- ─────────────────────────────────────────────────────────────────────
-- event_log (RFC-0008 §4.3)
-- ─────────────────────────────────────────────────────────────────────
-- ts is caller-supplied via ClockHandle. No SQL DEFAULT — a FakeClock-
-- backed test could otherwise produce rows timestamped with wallclock-now
-- while the reducer believes it's at FakeClock's time, silently diverging.

-- kind taxonomy (RFC-0012 §4): the seven values matched by
-- EventLogKind::as_db_str. CHECK enforces it at the DB layer the same
-- way host_rollout_records.state and rollouts.state do.

CREATE TABLE event_log (
    seq             INTEGER PRIMARY KEY AUTOINCREMENT,
    ts              TEXT NOT NULL,
    host_id         TEXT,
    rollout_id      TEXT,
    kind            TEXT NOT NULL
        CHECK (kind IN ('agent_event', 'plan_action', 'effect',
                        'gate_decision', 'verify_outcome',
                        'manifest_poll', 'rollout_event')),
    payload         TEXT NOT NULL
);

CREATE INDEX idx_event_log_host_ts
    ON event_log(host_id, ts)
    WHERE host_id IS NOT NULL;

CREATE INDEX idx_event_log_rollout
    ON event_log(rollout_id, seq)
    WHERE rollout_id IS NOT NULL;

CREATE INDEX idx_event_log_kind_ts
    ON event_log(kind, ts);

-- ─────────────────────────────────────────────────────────────────────
-- dispatch_queue (RFC-0008 §4.1 + plan 06 long-poll decision)
-- ─────────────────────────────────────────────────────────────────────
-- A row exists ⇔ a Dispatch is queued. After delivery the row is deleted;
-- if the agent never acks, the reducer re-emits via QueueDispatch on the
-- next plan tick (planner skips hosts with dispatch_acked_at already set,
-- so re-emission only happens when the prior Dispatch was lost).

CREATE TABLE dispatch_queue (
    hostname        TEXT NOT NULL,
    rollout_id      TEXT NOT NULL,
    target_closure  TEXT NOT NULL,
    soak_due_at     TEXT NOT NULL,
    enqueued_at     TEXT NOT NULL,
    PRIMARY KEY (hostname, rollout_id)
);

CREATE INDEX idx_dispatch_queue_hostname
    ON dispatch_queue(hostname);

-- ─────────────────────────────────────────────────────────────────────
-- probe_failures (RFC-0010 §7.2)
-- ─────────────────────────────────────────────────────────────────────
-- Typed denormalization of enforce-mode probe failures, derived from
-- event_log. Single writer (the applier's RemoteAppendEventLog handler,
-- 9b commit): on receipt of ProbeResult { mode = "enforce", status =
-- "Fail" }, write the event_log row AND the per-sub_result
-- probe_failures rows in one transaction. Non-evidence enforce-fail
-- probes produce one row with `control_id = NULL`. `event_log_seq` is
-- a FK back to event_log so the table is provably re-derivable from
-- canonical state.
--
-- The gate (planner_gates::compliance_wave) reads
-- `outstanding_failing_enforce_probes_by_rollout` to compute distinct-
-- control failure counts per (rollout, host). Indexed accordingly.
--
-- Phase 9a: schema only. The writer side lands in 9b — until then the
-- table stays empty and the gate is pass-through.

CREATE TABLE probe_failures (
    -- FK back to event_log proves re-derivability from canonical state.
    -- Nullable in 9b: today's event_log writer is fire-and-forget on a
    -- bounded mpsc, so the applier doesn't know the row's `seq` at
    -- co-write time. A follow-up tightens this (writer hands back the
    -- seq via oneshot) — when it lands the column becomes NOT NULL.
    event_log_seq   INTEGER
                    REFERENCES event_log(seq) ON DELETE CASCADE,
    rollout_id      TEXT NOT NULL,
    host_id         TEXT NOT NULL,
    probe_name      TEXT NOT NULL,
    control_id      TEXT,
    framework       TEXT,
    observed_at     TEXT NOT NULL
);

CREATE INDEX idx_probe_failures_rollout_host_control
    ON probe_failures(rollout_id, host_id, control_id);

CREATE INDEX idx_probe_failures_event_log_seq
    ON probe_failures(event_log_seq);
