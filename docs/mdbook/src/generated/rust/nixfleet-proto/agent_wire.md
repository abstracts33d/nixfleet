# `nixfleet_proto::agent_wire`

Agent ↔ control-plane wire types (RFC-0003 §4).

Defined in this crate (rather than in either binary) so the agent
and CP serialise/deserialise from one schema and Stream B can
reuse the same types for harness assertions. The Phase 3 expansion
adds `pendingGeneration`, `lastEvaluatedTarget`, `lastFetchOutcome`,
and `uptimeSecs` to the checkin body — all nullable, additive over
RFC-0003 §4.1's minimum.

Unknown-field posture follows the crate-level convention: serde's
default is to ignore unknowns; consumers MUST treat additions
within the same major version as backwards-compatible.

## Items

### 🔓 `const PROTOCOL_MAJOR_VERSION`

Protocol major version (RFC-0003 §6). Sent by the agent in
`X-Nixfleet-Protocol` on every `/v1/agent/*` request; CP checks
and rejects mismatched majors with 426 Upgrade Required.

v1 → v2 is a breaking change. Within a major, fields may be
added; agents and CP MUST ignore unknown fields.


### 🔓 `const PROTOCOL_VERSION_HEADER`

HTTP header carrying the agent's declared protocol major
version. Lowercase per HTTP/2 conventions (axum normalises
regardless).


### 🔓 `struct CheckinRequest`

POST /v1/agent/checkin request body. Sent by the agent every
`pollInterval` seconds; CP records into in-memory state.


### 🔓 `struct GenerationRef`

_(no doc comment)_


### 🔓 `struct PendingGeneration`

_(no doc comment)_


### 🔓 `struct EvaluatedTarget`

_(no doc comment)_


### 🔓 `struct FetchOutcome`

_(no doc comment)_


### 🔓 `enum FetchResult`

_(no doc comment)_


### 🔓 `struct CheckinResponse`

POST /v1/agent/checkin response. Phase 3 always returns
`target: null` (no rollouts dispatched until Phase 4).


### 🔓 `struct ConfirmRequest`

POST /v1/agent/confirm request body (Phase 4).

Agent posts this exactly once after a new generation has booted
and the agent process has come up healthy. CP records the
confirmation; the magic-rollback timer (separate task) checks
`pending_confirms.confirm_deadline` and transitions expired
records to `rolled-back` if no confirm arrived in the window.

Body shape per RFC-0003 §4.2 — minus probeResults (Phase 7).


### 🔓 `struct ConfirmResponse`

POST /v1/agent/confirm response.

204 No Content on acceptance — body is empty. RFC-0003 §4.2:
"204 on acceptance, 410 Gone if the rollout was cancelled or
the wave already failed (agent then triggers local rollback on
its own)." 410 is a status-code-only response; this struct
covers the rare success-with-body case (currently empty —
future Phase 4 PRs may add fields without a major bump).


### 🔓 `struct ReportRequest`

POST /v1/agent/report request body. Agent emits this when a
notable event happens out-of-band from the regular checkin
cadence — activation failure, realisation failure, post-switch
verify mismatch, enrollment / renewal failure, trust-file
problem.

Wire shape per RFC-0003 §4.3, with two operationally-useful
additions on top of the RFC's minimum:
- `agentVersion` for triage (CP can spot mismatched-rev agents).
- `occurredAt` so the operator can reason about timing without
  relying on CP-side receipt timestamp.

`event` is a discriminator string (kebab-case, see
[`ReportEvent`]). `details` holds per-event structured fields.
`rollout` correlates the event with a `pending_confirms` row
(matches `dispatch::Decision::Dispatch.rollout_id`); `null` for
events that aren't tied to a specific rollout (enrollment,
trust-error, …).

The earlier shipped shape (`kind` enum + free-form `error` +
`context: Value`) is retired here — `kind` was a closed enum
that needed proto bumps for new failure modes, `context: Value`
was opaque to operators, and there was no rollout linkage.


### 🔓 `enum ReportEvent`

Typed event variants. `event` is a kebab-case discriminator on
the wire; `details` carries the per-event structured body. New
failure modes add a variant — old agents/CPs see the variant
they don't recognise as `Other` if the consumer is permissive,
or surface a deserialise error for stricter callers.


### 🔓 `struct ReportResponse`

_(no doc comment)_


