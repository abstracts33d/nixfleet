# `nixfleet_control_plane::server::middleware`

Cross-cutting auth + protocol middleware for the v1 router.

Two functions, both consumed by `serve.rs`'s router builder:

- [`require_cn`] — extract the verified mTLS CN from the request
  extensions, enforce cert revocation when the DB is configured.
  Every `/v1/*` handler that gates on identity calls this first.
- [`protocol_version_middleware`] — RFC-0003 §6 protocol-version
  header enforcement on `/v1/*`. Forward-compat: missing header
  accepted with debug log; present+mismatched returns 426.

## Items

### 🔐 `fn require_cn`

Extract the verified CN from `PeerCertificates`, or return 401.
Also enforces cert revocation when `AppState.db` is set: a cert
whose notBefore predates the host's revocation entry is rejected
with 401. Re-enrolled certs (notBefore > revoked_before) pass.

Centralised so each `/v1/*` handler reads identity the same way.


### 🔐 `fn protocol_version_middleware`

Middleware: enforce `X-Nixfleet-Protocol: <PROTOCOL_MAJOR_VERSION>`
on `/v1/*` requests (RFC-0003 §6).

Forward-compat posture: missing header → log debug + accept. This
lets older agents (Phase 3-deployed before this PR landed) keep
working during the transition. Header present + mismatched major
→ 426 Upgrade Required + log warn.

`/healthz` is not subject to this — it's the operator's status
probe and runs unauthenticated; protocol-versioning the health
check makes the operational debug story worse without buying
anything.


