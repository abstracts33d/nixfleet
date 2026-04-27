# `nixfleet_control_plane::server::handlers`

HTTP route handlers for the long-running CP server.

Pulled out of the monolithic `server.rs`. Each handler is its
own free function with the route's signature; the router in
`serve.rs` (this module's parent) wires them under the `/v1/*`
tree. State + middleware are shared via the parent's `state` and
`middleware` modules.

## Items

### 🔐 `fn whoami`

`GET /v1/whoami` — returns the verified mTLS CN of the caller.


### 🔐 `fn checkin`

`POST /v1/agent/checkin` — record an agent checkin.

Validates the body's `hostname` matches the verified mTLS CN
(sanity check, not a security boundary — the CN was already
authenticated by WebPkiClientVerifier; this just catches
configuration drift like a host using the wrong cert).

Emits a journal line per checkin so operators can grep
`journalctl -u nixfleet-control-plane | grep checkin`.


### 🔒 `fn dispatch_target_for_checkin`

Per-checkin dispatch decision. Reads the latest verified
`FleetResolved` from `AppState`, queries the DB for any pending
confirm row for this host (idempotency guard), and runs
`dispatch::decide_target`. On `Dispatch`, inserts a
`pending_confirms` row keyed on the deterministic rollout id and
returns the target. All other Decision variants resolve to None.

Failures here log + return None — a transient DB hiccup or
missing fleet snapshot must not surface as HTTP 500 to the
agent. The agent retries on its next checkin (60s).


### 🔐 `fn report`

`POST /v1/agent/report` — record an out-of-band event report.

In-memory ring buffer per host, capped at `REPORT_RING_CAP`.
New reports push to the back; oldest is dropped on overflow.
Phase 5 promotes this to SQLite + correlates with rollouts.


### 🔒 `fn rand_suffix`

8-char lowercase-alnum suffix for event IDs. Not crypto-grade —
just enough to make IDs visually distinct in journal output.


### 🔐 `fn enroll`

`POST /v1/enroll` — bootstrap a new fleet host.

No mTLS required (this is the path before the host has a cert).
Authentication is via the bootstrap-token signature against the
org root key in trust.json. Order of checks matches RFC-0003 §2:
1. Replay defense
2. Expiry
3. Signature against `orgRootKey.{current,previous}`
4. Hostname binding (claim ↔ CSR CN)
5. Pubkey-fingerprint binding (SHA-256 of CSR pubkey DER)


### 🔐 `fn renew`

`POST /v1/agent/renew` — issue a fresh cert for an authenticated
agent. mTLS-required; the verified CN is stamped onto the new
cert via `issuance::issue_cert`.


### 🔐 `fn confirm`

`POST /v1/agent/confirm` — agent confirms successful activation.
Marks the matching `pending_confirms` row as confirmed.

Behaviour:
- Pending row exists, deadline not passed → mark confirmed, 204.
- No matching row in 'pending' state → 410 Gone (covers both
  "wrong rollout_id" and "deadline expired / cancelled"; agent
  responds the same way: trigger local rollback per RFC §4.2).
- DB unset → 503 (endpoint requires persistence).


### 🔐 `fn closure_proxy`

`GET /v1/agent/closure/{hash}` — closure proxy fallback for hosts
that can't reach the binary cache directly. Forwards narinfo
requests to the configured attic upstream. Real Nix-cache-protocol
forwarding (full nar streaming) is a follow-up PR; this lands the
wire shape + the upstream config path.

When `closure_upstream` is unset, returns 501 Not Implemented.


