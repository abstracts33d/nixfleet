# `nixfleet_control_plane::db`

SQLite persistence for the control plane (Phase 4 PR-1).

Skeleton ported from v0.1's `crates/control-plane/src/db.rs` (tag
v0.1.1) — same rusqlite + refinery stack, same WAL + FK posture.
v0.1's schema was for a different model (machines + generations
+ reports); Phase 4 starts fresh with a `migrations/` layout
committed to this PR. Subsequent Phase 4 PRs add migrations
additively.

Concurrency: a `Mutex<Connection>` guards a single SQLite
connection. SQLite scales fine for fleet sizes O(100) under WAL;
Phase 4 doesn't need a connection pool. Mutex poisoning is
converted to anyhow errors instead of panicking.

All schema-modifying operations go through `migrate()` which
refinery makes idempotent + version-tracked.

## Items

### 🔓 `struct Db`

SQLite-backed CP persistence.


### `impl Db`

- **`open`** — Open (or create) the SQLite database at `path`. Creates parent
directories as needed. Enables WAL + FK on the connection
before any migrations run.
- **`open_in_memory`** — Open a fresh in-memory database. Used by tests.
- **`migrate`** — Run all pending migrations. Idempotent under refinery —
previously-applied migrations are skipped.
- **`token_seen`** — True iff `nonce` was previously recorded.
- **`record_token_nonce`** — Record `nonce` as seen. No-op if the nonce already exists
(caller is expected to check `token_seen` first if it cares;
this is just `INSERT OR IGNORE`).
- **`prune_token_replay`** — Drop replay records older than `max_age` (typical: 24h, the
token validity window). Returns the number of pruned rows.
Phase 4 PR-2 wires this into a periodic background task.
- **`revoke_cert`** — Record a revocation: any cert for `hostname` with notBefore
older than `not_before` is rejected at mTLS time. Upsert
shape — revoking again moves the not_before forward.
- **`record_pending_confirm`** — Record a dispatched activation. Called from the dispatch loop
(Phase 4 follow-up) when CP populates `target` in a checkin
response. The agent will later post `/v1/agent/confirm` with
the same `rollout_id` once it boots the new closure.
- **`pending_confirm_exists`** — Returns true if the host has any `pending_confirms` row in
state `'pending'`. Used by the dispatch loop to avoid
re-dispatching while an activation is in flight (would create
a duplicate row racing the first).
- **`confirm_pending`** — Mark a pending confirmation as confirmed. Called by the
`/v1/agent/confirm` handler. Returns the number of rows
updated — 0 means no matching pending row (could be: rollout
cancelled, deadline already expired, or agent confirming
twice). Caller decides on the response code.
- **`pending_confirms_expired`** — Pending confirms whose deadline has passed and which haven't
been confirmed yet. Used by the magic-rollback timer task —
each row returned is a host that failed to confirm in time
and should be rolled back.

Returns (id, hostname, rollout_id, wave, target_closure_hash).

Wraps `confirm_deadline` in `datetime(...)` so SQLite parses the
stored RFC3339 string (`YYYY-MM-DDTHH:MM:SS+00:00`, written by
`chrono::DateTime::to_rfc3339`) into the same canonical
`YYYY-MM-DD HH:MM:SS` shape that `datetime('now')` returns,
before the `<` comparison. Naked string compare would put `T`
(0x54) above ` ` (0x20) at position 10, so deadlines would
always look greater than now — expired rows never matched and
the rollback timer was a no-op. Caught on lab during the first
real Phase 4 dispatch (deadline passed by 50s, row still
`pending`).
- **`mark_rolled_back`** — Mark expired confirms as rolled-back. Called by the magic-
rollback timer after `pending_confirms_expired` for the same
IDs. Idempotent — only updates rows still in 'pending' state,
so a second call with the same IDs is a no-op.
- **`cert_revoked_before`** — Return the most recent revocation `not_before` for `hostname`,
or `None` if not revoked. Caller compares against the
presented cert's notBefore at mTLS handshake time.

