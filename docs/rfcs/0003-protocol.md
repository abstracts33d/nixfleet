# RFC-0003: Agent ↔ control-plane protocol

**Status.** Accepted.
**Depends on.** RFC-0001, RFC-0002, magic rollback.
**Scope.** Wire protocol between agent and control plane. Identity, endpoints, polling, versioning, security properties. Does not cover control-plane-internal APIs. The agent-state event flow is owned by RFC-0005 (§4.1 here points at it).

## 1. Design goals

1. **Pull-only for control flow.** Agents initiate every connection. Control plane never needs to reach an agent - works behind CGNAT, hotel WiFi, intermittent links.
2. **Stateless on the wire.** Each request is self-describing. No sessions, no long-lived connections, no WebSockets in v1.
3. **Declarative intent, not commands.** The control plane answers "what should host X be running?", never "run this command". Scripted execution is outside the agent's vocabulary on purpose.
4. **Zero-knowledge for secrets.** Secrets do not transit the control plane in plaintext. The protocol carries closure hashes and references, not secret material.
5. **Explicitly versioned.** Every request and response carries a protocol version. Mismatches fail loudly.

## 2. Identity model

- **Host key = SSH host ed25519 key.** Machine-lifetime key already present on every NixOS host (`/etc/ssh/ssh_host_ed25519_key`). Signs probe outputs (RFC-0002 §5.3), decrypts agenix secrets, anchors the agent's cryptographic identity. Not transmitted to the control plane; only its public half is declared in `fleet.nix`.
- **Agent identity = mTLS client certificate, derived from the host key.** At enrollment, the agent generates the CSR using the SSH host key as the signing key; the public key in the cert is the host's SSH public key. CN = `hostname`, SANs carry declared host attributes (channel, tags - redundant with fleet.resolved, used only for sanity checking). This binding means compromising the mTLS cert and compromising the host key are the same event; short-lived certs bound the exposure of that event.
- **Cert issuance.** Agent sends the CSR + a one-shot bootstrap token (signed by the org root key, scoped to `expectedHostname` + `expectedPubkeyFingerprint`). Control plane verifies both, issues cert with 30-day validity. A mismatch between the CSR's public key and the token's `expectedPubkeyFingerprint` aborts enrollment.
- **Cert rotation.** Agent requests renewal at 50% of remaining validity. Old cert valid until expiry; overlap prevents downtime.
- **Cert revocation.** Control plane maintains a small revocation set (hostname -> notBefore timestamp). Agents with certs issued before `notBefore` for their hostname are rejected. Simpler than CRLs; works because cert lifetime is short.
- **No shared credentials.** No API keys, no HMAC secrets, no bearer tokens. mTLS end to end.

## 3. Wire format

- **Transport.** HTTP/2 over TLS 1.3. mTLS mandatory.
- **Body.** JSON. Canonical field names, no nulls (absence means absence), timestamps RFC 3339 UTC.
- **Headers.**
  - `X-Nixfleet-Protocol: 1` - major version. Mismatched = 400.
  - `X-Nixfleet-Agent-Version: <semver>` - informational.
  - `Content-Type: application/json`.
- **Why not gRPC/protobuf?** Stability, debuggability, homelab introspection. Revisit if wire size becomes a problem (it won't at fleet sizes nixfleet targets).

## 4. Endpoints

All endpoints rooted at `https://<control-plane>/v1/`.

### 4.1 Agent-driven event flow

RFC-0005 supersedes the v0.1 `POST /agent/checkin` + `POST /agent/confirm` + `POST /agent/report` triple. The wire surface is now:

- `POST /v1/agent/events` — outbound event stream (DispatchAck, ActivationStarted, ActivationComplete/Failed/Deferred, ProbeTopologyDeclared, ProbeResult, ProbeFailureFirst, Failed, RollbackComplete, Converged). One event per POST; CP dedupes by `(hostname, rollout_id, seq)`. See RFC-0005 §4.2 for the event vocabulary.
- `POST /v1/agent/heartbeat` — liveness + drift-detection. Minimal payload (`current_closure`, `last_event_seq_by_rollout`); never advances state. See RFC-0005 §4.3.
- `GET /v1/agent/dispatch` — agent long-polls for queued `Dispatch` payloads (the only CP→agent message; rollback is agent-decided). Preserves the pull-only contract per §1 design goal. See RFC-0005 §4.1.

The agent verifies every `target_closure` against the signed manifest fetched via §4.4 before acting on it; no CP-advertised value is trusted directly. See RFC-0002 §4.4 for the threat model that contract closes.

### 4.2 `GET /agent/closure/<hash>`

Optional. If the host cannot reach the binary cache directly (restricted network), the control plane can proxy closures. Preference remains: agents fetch from cache, not control plane - this endpoint exists as a fallback, not a default path.

### 4.3 Enrollment endpoints

Out of scope for this RFC in detail. Summary:

- `POST /enroll` - accepts bootstrap token + CSR, returns signed cert. Token is burned on use.
- `POST /agent/renew` - accepts current cert (mTLS) + CSR, returns refreshed cert.
- `POST /agent/bootstrap-report` - pre-cert reporting path for failures that prevent normal cert provisioning.

#### Bootstrap-nonce allowlist (durable replay invariant)

The CP refuses any `/v1/enroll` whose token nonce is not present in the
signed `bootstrap-nonces.json` artifact (declared in `fleet.nix`, signed
by `ciReleaseKey`, polled on the same cadence as `revocations.json`).
This closes the replay-after-DB-wipe vector: even if `state.db` is wiped
(rebuild, incident, disk loss), the durable replay invariant lives in
the signed fleet repo, not in CP-local state.

The allowlist entry's `expiresAt` is authoritative - it may be tighter
than the token's own `claims.expires_at`, but never extends past it
(the token's own claim is checked separately). Operators can
declaratively narrow a still-unexpired token's validity window by
reducing this value or removing the entry, without rotating the token
itself.

`nixfleet-release` prunes entries with `expiresAt < signedAt` at sign
time so the signed artifact contains only the operational set;
fleet.nix retains historical entries as a curated audit log.

See `docs/operations/bootstrap-token-lifecycle.md` for the operator
runbook.

#### Bootstrap report

Agents that fail enrollment can't reach the mTLS-gated event endpoints (no cert yet). `POST /agent/bootstrap-report` exists for this case alone.

**Authentication.** Bound to a hostname + agent-supplied pubkey via the same bootstrap token used by `POST /enroll`. The token is NOT consumed — multiple bootstrap reports may fire while the operator iterates on the underlying issue. The token's lifetime gates the window.

**Allowlisted events.** Only `TrustError` and `EnrollmentFailed` events are accepted on this endpoint. Anything else is `400`. The allowlist enforces the path's narrow purpose: surfacing why enrollment is broken, not generic agent telemetry.

**Response.** `204 No Content` on accept; the CP records the event in `event_log` so the operator dashboard sees pre-cert failures in the same place as post-cert ones. Subsequent successful `/enroll` does not retroactively rewrite the bootstrap-report rows.

### 4.4 `GET /v1/rollouts/<rolloutId>`

Distributes the signed `RolloutManifest` (RFC-0002 §4.4) to agents. mTLS-gated like every other endpoint. The CP serves the on-disk pre-signed pair byte-for-byte; it does not re-derive, re-sign, or otherwise transform the manifest.

**Path parameter.** `rolloutId` is the canonical RFC-0008 §6.3 composite `"{channel}@{channel_ref}"` exactly as the CP advertised it in `/agent/checkin` responses. The CP route validator enforces `[a-z0-9_-]+@[0-9a-f]+` to block path-traversal smuggling.

**Response.** Two body shapes, served via the standard HTTP `Accept` content-negotiation pattern:

- `Accept: application/json` (default) returns the manifest JSON bytes.
- `Accept: application/octet-stream` returns the raw signature bytes (`<rolloutId>.sig`).

Agents fetch both. Implementations MAY also expose a single endpoint that returns both bundled (e.g. `application/json` with the signature in a sibling `X-Nixfleet-Signature` header); the wire-test harness asserts both shapes round-trip identically.

**Status codes.**

- `200 OK` - manifest found, body served.
- `404 Not Found` - `rolloutId` is unknown to the CP (never adopted, or evicted post-rollout-completion).
- `503 Service Unavailable` - CP recently rebuilt and has not yet reloaded the rollouts directory; agent retries after `nextCheckinSecs`.

**Idempotency + caching.** Manifests are immutable by content-address: a given `rolloutId` always returns the same bytes, or `404` if it never existed. Agents that have already cached a manifest do NOT need to re-fetch on every checkin - string equality against the cached `rolloutId` is sufficient. Defensive re-fetches (e.g. on agent restart) are safe but wasteful.

**No write side.** There is no `POST` or `PUT` on this endpoint. Manifests are produced by CI alone; the CP holds no signing key for rollouts. Operator workflows that need to "edit a rollout plan" require a new commit (which produces a new `rolloutId`).

## 5. Polling cadence

- **Default interval.** 60s, controlled server-side via `nextCheckinSecs` in the checkin response.
- **Backoff on error.** Exponential with jitter, capped at the channel's `reconcileIntervalMinutes`. Network errors do not drain the confirm window - `/confirm` retries aggressively (up to 5×) within the window to survive transient failures.
- **Load shaping.** Control plane can vary `nextCheckinSecs` per-host to smooth thundering herds after a push (e.g. assigning each host a slot within the polling window based on a hash of its hostname).
- **Idle hosts.** A host with no pending target polls at the channel's idle cadence (can be much longer - weekly for `edge-slow`).

## 6. Versioning

- **Protocol major version** in header. v1 -> v2 is a breaking change; running mixed versions is disallowed and fails at check-in with a clear message. Upgrade path: control plane supports N and N+1 simultaneously; operators upgrade agents, then retire control plane's N support.
- **Schema evolution within a major.** Fields may be added; agents and control plane MUST ignore unknown fields. Required fields never change meaning. Removing a field requires a major bump.
- **Agent version (informational).** Control plane refuses agents older than its declared minimum, emits events for newer agents (may indicate staged upgrade in progress).

## 7. Security model

**Defended against:**

- **Passive network observer.** TLS 1.3 - sees only traffic shape.
- **Active on-path attacker without a cert.** mTLS fails the handshake; no data exposed.
- **Compromised non-target agent.** Cert only authorizes its own hostname; cannot request targets for other hosts, cannot submit reports for other hosts. Control plane enforces `cert.CN == request.hostname` on every endpoint.
- **Compromised control plane - closure forgery.** Cannot learn secrets (zero-knowledge property). Can serve a different closure hash as target -> agent fetches from attic, verifies attic's ed25519 signature against the pinned attic public key (docs/design/architecture.md §4), refuses unsigned or foreign-signed closures.
- **Compromised control plane - stale-closure replay.** A compromised CP cannot forge closures but could point hosts at an older-but-still-validly-signed closure to block security fixes. Mitigation: every check-in response references a CI-signed `fleet.resolved` revision; the agent fetches that artifact (directly from cache or via the CP) and refuses any target whose backing `fleet.resolved.meta.signedAt` is older than `channel.freshnessWindow` (per-channel declaration in minutes, required, no default - RFC-0001 §2.3). The freshness window is itself inside the signed artifact, so a compromised CP cannot widen it.
- **Replay.** Confirm requests include `bootId`; the control plane rejects a confirm whose `bootId` doesn't match the expected new boot.

**Not defended against (explicit):**

- **Compromised host (root).** If the host's TLS key is stolen, the attacker can act as that host until the cert is revoked. Mitigated by short cert lifetime + TPM-backed keys (future issue).
- **Denial of service.** Out of scope for this RFC. Rate limiting, fail2ban-style protections, and similar are operational concerns.
- **Malicious control-plane operator.** Is explicitly a trusted role (can push any generation to any host). The security boundary is between the fleet and outsiders, not between operators and hosts.

## 8. Offline behavior

- **Agent caches the last check-in response** on disk. If the control plane is unreachable, the agent continues to operate at its current generation. It does not auto-revert, does not auto-upgrade.
- **Prolonged offline window.** If check-in fails for longer than `channel.offlineGraceSecs` (default: 7 days), the agent emits a local systemd journal warning but takes no action. Action is an operator decision.
- **Clock skew tolerance.** All deadlines (confirm window, cert validity) carry ≥ 60s slack to absorb typical host↔CP clock drift.

