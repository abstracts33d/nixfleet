# `nixfleet_agent`

`nixfleet-agent` — Phase 3 PR-3 poll loop.

Real main loop. Reads cert paths + CP URL from CLI flags, builds
an mTLS reqwest client, polls `/v1/agent/checkin` every
`pollInterval` seconds with a richer body than RFC-0003 §4.1's
minimum (pending generation, last-fetch outcome, agent uptime).
No activation — the response's `target` is logged but never
acted on (Phase 4 wires that).

## Items

### 🔒 `fn post_report`

Build + POST a `/v1/agent/report` event to the CP. Best-effort:
telemetry MUST NOT crash the activation loop, so any HTTP / TLS
/ serde failure is logged at warn and swallowed. The event is
already in the local journal (the caller logs first); the report
is purely for the operator's CP-side view.


