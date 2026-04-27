# `nixfleet_agent::comms`

HTTP client wiring for talking to the control plane.

Builds an mTLS `reqwest::Client` from the operator-supplied PEM
paths. Provides typed `checkin` and `report` calls that round-
trip the wire types defined in `nixfleet_proto::agent_wire`.

## Items

### 🔒 `const CONNECT_TIMEOUT`

Connect timeout. Generous because lab is often on Tailscale and
the first connect after a sleep can be slow. The poll cadence
itself is 60s, so even ~10s connects don't compound badly.


### 🔒 `const REQUEST_TIMEOUT`

Per-request timeout (handshake + full request lifecycle).


### 🔓 `fn build_client`

Construct an mTLS-enabled HTTP client. CA cert pins the CP's
fleet CA; the client identity is the agent's per-host cert +
key. PR-1's TLS-only mode is supported (caller passes None for
`client_cert` and `client_key`); PR-3 onwards always wires both.


### 🔓 `fn checkin`

POST /v1/agent/checkin. Returns the typed response for the agent
to consume — Phase 3 always sees `target: None` and a 60s
`next_checkin_secs`.


### 🔓 `enum ConfirmOutcome`

Outcome of POST /v1/agent/confirm. Distinguishes the three
cases the activation loop needs to handle differently:
204 acknowledged, 410 cancelled (trigger local rollback per
RFC-0003 §4.2), other (deadline timer will sort it out).


### 🔓 `fn confirm`

POST /v1/agent/confirm. Called after a successful
`nixos-rebuild switch` to acknowledge the activation. Wire shape
per RFC-0003 §4.2.


### 🔓 `fn report`

POST /v1/agent/report. Used for out-of-band failure events
(verify-failed, fetch-failed, trust-error). Phase 3 doesn't have
a fetch path yet, but the function lands here so PR-4's poll
loop can call it directly.


